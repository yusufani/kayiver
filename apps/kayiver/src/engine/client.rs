//! Client engine: connects to the host, injects received input, reports
//! when the cursor pushes back through a portal edge.
//!
//! The client owns its cursor position (integer accumulation of relative
//! deltas, clamped to its own desktop bounds), which is what lets machines
//! with different resolutions and scaling factors interoperate.

use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Context, Result};
use kayiver_core::config::{Config, Peer};
use kayiver_core::layout::{point_on_edge, ratio_on_edge, Edge};
use kayiver_core::proto::{InputEvent, Intro, Msg, Rect, PROTOCOL_VERSION};
use kayiver_core::secure;
use kayiver_core::wire::write_frame;
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::platform::{self, Injector};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// Per-candidate probe: short, so a stale link-local address doesn't stall
/// the whole reconnect round.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
const RECV_TIMEOUT: Duration = Duration::from_secs(20);
const EDGE_INSET: i32 = 2;

pub fn run(cfg: Config) -> Result<()> {
    let host_peer = cfg
        .peers
        .first()
        .cloned()
        .context("no paired host — run `kayiver join <host-ip>` first")?;
    if cfg.peers.len() > 1 {
        warn!("multiple peers configured; using '{}' as host", host_peer.name);
    }

    // Status indicator (Windows tray / no-op elsewhere).
    platform::indicator::start(&host_peer.name);

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        // Serve the editor locally too, so opening it on the client shows the
        // arrangement + connection status (it's mostly informational on a
        // client; layout/shared changes are driven from the host).
        crate::ui::mark_running();
        tokio::spawn(async {
            if let Err(e) = crate::ui::serve_forever().await {
                debug!("client ui server not started: {e:#}");
            }
        });
        info!("layout editor: {}", crate::ui::url());

        let mut backoff = Duration::from_secs(1);
        // host_peer is re-read each round so learned addresses apply live.
        let host_name = host_peer.name.clone();
        loop {
            let peer = Config::load_or_init()
                .ok()
                .and_then(|c| c.peer(&host_name).cloned())
                .unwrap_or_else(|| host_peer.clone());
            match connect_once(&cfg, &peer).await {
                Ok(()) => {
                    info!("session ended, reconnecting");
                    crate::ui::set_link_error(Some("oturum kapandı — yeniden bağlanılıyor".into()));
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    warn!("connection failed: {e:#}");
                    crate::ui::set_link_error(Some(format!("{e:#}")));
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                }
            }
            platform::indicator::set_state(false, false);
            crate::ui::set_connected(&host_name, false);
            tokio::time::sleep(backoff).await;
        }
    })
}

/// Candidate addresses in preference order: last known good, the configured
/// primary, learned fallbacks — then mDNS discovery as the zero-config path.
/// A short probe (PROBE_TIMEOUT) filters dead addresses quickly (link-local
/// IPs drift whenever a cable is re-plugged, so any of these can go stale).
async fn find_host(peer: &Peer) -> std::result::Result<SocketAddr, String> {
    let mut candidates: Vec<String> = Vec::new();
    let push = |a: &str, v: &mut Vec<String>| {
        if !a.is_empty() && !v.iter().any(|x| x == a) {
            v.push(a.to_string());
        }
    };
    if let Some(a) = &peer.last_good {
        push(a, &mut candidates);
    }
    if let Some(a) = &peer.addr {
        push(a, &mut candidates);
    }
    for a in &peer.addrs {
        push(a, &mut candidates);
    }

    let mut failures: Vec<String> = Vec::new();
    for cand in &candidates {
        match cand.to_socket_addrs().ok().and_then(|mut it| it.next()) {
            Some(a) => {
                match tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(a)).await {
                    Ok(Ok(_)) => return Ok(a),
                    Ok(Err(e)) => failures.push(format!("{cand}: {e}")),
                    Err(_) => failures.push(format!("{cand}: zaman aşımı")),
                }
            }
            None => failures.push(format!("{cand}: adres çözülemedi")),
        }
    }
    if let Some(a) = kayiver_core::discovery::resolve(&peer.name, Duration::from_secs(3)).await {
        return Ok(a);
    }
    failures.push("mDNS: bulunamadı".into());
    Err(format!(
        "'{}' bulunamadı — denenen yollar: {}",
        peer.name,
        failures.join(" · ")
    ))
}

/// Remember the address a session actually succeeded over, so the next
/// reconnect tries it first (and it survives restarts).
fn remember_good_addr(peer_name: &str, addr: SocketAddr) {
    let addr = addr.to_string();
    if let Ok(mut cfg) = Config::load_or_init() {
        if let Some(p) = cfg.peers.iter_mut().find(|p| p.name == peer_name) {
            let known = p.addr.as_deref() == Some(addr.as_str()) || p.addrs.iter().any(|a| a == &addr);
            let mut dirty = false;
            if p.last_good.as_deref() != Some(addr.as_str()) {
                p.last_good = Some(addr.clone());
                dirty = true;
            }
            if !known {
                p.addrs.push(addr);
                if p.addrs.len() > 4 {
                    p.addrs.remove(0);
                }
                dirty = true;
            }
            if dirty {
                let _ = cfg.save();
            }
        }
    }
}

async fn connect_once(cfg: &Config, peer: &Peer) -> Result<()> {
    let addr = find_host(peer).await.map_err(|e| anyhow::anyhow!(e))?;
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
    crate::ui::set_connected(&peer.name, true);
    crate::ui::set_link_error(None);
    remember_good_addr(&peer.name, addr);
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
                // A display was just attached/detached: our desktop geometry
                // changed, so refresh the bounds that crossing + injection use
                // (otherwise the cursor would land using the stale, pre-detach
                // desktop and end up in the wrong place / on the hidden panel).
                if matches!(&result, Msg::DisplayPowerResult { error: None, .. }) {
                    let nb = platform::desktop_bounds();
                    state.bounds = nb;
                    state.injector = Injector::new()?;
                    state.pos = (nb.x + nb.w / 2, nb.y + nb.h / 2);
                    info!("desktop geometry refreshed after display change: {nb:?}");
                    // Note: we intentionally don't send Msg::Monitors here — an
                    // older host wouldn't decode it. The editor picks up the new
                    // shape on the next reconnect; crossing is already correct
                    // because it maps ratios against these refreshed bounds.
                }
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
                    info!("pushed through {edge} edge -> returning control to host");
                    writer.send(&Msg::CursorLeft { edge, ratio }).await?;
                }
            }
            Msg::Ping(n) => writer.send(&Msg::Pong(n)).await?,
            Msg::DisplayPower { index, expect, on } => {
                info!("host asks: {} display {index}", if on { "attach" } else { "detach" });
                let tx = dp_tx.clone();
                std::thread::spawn(move || {
                    let error = platform::set_display_enabled(index, Some(expect), on).err().map(|e| format!("{e:#}"));
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
