//! Host engine: owns the physical keyboard/mouse, routes input to clients.
//!
//! Threads:
//! - OS capture thread (platform-specific run loop / message pump)
//! - tokio runtime: accept loop, one reader+writer task pair per client
//!   session, and the router below.
//!
//! Focus model: `None` = input stays local (capture passes events through at
//! the OS layer, nothing crosses the network). `Some(peer)` = capture
//! swallows everything and the router relays it to that peer's session.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use drift_core::config::Config;
use drift_core::layout::Edge;
use drift_core::proto::{InputEvent, Intro, Msg, MouseButton, PROTOCOL_VERSION};
use drift_core::secure;
use drift_core::wire::read_frame;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, info, warn};

use crate::engine::Captured;
use crate::platform::{self, CaptureCtl};

const PING_INTERVAL: Duration = Duration::from_secs(5);
const SESSION_TIMEOUT: Duration = Duration::from_secs(15);
const RETURN_COOLDOWN: Duration = Duration::from_millis(300);
const EDGE_INSET: i32 = 2;

type Sessions = Arc<Mutex<HashMap<String, UnboundedSender<Msg>>>>;

enum SessionEvent {
    Connected { name: String },
    Disconnected { name: String },
    CursorLeft { name: String, edge: Edge, ratio: f32 },
}

pub fn run(cfg: Config) -> Result<()> {
    let bounds = platform::desktop_bounds();
    info!(name = %cfg.name, ?bounds, "starting drift host");

    let ctl = Arc::new(CaptureCtl::new(bounds));
    let (cap_tx, cap_rx) = mpsc::unbounded_channel();
    platform::start_capture(ctl.clone(), cap_tx).context("input capture failed to start")?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(host_main(cfg, ctl, cap_rx))
}

async fn host_main(cfg: Config, ctl: Arc<CaptureCtl>, mut cap_rx: UnboundedReceiver<Captured>) -> Result<()> {
    let cfg = Arc::new(cfg);
    let listener = TcpListener::bind(("0.0.0.0", cfg.port))
        .await
        .with_context(|| format!("cannot listen on port {}", cfg.port))?;
    // Keep the daemon alive for the lifetime of the host: dropping it would
    // withdraw the mDNS advertisement.
    let _mdns = drift_core::discovery::advertise(&cfg.name, cfg.port)
        .map_err(|e| warn!("mDNS advertisement failed (static addrs still work): {e}"))
        .ok();

    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();

    tokio::spawn(accept_loop(listener, cfg.clone(), sessions.clone(), evt_tx));

    let mut router = Router {
        cfg: cfg.clone(),
        ctl,
        sessions,
        focus: None,
        down_keys: HashSet::new(),
        down_buttons: HashSet::new(),
    };

    info!("host ready — move the cursor against a portal edge to cross over");
    loop {
        tokio::select! {
            cap = cap_rx.recv() => match cap {
                Some(ev) => router.on_captured(ev, &mut cap_rx),
                None => break,
            },
            evt = evt_rx.recv() => match evt {
                Some(ev) => router.on_session_event(ev),
                None => break,
            },
        }
    }
    Ok(())
}

struct Router {
    cfg: Arc<Config>,
    ctl: Arc<CaptureCtl>,
    sessions: Sessions,
    focus: Option<String>,
    down_keys: HashSet<u16>,
    down_buttons: HashSet<MouseButton>,
}

impl Router {
    fn send_to_focus(&self, msg: Msg) {
        if let Some(name) = &self.focus {
            let dead = {
                let sessions = self.sessions.lock().unwrap();
                match sessions.get(name) {
                    Some(tx) => tx.send(msg).is_err(),
                    None => true,
                }
            };
            if dead {
                debug!("focused session {name} gone");
            }
        }
    }

    fn on_captured(&mut self, ev: Captured, cap_rx: &mut UnboundedReceiver<Captured>) {
        match ev {
            Captured::Input(InputEvent::MouseMove { mut dx, mut dy }) => {
                // Coalesce a burst of queued moves into one event so a slow
                // network hiccup never builds a backlog of stale motion.
                let mut trailing = None;
                while let Ok(next) = cap_rx.try_recv() {
                    if let Captured::Input(InputEvent::MouseMove { dx: x, dy: y }) = next {
                        dx += x;
                        dy += y;
                    } else {
                        trailing = Some(next);
                        break;
                    }
                }
                self.send_to_focus(Msg::Input(InputEvent::MouseMove { dx, dy }));
                if let Some(next) = trailing {
                    self.on_captured(next, cap_rx);
                }
            }
            Captured::Input(ev) => {
                match ev {
                    InputEvent::Key { key, pressed } => {
                        if pressed { self.down_keys.insert(key); } else { self.down_keys.remove(&key); }
                    }
                    InputEvent::MouseButton { button, pressed } => {
                        if pressed { self.down_buttons.insert(button); } else { self.down_buttons.remove(&button); }
                    }
                    _ => {}
                }
                self.send_to_focus(Msg::Input(ev));
            }
            Captured::EdgeHit { edge, ratio } => {
                match self.cfg.layout.target(&self.cfg.name, edge) {
                    Some((peer, entry_edge)) if self.session_exists(peer) => {
                        info!("cursor -> {peer} (via {edge} edge)");
                        self.focus = Some(peer.to_string());
                        self.send_to_focus(Msg::Enter { edge: entry_edge, ratio });
                    }
                    _ => {
                        // Race: peer vanished between the portal check and now.
                        self.return_local_at(edge, ratio);
                    }
                }
            }
            Captured::Panic => {
                info!("panic escape — input returned to host");
                self.release_all();
                self.send_to_focus(Msg::Leave);
                self.focus = None;
                let b = self.ctl.bounds;
                platform::warp_cursor(b.x + b.w / 2, b.y + b.h / 2);
            }
        }
    }

    fn on_session_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Connected { name } => {
                info!("client connected: {name}");
                self.refresh_portals();
            }
            SessionEvent::Disconnected { name } => {
                info!("client disconnected: {name}");
                if self.focus.as_deref() == Some(name.as_str()) {
                    // Never leave the user with no cursor: pull input home.
                    self.focus = None;
                    self.down_keys.clear();
                    self.down_buttons.clear();
                    self.exit_forwarding();
                }
                self.refresh_portals();
            }
            SessionEvent::CursorLeft { name, edge, ratio } => {
                if self.focus.as_deref() != Some(name.as_str()) {
                    return; // stale report from a peer that lost focus already
                }
                match self.cfg.layout.target(&name, edge) {
                    Some((next, entry_edge)) if next == self.cfg.name => {
                        self.release_all();
                        self.send_to_focus(Msg::Leave);
                        self.focus = None;
                        self.return_local_at(entry_edge, ratio);
                        info!("cursor -> {} (home)", self.cfg.name);
                    }
                    Some((next, entry_edge)) if self.session_exists(next) => {
                        let next = next.to_string();
                        self.release_all();
                        self.send_to_focus(Msg::Leave);
                        info!("cursor -> {next}");
                        self.focus = Some(next);
                        self.send_to_focus(Msg::Enter { edge: entry_edge, ratio });
                    }
                    _ => {
                        // Leads nowhere (or target offline): come home.
                        self.release_all();
                        self.send_to_focus(Msg::Leave);
                        self.focus = None;
                        self.exit_forwarding();
                    }
                }
            }
        }
    }

    fn session_exists(&self, name: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(name)
    }

    /// Send key/button releases to the currently focused peer so nothing
    /// stays stuck down when focus moves away.
    fn release_all(&mut self) {
        let keys: Vec<u16> = self.down_keys.drain().collect();
        for key in keys {
            self.send_to_focus(Msg::Input(InputEvent::Key { key, pressed: false }));
        }
        let buttons: Vec<MouseButton> = self.down_buttons.drain().collect();
        for button in buttons {
            self.send_to_focus(Msg::Input(InputEvent::MouseButton { button, pressed: false }));
        }
    }

    fn return_local_at(&self, entry_edge: Edge, ratio: f32) {
        let (x, y) = drift_core::layout::point_on_edge(self.ctl.bounds, entry_edge, ratio, EDGE_INSET);
        self.exit_forwarding();
        platform::warp_cursor(x, y);
    }

    fn exit_forwarding(&self) {
        *self.ctl.cooldown_until.lock().unwrap() = Instant::now() + RETURN_COOLDOWN;
        self.ctl.forwarding.store(false, Ordering::SeqCst);
        platform::set_forwarding_visuals(false);
    }

    /// Portal edges are only armed when the machine behind them is online.
    fn refresh_portals(&self) {
        let mut active = Vec::new();
        for edge in self.cfg.layout.portals(&self.cfg.name) {
            if let Some((peer, _)) = self.cfg.layout.target(&self.cfg.name, edge) {
                if self.session_exists(peer) {
                    active.push(edge);
                }
            }
        }
        *self.ctl.portals.write().unwrap() = active;
    }
}

async fn accept_loop(listener: TcpListener, cfg: Arc<Config>, sessions: Sessions, evt_tx: UnboundedSender<SessionEvent>) {
    loop {
        let Ok((stream, addr)) = listener.accept().await else { return };
        let cfg = cfg.clone();
        let sessions = sessions.clone();
        let evt_tx = evt_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, cfg, sessions, evt_tx).await {
                debug!("connection from {addr}: {e}");
            }
        });
    }
}

async fn handle_conn(mut stream: TcpStream, cfg: Arc<Config>, sessions: Sessions, evt_tx: UnboundedSender<SessionEvent>) -> Result<()> {
    stream.set_nodelay(true)?;
    let intro = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut stream)).await??;
    let name = match Intro::decode(&intro)? {
        Intro::Session { name } => name,
        Intro::Pair => {
            // Pairing is only served by the dedicated `drift pair` command:
            // a running host must never silently accept new devices.
            anyhow::bail!("pair attempt while running; run `drift pair` instead");
        }
    };
    let peer = cfg.peer(&name).with_context(|| format!("unknown peer '{name}'"))?;
    let psk = peer.psk_bytes()?;

    let (mut reader, mut writer) = secure::handshake_responder(stream, &psk).await?;

    let hello = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await??;
    let (client_screen, os) = match hello {
        Msg::Hello { version, screen, os, .. } => {
            anyhow::ensure!(version == PROTOCOL_VERSION, "protocol version mismatch: {version} != {PROTOCOL_VERSION}");
            (screen, os)
        }
        other => anyhow::bail!("expected Hello, got {other:?}"),
    };
    debug!(?client_screen, %os, "client hello");

    writer
        .send(&Msg::Welcome {
            version: PROTOCOL_VERSION,
            name: cfg.name.clone(),
            portal_edges: cfg.layout.portals(&name),
        })
        .await?;

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Msg>();
    sessions.lock().unwrap().insert(name.clone(), out_tx);
    let _ = evt_tx.send(SessionEvent::Connected { name: name.clone() });

    // Writer task: relays router messages and keeps the link warm with pings.
    let writer_task = tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_INTERVAL);
        let mut seq = 0u64;
        loop {
            tokio::select! {
                msg = out_rx.recv() => match msg {
                    Some(m) => { if writer.send(&m).await.is_err() { return; } }
                    None => { let _ = writer.send(&Msg::Bye).await; return; }
                },
                _ = ping.tick() => {
                    seq += 1;
                    if writer.send(&Msg::Ping(seq)).await.is_err() { return; }
                }
            }
        }
    });

    // Reader loop with a liveness watchdog.
    let result: Result<()> = async {
        loop {
            let msg = tokio::time::timeout(SESSION_TIMEOUT, reader.recv()).await??;
            match msg {
                Msg::CursorLeft { edge, ratio } => {
                    let _ = evt_tx.send(SessionEvent::CursorLeft { name: name.clone(), edge, ratio });
                }
                Msg::Pong(_) => {}
                Msg::Bye => return Ok(()),
                other => debug!("unexpected from {name}: {other:?}"),
            }
        }
    }
    .await;

    sessions.lock().unwrap().remove(&name);
    let _ = evt_tx.send(SessionEvent::Disconnected { name });
    writer_task.abort();
    result
}
