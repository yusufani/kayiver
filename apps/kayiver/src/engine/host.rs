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
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use kayiver_core::config::{Config, SharedMonitor};
use kayiver_core::layout::{Edge, Layout};
use kayiver_core::proto::{InputEvent, Intro, Msg, MouseButton, PROTOCOL_VERSION};
use kayiver_core::secure;
use kayiver_core::wire::read_frame;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, info, warn};

use crate::engine::Captured;
use crate::platform::{self, CaptureCtl};

const PING_INTERVAL: Duration = Duration::from_secs(1);
const SESSION_TIMEOUT: Duration = Duration::from_secs(15);
const RETURN_COOLDOWN: Duration = Duration::from_millis(300);
const EDGE_INSET: i32 = 2;

type Sessions = Arc<Mutex<HashMap<String, UnboundedSender<Msg>>>>;
/// Layout is shared (and hot-reloaded) so `kayiver ui` edits apply live.
type SharedLayout = Arc<RwLock<Layout>>;
/// Shared-monitor config, hot-reloaded together with the layout.
type SharedCfg = Arc<RwLock<SharedMonitor>>;
/// Live in-memory copy of each peer's monitor shapes, keyed by peer name.
/// Updated when a peer reports geometry; read on the cursor hot path so a
/// shared-edge crossing never has to touch the disk.
type PeerScreens = Arc<RwLock<HashMap<String, Vec<kayiver_core::proto::Rect>>>>;

enum SessionEvent {
    Connected { name: String },
    Disconnected { name: String },
    CursorLeft { name: String, edge: Edge, ratio: f32 },
    /// The peer's cursor moved onto the shared panel (showing this host), at
    /// relative position (fx, fy) — take control back onto our copy of it.
    SharedCross { name: String, fx: f32, fy: f32 },
    LayoutChanged,
}

pub fn run(cfg: Config) -> Result<()> {
    let bounds = platform::desktop_bounds();
    info!(name = %cfg.name, ?bounds, "starting kayiver host");

    let ctl = Arc::new(CaptureCtl::new(bounds));
    let (cap_tx, cap_rx) = mpsc::unbounded_channel();
    platform::start_capture(ctl.clone(), cap_tx.clone()).context("input capture failed to start")?;
    // Hands control to the peer when the cursor moves onto a shared monitor
    // that's showing it (via Captured::SharedEnter through the same channel).
    platform::start_cursor_guard(ctl.clone(), cap_tx);

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(host_main(cfg, ctl, cap_rx))
}

async fn host_main(cfg: Config, ctl: Arc<CaptureCtl>, mut cap_rx: UnboundedReceiver<Captured>) -> Result<()> {
    let cfg = Arc::new(cfg);
    let listener = TcpListener::bind(("0.0.0.0", cfg.port))
        .await
        .with_context(|| {
            format!(
                "port {} is busy — kayiver is probably already running (check with: pgrep -fl kayiver)",
                cfg.port
            )
        })?;
    // Keep the daemon alive for the lifetime of the host: dropping it would
    // withdraw the mDNS advertisement.
    let _mdns = kayiver_core::discovery::advertise(&cfg.name, cfg.port)
        .map_err(|e| warn!("mDNS advertisement failed (static addrs still work): {e}"))
        .ok();

    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let layout: SharedLayout = Arc::new(RwLock::new(cfg.layout.clone()));
    let shared: SharedCfg = Arc::new(RwLock::new(cfg.shared_monitor.clone()));
    // Seed the live peer-screen cache from whatever the config last recorded,
    // so a crossing works even before the peer sends a fresh geometry update.
    let peer_screens: PeerScreens = Arc::new(RwLock::new(
        cfg.peers.iter().map(|p| (p.name.clone(), p.screens.clone())).collect(),
    ));
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();

    tokio::spawn(accept_loop(listener, cfg.clone(), layout.clone(), sessions.clone(), peer_screens.clone(), evt_tx.clone()));
    tokio::spawn(watch_layout(layout.clone(), shared.clone(), ctl.clone(), evt_tx));

    // Shared-monitor state: arm the hotkey. On start the host owns the panel
    // (its cursor is free; the peer is told to block its shared rect).
    let shared_peer = shared_peer_name(&cfg, &cfg.shared_monitor);
    let shared_owner = cfg.name.clone();
    if cfg.shared_monitor.configured() {
        ctl.shared_hotkey.store(cfg.shared_monitor.hotkey, Ordering::SeqCst);
    }
    crate::ui::set_shared_state(
        cfg.shared_monitor.configured(),
        shared_peer.clone(),
        Some(shared_owner.clone()),
    );

    let mut router = Router {
        cfg: cfg.clone(),
        layout,
        shared,
        peer_screens,
        shared_owner,
        ctl,
        sessions,
        focus: None,
        down_keys: HashSet::new(),
        down_buttons: HashSet::new(),
    };

    // The layout editor rides along with the host process.
    crate::ui::mark_running();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    crate::ui::set_cmd_sender(cmd_tx);
    tokio::spawn(async {
        if let Err(e) = crate::ui::serve_forever().await {
            debug!("ui server not started: {e:#}");
        }
    });
    info!("layout editor: {}", crate::ui::url());
    info!("host ready — move the cursor against a portal edge to cross over");
    loop {
        let prev_focus = router.focus.clone();
        tokio::select! {
            cap = cap_rx.recv() => match cap {
                Some(ev) => router.on_captured(ev, &mut cap_rx),
                None => break,
            },
            evt = evt_rx.recv() => match evt {
                Some(ev) => router.on_session_event(ev),
                None => break,
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(crate::ui::UiCmd::SetSharedOwner(owner)) => router.set_shared_owner(&owner),
                None => break,
            },
        }
        if router.focus != prev_focus {
            crate::ui::set_focus(router.focus.clone());
        }
    }
    Ok(())
}

/// The peer that shares the panel: explicit config, else the first paired peer.
fn shared_peer_name(cfg: &Config, sm: &SharedMonitor) -> Option<String> {
    sm.peer.clone().or_else(|| cfg.peers.first().map(|p| p.name.clone()))
}

struct Router {
    cfg: Arc<Config>,
    layout: SharedLayout,
    shared: SharedCfg,
    peer_screens: PeerScreens,
    /// Which machine the shared panel is currently showing (best knowledge).
    shared_owner: String,
    ctl: Arc<CaptureCtl>,
    sessions: Sessions,
    focus: Option<String>,
    down_keys: HashSet<u16>,
    down_buttons: HashSet<MouseButton>,
}

impl Router {
    fn layout_target(&self, machine: &str, edge: Edge) -> Option<(String, Edge)> {
        let layout = self.layout.read().unwrap();
        layout.target(machine, edge).map(|(n, e)| (n.to_string(), e))
    }

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
                // Shared-panel edge → the peer's monitor beyond it (e.g. a
                // Windows-only screen physically above the shared panel). Takes
                // precedence over the machine-level layout link.
                if self.try_shared_edge_cross(edge, ratio) {
                    return;
                }
                match self.layout_target(&self.cfg.name, edge) {
                    Some((peer, entry_edge)) if self.session_exists(&peer) => {
                        info!("cursor -> {peer} (via {edge} edge)");
                        crate::ui::set_cross_flash(edge);
                        self.focus = Some(peer);
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
            Captured::SharedHotkey => self.set_shared_owner("toggle"),
            Captured::SharedEnter { fx, fy } => {
                // Local cursor moved onto the shared panel (showing the peer) →
                // hand control to the peer, onto its copy of the panel.
                let sm = self.shared.read().unwrap().clone();
                let peer = shared_peer_name(&self.cfg, &sm);
                if let (Some(peer), Some(pr)) = (peer, sm.peer_rect) {
                    if self.session_exists(&peer) {
                        let x = pr.x + (fx * pr.w as f32) as i32;
                        let y = pr.y + (fy * pr.h as f32) as i32;
                        self.ctl.forwarding.store(true, Ordering::SeqCst);
                        platform::set_forwarding_visuals(true);
                        self.focus = Some(peer);
                        self.send_to_focus(Msg::EnterAt { x, y });
                        info!("cursor -> peer (onto shared panel)");
                    }
                }
            }
        }
    }

    /// Flip which machine the shared panel is "showing": attach the display on
    /// the new owner, detach it on the other side. `owner` is a machine name
    /// or "toggle". Mirrors the physical input switch the user just pressed
    /// (or is about to press) on the monitor itself.
    fn set_shared_owner(&mut self, owner: &str) {
        let sm = self.shared.read().unwrap().clone();
        if !sm.configured() {
            warn!("shared monitor not configured (set shared_monitor in config or via the editor)");
            return;
        }
        let Some(peer) = shared_peer_name(&self.cfg, &sm) else {
            warn!("shared monitor: no peer configured/paired");
            return;
        };
        let owner = match owner {
            "toggle" => {
                if self.shared_owner == self.cfg.name { peer.clone() } else { self.cfg.name.clone() }
            }
            o if o == self.cfg.name || o == peer => o.to_string(),
            o => {
                warn!("shared monitor: unknown machine '{o}'");
                return;
            }
        };
        let to_me = owner == self.cfg.name;
        info!("shared monitor -> {owner}");
        self.shared_owner = owner.clone();
        crate::ui::set_shared_owner(Some(owner));
        crate::ui::set_shared_error(None);

        // Cursor-skip model (no display is ever touched): the machine that is
        // NOT being shown blocks its shared rect so the cursor skips over it.
        // Local (host): block local_rect unless the host owns the panel.
        *self.ctl.blocked.write().unwrap() = if to_me { None } else { sm.local_rect };

        // Peer: block its rect when the host owns the panel; clear when it does.
        let block = if to_me { sm.peer_rect } else { None };
        let msg = Msg::SharedBlock { rect: block };
        let sent = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(&peer).map(|tx| tx.send(msg).is_ok()).unwrap_or(false)
        };
        if !sent {
            warn!("shared monitor: peer '{peer}' offline");
            crate::ui::set_shared_error(Some(format!("{peer} offline")));
        }
    }

    fn on_session_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Connected { name } => {
                info!("client connected: {name}");
                self.refresh_portals();
                // Re-establish the shared-monitor block on the (re)connected
                // peer to match the current owner.
                if self.shared.read().unwrap().configured() {
                    let owner = self.shared_owner.clone();
                    self.set_shared_owner(&owner);
                }
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
            SessionEvent::LayoutChanged => self.refresh_portals(),
            SessionEvent::SharedCross { name, fx, fy } => {
                if self.focus.as_deref() != Some(name.as_str()) {
                    return; // stale
                }
                let rect = self.shared.read().unwrap().local_rect;
                if let Some(r) = rect {
                    self.release_all();
                    self.send_to_focus(Msg::Leave);
                    self.focus = None;
                    self.exit_forwarding();
                    let x = r.x + (fx * r.w as f32) as i32;
                    let y = r.y + (fy * r.h as f32) as i32;
                    platform::warp_cursor(x, y);
                    info!("cursor -> {} (onto shared panel)", self.cfg.name);
                }
            }
            SessionEvent::CursorLeft { name, edge, ratio } => {
                if self.focus.as_deref() != Some(name.as_str()) {
                    return; // stale report from a peer that lost focus already
                }
                match self.layout_target(&name, edge) {
                    Some((next, entry_edge)) if next == self.cfg.name => {
                        self.release_all();
                        self.send_to_focus(Msg::Leave);
                        self.focus = None;
                        self.return_local_at(entry_edge, ratio);
                        info!("cursor -> {} (home)", self.cfg.name);
                    }
                    Some((next, entry_edge)) if self.session_exists(&next) => {
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

    /// If the cursor left through the shared panel's `edge`, and the peer has a
    /// monitor touching ITS copy of the panel on that same side, cross there —
    /// so "up from the shared panel" reaches the peer's screen that physically
    /// sits above it. Returns true if it handled the crossing.
    fn try_shared_edge_cross(&mut self, edge: Edge, ratio: f32) -> bool {
        let sm = self.shared.read().unwrap().clone();
        let (Some(local), Some(prect)) = (sm.local_rect, sm.peer_rect) else { return false };
        let Some(peer) = shared_peer_name(&self.cfg, &sm) else { return false };
        if !self.session_exists(&peer) {
            return false;
        }
        // Where did the cursor leave the host desktop? Must be on `local`'s edge.
        let b = self.ctl.bounds;
        let (ex, ey) = kayiver_core::layout::point_on_edge(b, edge, ratio, 0);
        let on_local_edge = match edge {
            Edge::Top => ey <= local.y && ex >= local.x && ex < local.right(),
            Edge::Bottom => ey >= local.bottom() - 1 && ex >= local.x && ex < local.right(),
            Edge::Left => ex <= local.x && ey >= local.y && ey < local.bottom(),
            Edge::Right => ex >= local.right() - 1 && ey >= local.y && ey < local.bottom(),
        };
        if !on_local_edge {
            return false;
        }
        // Relative position along the panel edge (0..1).
        let f = match edge {
            Edge::Top | Edge::Bottom => ((ex - local.x) as f32 / local.w.max(1) as f32).clamp(0.0, 1.0),
            Edge::Left | Edge::Right => ((ey - local.y) as f32 / local.h.max(1) as f32).clamp(0.0, 1.0),
        };
        // Find the peer monitor adjacent to peer_rect on the same side. Read
        // from the live in-memory cache — never the disk — so this stays on the
        // fast path while the cursor is pressed against the shared edge.
        let screens = self.peer_screens.read().unwrap().get(&peer).cloned().unwrap_or_default();
        let adj = screens.into_iter().find(|m| match edge {
            Edge::Top => (m.bottom() - prect.y).abs() <= 8 && m.x < prect.right() && m.right() > prect.x,
            Edge::Bottom => (m.y - prect.bottom()).abs() <= 8 && m.x < prect.right() && m.right() > prect.x,
            Edge::Left => (m.right() - prect.x).abs() <= 8 && m.y < prect.bottom() && m.bottom() > prect.y,
            Edge::Right => (m.x - prect.right()).abs() <= 8 && m.y < prect.bottom() && m.bottom() > prect.y,
        });
        let (x, y, how) = match adj {
            // A peer monitor sits beyond the panel on this side (e.g. C above
            // B): land near its shared edge, at the aligned offset.
            Some(m) => {
                let p = match edge {
                    Edge::Top => (m.x + (f * m.w as f32) as i32, m.bottom() - 2),
                    Edge::Bottom => (m.x + (f * m.w as f32) as i32, m.y + 2),
                    Edge::Left => (m.right() - 2, m.y + (f * m.h as f32) as i32),
                    Edge::Right => (m.x + 2, m.y + (f * m.h as f32) as i32),
                };
                (p.0, p.1, "adjacent monitor")
            }
            // No monitor beyond the panel, but the machine-level layout routes
            // this edge to the shared peer (e.g. B's outer right edge → the
            // Windows box): hand the panel over. Land on the peer's OWN copy of
            // the panel at the same relative position, entering from the
            // opposite edge — never the desktop-wide ratio, which would drift
            // onto a taller/wider peer's other monitors.
            None => {
                let routes_to_peer = self
                    .layout_target(&self.cfg.name, edge)
                    .is_some_and(|(target, _)| target == peer);
                if !routes_to_peer {
                    return false;
                }
                let p = match edge {
                    Edge::Right => (prect.x + 2, prect.y + (f * prect.h as f32) as i32),
                    Edge::Left => (prect.right() - 2, prect.y + (f * prect.h as f32) as i32),
                    Edge::Bottom => (prect.x + (f * prect.w as f32) as i32, prect.y + 2),
                    Edge::Top => (prect.x + (f * prect.w as f32) as i32, prect.bottom() - 2),
                };
                (p.0, p.1, "shared panel handover")
            }
        };
        info!("cursor -> {peer} (shared panel {edge} edge -> {how})");
        crate::ui::set_cross_flash(edge);
        self.focus = Some(peer);
        self.send_to_focus(Msg::EnterAt { x, y });
        true
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
        let (x, y) = kayiver_core::layout::point_on_edge(self.ctl.bounds, entry_edge, ratio, EDGE_INSET);
        self.exit_forwarding();
        platform::warp_cursor(x, y);
        crate::ui::set_cross_flash(entry_edge); // cursor arrived back on this machine
    }

    fn exit_forwarding(&self) {
        *self.ctl.cooldown_until.lock().unwrap() = Instant::now() + RETURN_COOLDOWN;
        self.ctl.forwarding.store(false, Ordering::SeqCst);
        platform::set_forwarding_visuals(false);
    }

    /// Portal edges are only armed when the machine behind them is online.
    fn refresh_portals(&self) {
        let mut active = Vec::new();
        {
            let layout = self.layout.read().unwrap();
            for edge in layout.portals(&self.cfg.name) {
                if let Some((peer, _)) = layout.target(&self.cfg.name, edge) {
                    if self.sessions.lock().unwrap().contains_key(peer) {
                        active.push(edge);
                    }
                }
            }
        }
        *self.ctl.portals.write().unwrap() = active;
    }
}

/// Re-read the config every 2 s; on change, swap the shared layout /
/// shared-monitor settings and nudge the router. This is what makes
/// `kayiver ui` (or hand-editing config.toml) apply without restarting.
async fn watch_layout(
    layout: SharedLayout,
    shared: SharedCfg,
    ctl: Arc<CaptureCtl>,
    evt_tx: UnboundedSender<SessionEvent>,
) {
    let path = Config::path();
    let mtime = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut last = mtime(&path);
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let cur = mtime(&path);
        if cur == last {
            continue;
        }
        last = cur;
        match Config::load_or_init() {
            Ok(new_cfg) => {
                let changed = {
                    let mut l = layout.write().unwrap();
                    if *l != new_cfg.layout {
                        *l = new_cfg.layout.clone();
                        true
                    } else {
                        false
                    }
                };
                if changed {
                    info!("layout reloaded from config");
                    let _ = evt_tx.send(SessionEvent::LayoutChanged);
                }
                let sm_changed = {
                    let mut s = shared.write().unwrap();
                    if *s != new_cfg.shared_monitor {
                        *s = new_cfg.shared_monitor.clone();
                        true
                    } else {
                        false
                    }
                };
                if sm_changed {
                    info!("shared-monitor settings reloaded from config");
                    let sm = new_cfg.shared_monitor.clone();
                    ctl.shared_hotkey.store(sm.configured() && sm.hotkey, Ordering::SeqCst);
                    crate::ui::set_shared_state(
                        sm.configured(),
                        shared_peer_name(&new_cfg, &sm),
                        None,
                    );
                }
            }
            Err(e) => warn!("config changed but reload failed: {e}"),
        }
    }
}

async fn accept_loop(listener: TcpListener, cfg: Arc<Config>, layout: SharedLayout, sessions: Sessions, peer_screens: PeerScreens, evt_tx: UnboundedSender<SessionEvent>) {
    loop {
        let Ok((stream, addr)) = listener.accept().await else { return };
        let cfg = cfg.clone();
        let layout = layout.clone();
        let sessions = sessions.clone();
        let peer_screens = peer_screens.clone();
        let evt_tx = evt_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, cfg, layout, sessions, peer_screens, evt_tx).await {
                debug!("connection from {addr}: {e}");
            }
        });
    }
}

async fn handle_conn(mut stream: TcpStream, cfg: Arc<Config>, layout: SharedLayout, sessions: Sessions, peer_screens: PeerScreens, evt_tx: UnboundedSender<SessionEvent>) -> Result<()> {
    stream.set_nodelay(true)?;
    let intro = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut stream)).await??;
    let name = match Intro::decode(&intro)? {
        Intro::Session { name } => name,
        Intro::Pair => {
            // Pairing is only served by the dedicated `kayiver pair` command:
            // a running host must never silently accept new devices.
            anyhow::bail!("pair attempt while running; run `kayiver pair` instead");
        }
    };
    let peer = cfg.peer(&name).with_context(|| format!("unknown peer '{name}'"))?;
    let psk = peer.psk_bytes()?;

    let (mut reader, mut writer) = secure::handshake_responder(stream, &psk).await?;

    let hello = tokio::time::timeout(Duration::from_secs(5), reader.recv()).await??;
    let (client_screen, os, monitors) = match hello {
        Msg::Hello { version, screen, os, monitors, .. } => {
            anyhow::ensure!(version == PROTOCOL_VERSION, "protocol version mismatch: {version} != {PROTOCOL_VERSION}");
            (screen, os, monitors)
        }
        other => anyhow::bail!("expected Hello, got {other:?}"),
    };
    debug!(?client_screen, %os, "client hello");

    // Cache the peer's monitor shapes + OS so the layout editor can draw them
    // and map its display indices.
    cache_peer_screens(&name, &monitors, Some(&os), &peer_screens);

    let portal_edges = { layout.read().unwrap().portals(&name) };
    writer
        .send(&Msg::Welcome {
            version: PROTOCOL_VERSION,
            name: cfg.name.clone(),
            portal_edges,
        })
        .await?;

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Msg>();
    sessions.lock().unwrap().insert(name.clone(), out_tx);
    let _ = evt_tx.send(SessionEvent::Connected { name: name.clone() });
    crate::ui::set_connected(&name, true);

    // Ping seq -> send time, so a Pong yields a round-trip measurement.
    let pending: Arc<Mutex<HashMap<u64, Instant>>> = Arc::new(Mutex::new(HashMap::new()));

    // Writer task: relays router messages and keeps the link warm with pings.
    let writer_pending = pending.clone();
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
                    {
                        let mut p = writer_pending.lock().unwrap();
                        p.insert(seq, Instant::now());
                        // Bound the map if pongs stop coming.
                        if p.len() > 32 {
                            let cutoff = Instant::now() - Duration::from_secs(30);
                            p.retain(|_, t| *t > cutoff);
                        }
                    }
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
                Msg::SharedCross { fx, fy } => {
                    let _ = evt_tx.send(SessionEvent::SharedCross { name: name.clone(), fx, fy });
                }
                Msg::Pong(seq) => {
                    let sent = pending.lock().unwrap().remove(&seq);
                    if let Some(sent) = sent {
                        crate::ui::set_rtt(&name, sent.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                Msg::DisplayPowerResult { index, on, error } => match error {
                    None => info!("{name}: display {index} {}", if on { "attached" } else { "detached" }),
                    Some(e) => {
                        warn!("{name}: display {index} {} failed: {e}", if on { "attach" } else { "detach" });
                        crate::ui::set_shared_error(Some(format!("{name}: {e}")));
                    }
                },
                Msg::Monitors { monitors, .. } => {
                    // The peer's desktop changed (a display was attached/detached);
                    // refresh the cache so the editor and crossing use the new shape.
                    debug!("{name}: geometry update, {} monitors", monitors.len());
                    cache_peer_screens(&name, &monitors, None, &peer_screens);
                }
                Msg::Bye => return Ok(()),
                other => debug!("unexpected from {name}: {other:?}"),
            }
        }
    }
    .await;

    sessions.lock().unwrap().remove(&name);
    crate::ui::set_connected(&name, false);
    let _ = evt_tx.send(SessionEvent::Disconnected { name });
    writer_task.abort();
    result
}

/// Persist a peer's monitor shapes (and optionally its OS) so the layout
/// editor can draw them and map display indices. Called from the initial
/// Hello and from later `Monitors` geometry updates.
fn cache_peer_screens(name: &str, monitors: &[kayiver_core::proto::Rect], os: Option<&str>, live: &PeerScreens) {
    if monitors.is_empty() {
        return;
    }
    // Live cache first (cheap, read on the cursor hot path); disk after.
    live.write().unwrap().insert(name.to_string(), monitors.to_vec());
    if let Ok(mut fresh) = Config::load_or_init() {
        if let Some(p) = fresh.peers.iter_mut().find(|p| p.name == name) {
            let os_changed = os.map(|o| p.os.as_deref() != Some(o)).unwrap_or(false);
            if p.screens != monitors || os_changed {
                p.screens = monitors.to_vec();
                if let Some(o) = os {
                    p.os = Some(o.to_string());
                }
                if let Err(e) = fresh.save() {
                    debug!("could not cache peer screens: {e}");
                }
            }
        }
    }
}
