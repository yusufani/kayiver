//! Client engine: connects to the host, injects received input, reports
//! when the cursor pushes back through a portal edge.
//!
//! The client owns its cursor position (integer accumulation of relative
//! deltas, clamped to its own desktop bounds), which is what lets machines
//! with different resolutions and scaling factors interoperate.

use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Context, Result};
use drift_core::config::{Config, Peer};
use drift_core::layout::{point_on_edge, ratio_on_edge, Edge};
use drift_core::proto::{InputEvent, Intro, Msg, Rect, PROTOCOL_VERSION};
use drift_core::secure;
use drift_core::wire::write_frame;
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::platform::{self, Injector};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const RECV_TIMEOUT: Duration = Duration::from_secs(20);
const EDGE_INSET: i32 = 2;

pub fn run(cfg: Config) -> Result<()> {
    let host_peer = cfg
        .peers
        .first()
        .cloned()
        .context("no paired host — run `drift join <host-ip>` first")?;
    if cfg.peers.len() > 1 {
        warn!("multiple peers configured; using '{}' as host", host_peer.name);
    }

    // Status indicator (Windows tray / no-op elsewhere).
    platform::indicator::start(&host_peer.name);

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let mut backoff = Duration::from_secs(1);
        loop {
            match connect_once(&cfg, &host_peer).await {
                Ok(()) => {
                    info!("session ended, reconnecting");
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    warn!("connection failed: {e:#}");
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                }
            }
            platform::indicator::set_state(false, false);
            tokio::time::sleep(backoff).await;
        }
    })
}

/// Static address first (works over VPNs / multicast-free networks), then
/// mDNS discovery as the zero-config path.
async fn find_host(peer: &Peer) -> Option<SocketAddr> {
    if let Some(addr) = &peer.addr {
        if let Ok(mut addrs) = addr.to_socket_addrs() {
            if let Some(a) = addrs.next() {
                if tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(a)).await.map(|r| r.is_ok()).unwrap_or(false) {
                    return Some(a);
                }
            }
        }
    }
    drift_core::discovery::resolve(&peer.name, Duration::from_secs(3)).await
}

async fn connect_once(cfg: &Config, peer: &Peer) -> Result<()> {
    let addr = find_host(peer).await.context("host not found (static addr and mDNS both failed)")?;
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .context("connect timeout")??;
    stream.set_nodelay(true)?;

    write_frame(&mut stream, &Intro::Session { name: cfg.name.clone() }.encode()?).await?;
    let psk = peer.psk_bytes()?;
    let (mut reader, mut writer) = secure::handshake_initiator(stream, &psk).await?;

    let bounds = platform::desktop_bounds();
    writer
        .send(&Msg::Hello {
            version: PROTOCOL_VERSION,
            name: cfg.name.clone(),
            os: std::env::consts::OS.to_string(),
            screen: bounds,
            monitors: platform::monitors(),
        })
        .await?;

    let portal_edges = match tokio::time::timeout(Duration::from_secs(5), reader.recv()).await?? {
        Msg::Welcome { version, portal_edges, .. } => {
            anyhow::ensure!(version == PROTOCOL_VERSION, "protocol version mismatch");
            portal_edges
        }
        other => anyhow::bail!("expected Welcome, got {other:?}"),
    };

    info!("connected to host '{}' at {addr}; my desktop bounds = {bounds:?}", peer.name);
    platform::indicator::set_state(true, false);
    let mut state = ClientState {
        injector: Injector::new()?,
        bounds,
        portal_edges,
        pos: (bounds.x + bounds.w / 2, bounds.y + bounds.h / 2),
        active: false,
    };

    // Display attach/detach runs blocking on its own thread; results come
    // back through this channel so injection never stalls.
    let (dp_tx, mut dp_rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();

    loop {
        let msg = tokio::select! {
            m = tokio::time::timeout(RECV_TIMEOUT, reader.recv()) => m.context("session timed out")??,
            Some(result) = dp_rx.recv() => {
                writer.send(&result).await?;
                continue;
            }
        };
        match msg {
            Msg::Enter { edge, ratio } => {
                state.pos = point_on_edge(state.bounds, edge, ratio, EDGE_INSET);
                state.injector.mouse_to(state.pos.0, state.pos.1, 0, 0);
                state.active = true;
                platform::indicator::set_state(true, true);
                info!("cursor entered via {edge} edge -> injecting at {:?}", state.pos);
            }
            Msg::Leave => {
                state.active = false;
                state.injector.release_all();
                platform::indicator::set_state(true, false);
                debug!("cursor left; input released");
            }
            Msg::Input(ev) => {
                debug!("input {ev:?} -> pos {:?}", state.pos);
                if let Some((edge, ratio)) = state.apply(ev) {
                    state.active = false;
                    state.injector.release_all();
                    platform::indicator::set_state(true, false);
                    // Hand the shared monitor back to the host's input while
                    // this machine is still displayed (its DDC link works now).
                    switch_shared_display(cfg);
                    info!("pushed through {edge} edge -> returning control to host");
                    writer.send(&Msg::CursorLeft { edge, ratio }).await?;
                }
            }
            Msg::Ping(n) => writer.send(&Msg::Pong(n)).await?,
            Msg::DisplayPower { index, on } => {
                info!("host asks: {} display {index}", if on { "attach" } else { "detach" });
                let tx = dp_tx.clone();
                std::thread::spawn(move || {
                    let error = platform::set_display_enabled(index, on).err().map(|e| format!("{e:#}"));
                    if let Some(e) = &error {
                        warn!("display {index} {}: {e}", if on { "attach" } else { "detach" });
                    }
                    let _ = tx.send(Msg::DisplayPowerResult { index, on, error });
                });
            }
            Msg::Bye => return Ok(()),
            other => warn!("unexpected message: {other:?}"),
        }
    }
}

/// Switch the shared monitor to the host's input (DDC auto-switch), off-thread.
fn switch_shared_display(cfg: &Config) {
    if !cfg.display.auto_switch {
        return;
    }
    let Some(value) = cfg.display.peer_input else { return };
    let index = cfg.display.display_index.unwrap_or(0);
    std::thread::spawn(move || {
        if let Err(e) = platform::set_display_input(index, value) {
            warn!("display switch failed: {e:#}");
        }
    });
}

struct ClientState {
    injector: Injector,
    bounds: Rect,
    portal_edges: Vec<Edge>,
    pos: (i32, i32),
    active: bool,
}

impl ClientState {
    /// Apply one input event. Returns Some((edge, ratio)) when the cursor
    /// pushed through a portal edge and control should go back to the host.
    fn apply(&mut self, ev: InputEvent) -> Option<(Edge, f32)> {
        if !self.active {
            // Events already in flight when we sent CursorLeft: drop them so
            // the cursor doesn't twitch after the handoff.
            return None;
        }
        match ev {
            InputEvent::MouseMove { dx, dy } => {
                let nx = self.pos.0 + dx;
                let ny = self.pos.1 + dy;
                if let Some(hit) = self.portal_hit(nx, ny) {
                    return Some(hit);
                }
                self.pos.0 = nx.clamp(self.bounds.x, self.bounds.right() - 1);
                self.pos.1 = ny.clamp(self.bounds.y, self.bounds.bottom() - 1);
                self.injector.mouse_to(self.pos.0, self.pos.1, dx, dy);
            }
            InputEvent::MouseButton { button, pressed } => self.injector.button(button, pressed),
            InputEvent::Wheel { dx, dy } => self.injector.wheel(dx, dy),
            InputEvent::Key { key, pressed } => self.injector.key(key, pressed),
        }
        None
    }

    fn portal_hit(&self, nx: i32, ny: i32) -> Option<(Edge, f32)> {
        for &edge in &self.portal_edges {
            let out = match edge {
                Edge::Left => nx < self.bounds.x,
                Edge::Right => nx >= self.bounds.right(),
                Edge::Top => ny < self.bounds.y,
                Edge::Bottom => ny >= self.bounds.bottom(),
            };
            if out {
                let cx = nx.clamp(self.bounds.x, self.bounds.right() - 1);
                let cy = ny.clamp(self.bounds.y, self.bounds.bottom() - 1);
                return Some((edge, ratio_on_edge(self.bounds, edge, cx, cy)));
            }
        }
        None
    }
}
