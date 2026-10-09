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
use std::net::ToSocketAddrs;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use kayiver_core::config::{Config, Mode, Peer, SharedMonitor};
use kayiver_core::layout::{point_in, point_on_edge, Edge, Layout, Link};
use kayiver_core::proto::{InputEvent, Intro, Msg, MouseButton, PROTOCOL_VERSION};
use kayiver_core::secure;
use kayiver_core::wire::{read_frame, write_frame};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, info, warn};

use crate::engine::Captured;
use crate::platform::{self, CaptureCtl, Injector};

/// A peer is driving this machine: which one, and where we believe the cursor
/// is. The position is dead-reckoned from the relative deltas it sends — this
/// side owns the absolute coordinate, which is what lets two machines with
/// different resolutions and scaling interoperate.
struct Driven {
    peer: String,
    pos: (i32, i32),
}

/// What an input event from the driving peer triggered on this desk.


const SESSION_TIMEOUT: Duration = Duration::from_secs(15);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// Per-candidate probe: short, so a stale address doesn't stall a whole
/// reconnect round.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
const EDGE_INSET: i32 = 2;

/// Fast-ping cadence that keeps a Wi-Fi radio out of doze while input is (or
/// is about to be) flowing. Same rationale as the tablet path's keepalive
/// (android.rs): the wake penalty after a pause is 50-200ms vs ~8ms hot RTT.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(8);
/// Keep the radio hot this long after forwarding ends, so hopping back and
/// forth between machines never pays the wake penalty.
const HEARTBEAT_TAIL: Duration = Duration::from_secs(60);
/// Idle tick: how often the heartbeat task re-checks whether to go fast.
/// One ping is sent every 10th idle tick, preserving the old 1s liveness rate.
const HEARTBEAT_TICK: Duration = Duration::from_millis(100);
/// Pre-warm distance: the cursor loitering this close to a portal edge (or the
/// shared panel) starts fast pings, so the radio is awake before the crossing.
const WARM_EDGE_PX: i32 = 100;

/// name -> (session id, sender). The id lets a session's cleanup remove
/// ONLY its own entry: a reconnect inserts the successor first, and the old
/// session's teardown used to wipe it — leaving a live session invisible to
/// the router ("peer offline" while RTT kept updating).
type Sessions = Arc<Mutex<HashMap<String, (u64, UnboundedSender<Msg>)>>>;
static SESSION_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// Layout is shared (and hot-reloaded) so `kayiver ui` edits apply live.
type SharedLayout = Arc<RwLock<Layout>>;
/// Shared-monitor config, hot-reloaded together with the layout.
type SharedCfg = Arc<RwLock<SharedMonitor>>;
/// Live in-memory copy of each peer's monitor shapes, keyed by peer name.
/// Updated when a peer reports geometry; read on the cursor hot path so a
/// shared-edge crossing never has to touch the disk.
type PeerScreens = Arc<RwLock<HashMap<String, Vec<kayiver_core::proto::Rect>>>>;

enum SessionEvent {
    Connected { name: String, connection:u64 },
    Disconnected { name: String },
    /// The peer's cursor moved onto the shared panel (showing this host), at
    /// relative position (fx, fy) — take control back onto our copy of it.
    /// The peer asked for a shared-panel ownership change (its hotkey, tray,
    /// editor button or `kayiver monitor`). We arbitrate; it just asks.
    SharedRequest { name: String, owner: String },
    /// Anything the session reader didn't consume itself, handed to the router
    /// — this is where being DRIVEN by a peer is handled (Enter/Input/Leave).
    Inbound { name: String, msg: Msg, connection:u64 },
    LayoutChanged,
    LocalMonitorsChanged,
}

/// "ctrl"/"alt"/"win" → left-modifier HID; anything else falls back.
fn mod_hid(name: &str, fallback: u16) -> u16 {
    match name {
        "ctrl" => 0xE0,
        "alt" => 0xE2,
        "win" => 0xE3,
        _ => fallback,
    }
}

/// Put this desk's monitors where the editor said they go, if the OS has
/// forgotten. Windows drops the arrangement whenever the shared panel's
/// input is switched away and back — this desk's daily routine — and the
/// crossing math is geometry-first, so a monitor the OS parked "beside" the
/// panel turns the physical top edge into a wall.
fn reapply_arrangement(desired: &[kayiver_core::proto::Rect]) -> bool {
    if desired.is_empty() {
        return false;
    }
    match platform::apply_arrangement(desired) {
        Ok(true) => {
            info!("desk arrangement applied: {desired:?}");
            true
        }
        Ok(false) => false,
        Err(e) => {
            warn!("could not apply desk arrangement: {e:#}");
            false
        }
    }
}

pub fn run(mut cfg: Config) -> Result<()> {
    if platform::bind_shared_identity(&mut cfg.shared_monitor) {
        cfg.save()?;
    }
    reapply_arrangement(&cfg.arrangement);
    let bounds = platform::desktop_bounds();
    info!(name = %cfg.name, ?bounds, "starting kayiver");

    // Status indicator (Windows tray / no-op elsewhere).
    if let Some(p) = cfg.peers.first() {
        platform::indicator::start(&p.name);
    }

    let ctl = Arc::new(CaptureCtl::new(bounds));
    crate::ui::set_capture(ctl.clone());
    let (cap_tx, cap_rx) = mpsc::unbounded_channel();
    if cfg.capture == "off" {
        info!("capture = \"off\": this machine can be driven but never drives");
    } else if let Err(e) = platform::start_capture(ctl.clone(), cap_tx.clone()) {
        // Not fatal: a machine that cannot capture is still a perfectly good
        // screen for its peer to drive.
        warn!("input capture failed to start ({e:#}) — this machine can be driven but cannot drive");
    }

    // A window that opens on OUR copy of the shared panel while the panel is
    // showing the PEER is stranded: nothing is drawn there for us, and the
    // cursor deliberately skips that rect, so it cannot even be fetched. Sweep
    // such windows onto a monitor that is actually showing us. Poll rather
    // than hook: windows appear, move and un-maximize on their own, and one
    // cheap enumeration a second is far simpler than shell hooks.
    {
        let ctl = ctl.clone();
        std::thread::Builder::new()
            .name("kayiver-window-rescue".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                let blocked = *ctl.blocked.read().unwrap();
                if let Some(b) = blocked {
                    platform::rescue_windows_off(b);
                }
            })
            .ok();
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(host_main(cfg, ctl, cap_rx))
}

async fn host_main(cfg: Config, ctl: Arc<CaptureCtl>, mut cap_rx: UnboundedReceiver<Captured>) -> Result<()> {
    let cfg = Arc::new(cfg);
    // Transport role is now the ONLY thing the mode decides: the machine that
    // ran `kayiver pair` listens, the one that ran `join` dials. Both run this
    // same engine and either can take control of the other — a session is full
    // duplex, so which end opened it is invisible above the handshake.
    let listening = cfg.mode == Mode::Host;
    let listener = if listening {
        Some(
            TcpListener::bind(("0.0.0.0", cfg.port))
                .await
                .with_context(|| {
                    format!(
                        "port {} is busy — kayiver is probably already running (check with: pgrep -fl kayiver)",
                        cfg.port
                    )
                })?,
        )
    } else {
        None
    };
    // Keep the daemon alive for the lifetime of the process: dropping it would
    // withdraw the mDNS advertisement.
    let _mdns = listening.then(|| {
        kayiver_core::discovery::advertise(&cfg.name, cfg.port)
            .map_err(|e| warn!("mDNS advertisement failed (static addrs still work): {e}"))
            .ok()
    });

    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let layout: SharedLayout = Arc::new(RwLock::new(cfg.layout.clone()));
    let shared: SharedCfg = Arc::new(RwLock::new(cfg.shared_monitor.clone()));
    // Seed the live peer-screen cache from whatever the config last recorded,
    // so a crossing works even before the peer sends a fresh geometry update.
    let peer_screens: PeerScreens = Arc::new(RwLock::new(
        cfg.peers.iter().map(|p| (p.name.clone(), p.screens.clone())).collect(),
    ));
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();

    // Shared clipboard: watch ours and push changes to every connected peer.
    let clip = crate::engine::clipsync::new_state();
    {
        let sessions_c = sessions.clone();
        crate::engine::clipsync::watch(clip.clone(), move |text| {
            let s = sessions_c.lock().unwrap();
            for (_, tx) in s.values() {
                let _ = tx.send(Msg::Clipboard { text: text.clone() });
            }
        });
    }

    match listener {
        Some(l) => {
            tokio::spawn(accept_loop(l, cfg.clone(), layout.clone(), sessions.clone(), peer_screens.clone(), clip.clone(), evt_tx.clone(), ctl.clone()));
        }
        None => {
            if let Some(p) = cfg.peers.first() {
                tokio::spawn(dial_loop(p.name.clone(), cfg.clone(), layout.clone(), sessions.clone(), peer_screens.clone(), clip.clone(), evt_tx.clone(), ctl.clone()));
            } else {
                warn!("no peer paired — run `kayiver join <host-ip>`");
            }
        }
    }
    tokio::spawn(watch_layout(layout.clone(), shared.clone(), ctl.clone(), evt_tx));

    // Shared-monitor state: arm the hotkey. Ownership survives restarts —
    // the physical panel doesn't change inputs just because kayiver did.
    // Falling back to "host owns it" used to block the peer's copy (and cover
    // it with the notice overlay) right after every deploy while the panel
    // was in fact still showing the peer.
    let shared_peer = shared_peer_name(&cfg, &cfg.shared_monitor);
    let shared_owner = match cfg.shared_monitor.last_owner.clone() {
        Some(o) if o == cfg.name || Some(o.as_str()) == shared_peer.as_deref() => o,
        _ => cfg.name.clone(),
    };
    if cfg.shared_monitor.configured() {
        ctl.shared_hotkey.store(cfg.shared_monitor.hotkey, Ordering::SeqCst);
    }
    ctl.mac_shortcuts.store(cfg.mac_shortcuts, Ordering::Relaxed);
    *ctl.win_mods.write().unwrap() = (
        mod_hid(&cfg.win_modifiers.ctrl, 0xE0),
        mod_hid(&cfg.win_modifiers.opt, 0xE2),
        mod_hid(&cfg.win_modifiers.cmd, 0xE3),
    );
    *ctl.tablet_edge.write().unwrap() = cfg.tablet_edge.as_deref().and_then(parse_edge);
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
        driven: None,
        motion_receivers: HashMap::new(),
        monitor_identities: HashMap::new(),
        local_screens: platform::monitors(),
        injector: None,
        arbiter: cfg.mode == Mode::Host,
        down_keys: HashSet::new(),
        forwarded_keys: HashMap::new(),
        down_buttons: HashSet::new(),
        pending_drop_url: None,
        tablet_active: false,
        tablet_vpos: (0, 0),
        tablet_size: (2560, 1600),
        tablet_entry_ratio: 0.5,
    };

    router.refresh_shared_rects();
    // A desk with no peer still needs its local surfaces from startup.
    router.refresh_navigation();

    // Re-apply the restored ownership locally right away: if the panel is
    // showing the peer, this desk's copy must be blocked from the first
    // frame, not from whenever the peer happens to connect.
    if router.shared_owner != router.cfg.name {
        let sm = router.shared.read().unwrap().clone();
        router.set_local_block(sm.local_rect);
    }

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
                Some(crate::ui::UiCmd::SetSharedOwner(owner)) => router.request_shared_owner(&owner),
                Some(crate::ui::UiCmd::TabletControl(on)) => router.set_tablet_control(on),
                Some(crate::ui::UiCmd::Arrange { machine, monitors }) => router.arrange(&machine, monitors),
                Some(crate::ui::UiCmd::UseAddr { peer, addr }) => {
                    info!("asking {peer} to reconnect via {addr}");
                    let sent = router
                        .sessions
                        .lock()
                        .unwrap()
                        .get(&peer)
                        .map(|(_, tx)| tx.send(Msg::UseAddr { addr }).is_ok())
                        .unwrap_or(false);
                    if !sent {
                        warn!("use-addr: peer '{peer}' offline");
                    }
                }
                Some(crate::ui::UiCmd::QuickShareAccept { peer, id }) => {
                    info!("sending QuickShareAccept #{id} to {peer}");
                    if let Some(qs) = crate::ui::get_quick_share() {
                        if qs.id == id {
                            if let kayiver_core::proto::QuickSharePayload::File { name, size, .. } = &qs.payload {
                                if let Err(e) = crate::engine::quickshare::engine().prepare_inbound_transfer(id, name, *size) {
                                    warn!("failed to prepare inbound file transfer: {e:#}");
                                }
                            }
                        }
                    }
                    router.send_to(&peer, Msg::QuickShareAccept { id });
                }
                Some(crate::ui::UiCmd::QuickShareDismiss) => {
                    crate::ui::dismiss_quick_share();
                }
                None => break,
            },
        }
        if router.focus != prev_focus {
            crate::ui::set_focus(router.focus.clone());
        }
    }
    Ok(())
}

fn parse_edge(s: &str) -> Option<Edge> {
    match s {
        "left" => Some(Edge::Left),
        "right" => Some(Edge::Right),
        "top" => Some(Edge::Top),
        "bottom" => Some(Edge::Bottom),
        _ => None,
    }
}

/// UHID mouse button bit index for a captured button.
fn button_index(b: MouseButton) -> u8 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::X1 => 3,
        MouseButton::X2 => 4,
    }
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
    /// Set while WE are driving that peer. Mutually exclusive with `driven`:
    /// this desk cannot both send input and receive it.
    focus: Option<String>,
    /// Set while a peer is driving US.
    driven: Option<Driven>,
    /// Tail movement remains valid until the driver acknowledges Leave.
    motion_receivers: HashMap<String, kayiver_core::motion::Receiver>,
    monitor_identities: HashMap<String, Vec<(String, kayiver_core::proto::Rect)>>,
    /// Captured motion is swallowed before it reaches this async router. A
    /// shared return can overtake that queue; those deltas still belong locally.
    local_screens: Vec<kayiver_core::proto::Rect>,
    /// Built on first use — a machine that is never driven never needs it.
    injector: Option<Injector>,
    /// Whether THIS machine arbitrates shared-panel ownership. Control is fully
    /// symmetric, but ownership stays single-writer: the other side asks with
    /// `SharedRequest` rather than deciding for itself, so two flips can never
    /// disagree about which machine the panel is showing.
    arbiter: bool,
    /// Our own edges that lead somewhere, independent of who is driving. This
    /// is what the injected cursor is tested against while we are driven;
    /// `ctl.portals` is the set the OS hook triggers on and goes EMPTY while
    /// driven, so a physical nudge can't start a control fight.
    down_keys: HashSet<u16>,
    forwarded_keys: HashMap<u16,u16>,
    down_buttons: HashSet<MouseButton>,
    /// A URL grabbed from the drag pasteboard when a link was dragged across to
    /// the peer; opened on that peer when the drag is released (left button up).
    pending_drop_url: Option<String>,
    /// While true, captured input is forwarded to the connected Android tablet
    /// (via scrcpy UHID) instead of the local desktop or a peer.
    tablet_active: bool,
    /// Virtual cursor position on the tablet + its size, tracked from relative
    /// deltas so we know when the cursor has walked back to the entry edge.
    tablet_vpos: (i32, i32),
    tablet_size: (i32, i32),
    tablet_entry_ratio: f32,
}

impl Router {


    fn send_to_focus(&self, msg: Msg) {
        // Mac-style shortcuts on Windows peers: swap ⌘↔Ctrl at the wire
        // boundary. Presses and releases pass through the same remap (and
        // release_all sends through here too), so nothing can get stuck.
        let msg = match msg {
            Msg::Input(InputEvent::Key { key, pressed }) => {
                Msg::Input(InputEvent::Key { key: self.remap_key(key), pressed })
            }
            m => m,
        };
        if let Some(name) = &self.focus {
            let dead = {
                let sessions = self.sessions.lock().unwrap();
                match sessions.get(name) {
                    Some((_, tx)) => tx.send(msg).is_err(),
                    None => true,
                }
            };
            if dead {
                debug!("focused session {name} gone");
            }
        }
    }

    fn try_send_quick_share(&self, peer: &str) {
        if let Some(offer) = crate::engine::quickshare::engine().get_offer_for_crossing() {
            info!("quickshare: offering #{}: {:?} to {peer}", offer.id, offer.payload);
            self.send_to(peer, Msg::QuickShareOffer(offer));
        }
    }

    /// Switch the shared panel, or ask the machine that arbitrates to. This is
    /// what makes the hotkey, the tray, the editor button and `kayiver monitor`
    /// work identically on both desks.
    fn request_shared_owner(&mut self, owner: &str) {
        if self.arbiter {
            self.set_shared_owner(owner);
            return;
        }
        let Some(peer) = self.cfg.peers.first().map(|p| p.name.clone()) else {
            warn!("shared panel: no peer to ask");
            return;
        };
        self.send_to(&peer, Msg::SharedRequest { owner: owner.to_string() });
    }

    /// The editor's arrangement for `machine`: ours is persisted and applied
    /// here; a peer's is pushed to it (it persists and applies its own).
    fn arrange(&mut self, machine: &str, monitors: Vec<kayiver_core::proto::Rect>) {
        if machine == self.cfg.name {
            self.adopt_arrangement(monitors);
            return;
        }
        info!("arrangement for {machine}: {monitors:?}");
        let sent = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(machine).map(|(_, tx)| tx.send(Msg::Arrange { monitors }).is_ok()).unwrap_or(false)
        };
        if !sent {
            warn!("arrangement: peer '{machine}' offline — not sent");
            crate::ui::set_link_error(Some(format!("{machine} offline — arrangement not sent")));
        }
    }

    /// Remember the arrangement the arbiter's editor gave us and make the OS
    /// match it now (and again every time it forgets, see `reapply_arrangement`).
    fn adopt_arrangement(&mut self, monitors: Vec<kayiver_core::proto::Rect>) {
        info!("desk arrangement received: {monitors:?}");
        match Config::load_or_init() {
            Ok(mut c) => {
                if c.arrangement != monitors {
                    c.arrangement = monitors.clone();
                    if let Err(e) = c.save() {
                        warn!("could not persist desk arrangement: {e:#}");
                    }
                }
            }
            Err(e) => warn!("could not load config to persist arrangement: {e:#}"),
        }
        reapply_arrangement(&monitors);
    }

    /// Send to a named peer regardless of focus — used while we are the one
    /// being driven, when `focus` is deliberately None.
    fn send_to(&self, peer: &str, msg: Msg) {
        let sessions = self.sessions.lock().unwrap();
        if let Some((_, tx)) = sessions.get(peer) {
            let _ = tx.send(msg);
        }
    }

    /// A peer took control of this desk: start injecting what it sends.
    fn enter_driven(&mut self, peer: &str, pos: (i32, i32)) -> bool {
        if self.injector.is_none() {
            match Injector::new() {
                Ok(i) => self.injector = Some(i),
                Err(e) => {
                    // Bounce it straight back rather than swallowing the
                    // cursor into a machine that cannot move it.
                    warn!("cannot inject input ({e:#}) — refusing control from {peer}");
                    return false;
                }
            }
        }
        // focus is asynchronous. Capture may already have claimed a remote
        // source while its Motion frame is still waiting in the router queue.
        if self.focus.is_some() {return false;}
        {
            let _gate=self.ctl.motion_gate.lock().unwrap();
            if !self.ctl.navigation.lock().unwrap().try_drive(peer) {return false;}
            self.ctl.forwarding.store(false,Ordering::SeqCst);
            self.ctl.driven.store(true, Ordering::SeqCst);
        }
        self.driven = Some(Driven { peer: peer.to_string(), pos });
        self.refresh_portals();
        if let Some(inj) = self.injector.as_mut() {
            inj.mouse_to(pos.0, pos.1, 0, 0);
        }
        platform::indicator::set_state(true, true);
        crate::ui::set_focus(Some(self.cfg.name.clone()));
        info!("{peer} is driving this desk — injecting at {pos:?}");
        true
    }

    /// Control left this desk again (handed back, or the session died).
    fn leave_driven(&mut self) {
        if self.driven.take().is_none() {return;}
        // Release held input while the source remains protected. Publish Local
        // only after native parking and its movement reference are settled.
        if let Some(inj)=self.injector.as_mut() {inj.release_all();}
        {
            let _gate=self.ctl.motion_gate.lock().unwrap();
            if let Some(b)=*self.ctl.blocked.read().unwrap() {self.park_off_hidden_panel(b);}
            platform::rebase_native_capture();
            self.ctl.navigation.lock().unwrap().drive(None);
            self.ctl.driven.store(false,Ordering::SeqCst);
        }
        self.refresh_portals();
        platform::indicator::set_state(true, false);
        crate::ui::set_focus(None);
        debug!("no longer driven; input released");
    }

    /// If our cursor is resting on `b` — our copy of the shared panel, which
    /// is (about to be) blocked because the panel shows the peer — move it to
    /// a monitor that is actually showing us. A cursor left on a hidden panel
    /// is invisible, and the guard would read it as a fresh "moved onto the
    /// panel" the moment it looks, handing control to a peer nobody is
    /// sitting at. Park on a monitor CENTRE rather than skipping out to the
    /// rect's edge: on a two-monitor desk that edge is usually a portal edge
    /// (and off-desktop on the panel's outer side), which is its own handover.
    fn park_off_hidden_panel(&self, b: kayiver_core::proto::Rect) {
        let (x, y) = platform::cursor_pos();
        if !point_in(b, x, y) {
            return;
        }
        if let Some(m) = platform::monitors().into_iter().find(|m| *m != b) {
            platform::warp_cursor_settled(m.x + m.w / 2, m.y + m.h / 2);
            info!("cursor was resting on the hidden panel — parked on {m:?}");
        }
    }

    fn set_local_block(&self, rect: Option<kayiver_core::proto::Rect>) {
        *self.ctl.blocked.write().unwrap() = rect;
        // Use the resolved local geometry for every ownership path: startup,
        // local hotkey/editor, peer sync, and display arrangement changes.
        platform::passive::show(rect.map(|r| (r,
            "Bu ekran diğer bilgisayarı gösteriyor. Bu bilgisayara dönmek için ekranın girişini değiştirip Ctrl+Alt+M tuşlarına basın.".into()
        )));
    }

    /// Apply a block on our copy of the panel that the PEER decided (over the
    /// wire, or adopted from its state) — as opposed to one this desk's own
    /// hotkey asked for, where a cursor already resting on the panel is
    /// deliberately carried over. Nobody here pressed anything, so a cursor
    /// sitting on the panel must be parked, never handed over. This is what
    /// every (re)connect used to trip: the arbiter re-sends its block on
    /// connect, the peer's cursor was still on its panel copy, and the peer
    /// promptly took control of the arbiter with no one at its desk.
    fn apply_peer_block(&mut self, rect: Option<kayiver_core::proto::Rect>) {
        let sm = self.shared.read().unwrap().clone();
        let rect = if rect.is_some() && sm.local_id.is_some() {
            platform::resolve_shared_local(&sm).map(|(_, r)| r)
        } else { rect };
        if let Some(b) = rect {
            let busy = self.ctl.forwarding.load(Ordering::SeqCst) || self.driven.is_some();
            if !busy {
                self.park_off_hidden_panel(b);
            }
        }
        self.set_local_block(rect);
        // Don't wait a poll tick: anything already sitting on the panel we
        // just lost is stranded right now.
        if let Some(b) = rect {
            platform::rescue_windows_off(b);
        }
    }

    /// Remember which machine the panel shows across restarts. Only a real
    /// change touches the disk (the editor also writes the config).
    fn persist_shared_owner(&self, owner: &str) {
        if let Ok(mut c) = Config::load_or_init() {
            if c.shared_monitor.last_owner.as_deref() != Some(owner) {
                c.shared_monitor.last_owner = Some(owner.to_string());
                if let Err(e) = c.save() {
                    warn!("could not persist shared owner: {e:#}");
                }
            }
        }
    }

    /// Carry the unconsumed movement over the seam, including packets sent
    /// before the driver learned about the handoff. Never inject into a new focus.




    /// Apply one input event from the driving peer, dead-reckoning our own
    /// absolute cursor. Returns the crossing it caused, if any.


    /// Mirror the arbiter's actual screen pair, not just its editor or owner.
    /// Legacy client indices can still point at a different monitor after a
    /// primary-display change, even when its editor already shows the right pair.
    fn adopt_shared_pair(&mut self, peer: &str, state: &str, owner: &str) -> bool {
        let parsed = (|| -> Option<(usize, kayiver_core::proto::Rect, usize, kayiver_core::proto::Rect, Vec<kayiver_core::proto::Rect>)> {
            let state: serde_json::Value = serde_json::from_str(state).ok()?;
            let shared = state.get("shared_monitor")?;
            if shared.get("peer")?.as_str()? != self.cfg.name { return None; }
            let local_pick = usize::try_from(shared.get("peer_monitor")?.as_u64()?).ok()?;
            let peer_pick = usize::try_from(shared.get("local_monitor")?.as_u64()?).ok()?;
            let machines = state.get("machines")?.as_array()?;
            let rect = |name: &str, pick: usize| {
                let machine = machines.iter().find(|m| m["name"].as_str() == Some(name))?;
                serde_json::from_value::<kayiver_core::proto::Rect>(machine.get("monitors")?.get(pick)?.clone()).ok()
            };
            let remote = machines.iter().find(|m| m["name"].as_str() == Some(peer))?;
            let remote_screens = serde_json::from_value::<Vec<kayiver_core::proto::Rect>>(remote.get("monitors")?.clone()).ok()?;
            Some((local_pick, rect(&self.cfg.name, local_pick)?, peer_pick, rect(peer, peer_pick)?, remote_screens))
        })();
        let Some((_, local_rect, peer_pick, peer_rect, remote_screens)) = parsed else { return false };
        // A synced view can be in flight while the local monitor list changes.
        // Bind only an exact, unique current screen; never guess from resolution.
        let monitors = platform::monitors();
        let picks: Vec<_> = monitors.iter().enumerate().filter(|(_, r)| **r == local_rect).map(|(i, _)| i).collect();
        if picks.len() != 1 { return false; }
        // StateSync is authored by the authenticated arbiter. Welcome does not
        // include its monitors, so a newly paired client may have no cache yet.
        cache_peer_screens(peer, &remote_screens, None, &self.peer_screens);
        let Ok(mut saved) = Config::load_or_init() else { return false };
        let mut next = self.shared.read().unwrap().clone();
        next.local_index = Some(if cfg!(target_os = "macos") { picks[0] as u32 + 1 } else { picks[0] as u32 });
        next.local_rect = Some(local_rect);
        next.local_id = platform::identified_monitors().get(picks[0]).and_then(|(id, _)| id.clone());
        next.peer_index = Some(if saved.peer(peer).and_then(|p| p.os.as_deref()) == Some("macos") {
            peer_pick as u32 + 1
        } else { peer_pick as u32 });
        next.peer_rect = Some(peer_rect);
        next.peer = Some(peer.to_string());
        next.last_owner = Some(owner.to_string());
        let changed = *self.shared.read().unwrap() != next;
        if changed {
            info!("shared pair adopted from {peer}: local={local_rect:?}, peer={peer_rect:?}");
            *self.shared.write().unwrap() = next.clone();
        }
        if saved.shared_monitor != next {
            saved.shared_monitor = next;
            if let Err(e) = saved.save() { warn!("could not persist shared pair: {e:#}"); }
        }
        changed
    }

    /// Route one message that arrived from `name`. These are the arms that
    /// used to live in the client engine — every machine handles them now.
    fn on_inbound(&mut self, name: String, msg: Msg) {
        match msg {
            Msg::Enter {..} | Msg::EnterAt {..} => debug!("reserved legacy handoff ignored"),
            Msg::Leave => {
                if self.driven.as_ref().is_some_and(|d| d.peer == name) {
                    self.leave_driven();
                }
            }
            Msg::Navigation { topology } => {
                if !self.arbiter {
                    let mut nav=self.ctl.navigation.lock().unwrap();
                    if topology.revision<nav.topology.revision {return;}
                    let changed=topology!=nav.topology;
                    nav.install(self.cfg.name.clone(), topology);
                    let lost=matches!(nav.control,platform::navigation::Control::Recovering) || (changed && self.focus.is_some() && matches!(nav.control,platform::navigation::Control::Local));
                    drop(nav);
                    if lost {self.recover_local("active display changed");}
                    self.refresh_portals();
                }
            }
            Msg::MonitorIdentity { monitors } => {
                self.monitor_identities.insert(name.clone(), monitors);
                self.refresh_navigation();
            }
            Msg::NavigationRejected {stamp,reason}=>{
                let current={
                    let _gate=self.ctl.motion_gate.lock().unwrap();
                    let current=self.ctl.navigation.lock().unwrap().reject_control(&name,stamp);
                    if current {self.ctl.forwarding.store(false,Ordering::SeqCst);}
                    current
                };
                if current {self.recover_local(&reason);}
            }
            Msg::CursorFrame { stamp, surface, x, y, keys, buttons } => {
                let nav = self.ctl.navigation.lock().unwrap();
                let valid = nav.topology.revision == stamp.revision && nav.topology.surface(&surface)
                    .is_some_and(|s| s.machine == self.cfg.name && point_in(s.rect,x,y));
                drop(nav);
                if !valid {self.send_to(&name,Msg::NavigationRejected {stamp,reason:"layout changed or target disappeared".into()});return;}
                let receiver = self.motion_receivers.entry(name.clone()).or_default();
                let session = receiver.last.map_or(stamp.session,|last|last.session);
                if !receiver.accept(stamp,session,stamp.revision) { return; }
                if self.focus.is_some() || self.driven.as_ref().is_some_and(|d|d.peer != name) {self.send_to(&name,Msg::NavigationRejected {stamp,reason:"another controller is active".into()});return;}
                if !self.driven.as_ref().is_some_and(|d|d.peer == name) {
                    if !self.enter_driven(&name,(x,y)) {self.send_to(&name,Msg::NavigationRejected {stamp,reason:"input control could not be acquired".into()});return;}
                    if let Some(i)=self.injector.as_mut() {
                        for key in keys {i.key(key,true);}
                        for button in buttons {i.button(button,true);}
                    }
                }
                if let Some(d) = self.driven.as_mut() { d.pos=(x,y); }
                if self.injector.as_mut().is_none_or(|i|!i.mouse_to(x,y,0,0)) {
                    self.send_to(&name,Msg::NavigationRejected {stamp,reason:"cursor injection failed".into()});
                    self.leave_driven();
                }
            }
            Msg::CursorRelease { stamp } => {
                let receiver=self.motion_receivers.entry(name.clone()).or_default();
                let session=receiver.last.map_or(stamp.session,|last|last.session);
                if !receiver.accept(stamp,session,stamp.revision) {return;}
                if self.driven.as_ref().is_some_and(|d|d.peer==name) { self.leave_driven(); }
            }
            Msg::ControlledInput {stamp,event}=>{
                let revision=self.ctl.navigation.lock().unwrap().topology.revision;
                let receiver=self.motion_receivers.entry(name.clone()).or_default();
                let session=receiver.last.map_or(stamp.session,|last|last.session);
                if receiver.accept(stamp,session,revision) && self.driven.as_ref().is_some_and(|d|d.peer==name) {
                    if let Some(i)=self.injector.as_mut() {match event {
                        InputEvent::Key {key,pressed}=>i.key(key,pressed),
                        InputEvent::MouseButton {button,pressed}=>i.button(button,pressed),
                        InputEvent::Wheel {dx,dy}=>i.wheel(dx,dy),_=>{}
                    }}
                }
            }
            Msg::Input(ev) => {
                if !self.driven.as_ref().is_some_and(|d|d.peer==name) {return;}
                if let Some(i)=self.injector.as_mut() {match ev {
                    InputEvent::MouseMove {..}=>{},
                    InputEvent::MouseButton {button,pressed}=>i.button(button,pressed),
                    InputEvent::Key {key,pressed}=>i.key(key,pressed),
                    InputEvent::Wheel {dx,dy}=>i.wheel(dx,dy),
                }}
            }
            Msg::SharedBlock { rect } => {
                info!("shared block -> {rect:?}");
                self.apply_peer_block(rect);
            }
            Msg::Arrange { monitors } => self.adopt_arrangement(monitors),
            Msg::StateSync { state, shared_configured, owner } => {
                // Only the arbiter authors the editor view; it must never adopt
                // the other side's copy of it (both machines broadcast on
                // connect, so without this they would overwrite each other).
                if self.arbiter {
                    return;
                }
                debug!("state synced from {name} ({} bytes)", state.len());
                let pair_changed = self.adopt_shared_pair(&name, &state, &owner);
                crate::ui::set_synced_state(state);
                crate::ui::set_shared_state(shared_configured, Some(name.clone()), Some(owner.clone()));
                // The arbiter's word on who the panel shows is THE state; ours
                // was only ever a restart-restored guess. Adopt it, or the two
                // desks disagree forever (each side's copy of the owner only
                // moved on its own hotkey) and re-assert opposite blocks at
                // every reconnect — both panels blocked, each cursor skipping.
                let sm = self.shared.read().unwrap().clone();
                let known = owner == self.cfg.name
                    || shared_peer_name(&self.cfg, &sm).as_deref() == Some(owner.as_str());
                if sm.configured() && known && (self.shared_owner != owner || pair_changed) {
                    info!("shared owner adopted from {name}: {owner}");
                    self.shared_owner = owner.clone();
                    self.persist_shared_owner(&owner);
                    crate::ui::set_shared_owner(Some(owner.clone()));
                    let to_me = owner == self.cfg.name;
                    self.apply_peer_block(if to_me { None } else { sm.local_rect });
                    self.refresh_portals();
                }
            }
            // Exhaustive on purpose — no catch-all. These are consumed by the
            // session reader itself, or are not router business. Adding a
            // variant must be a compile error here rather than a silent drop:
            // StateSync, Ping and UseAddr were lost exactly that way when the
            // host and client engines merged.
            Msg::Hello { .. }
            | Msg::Welcome { .. }
            | Msg::Ping(_)
            | Msg::Pong(_)
            | Msg::Bye
            | Msg::CursorLeft { .. }
            | Msg::CursorLeftCarry { .. }
            | Msg::SharedCross { .. }
            | Msg::SharedCarry {..}
            | Msg::Monitors { .. }
            | Msg::Builtin { .. }
            | Msg::Clipboard { .. }
            | Msg::OpenUrl { .. }
            | Msg::SharedRequest { .. }
            | Msg::UseAddr { .. }
            | Msg::QuickShareOffer(_)
            | Msg::QuickShareAccept { .. }
            | Msg::QuickShareChunk { .. }
            | Msg::QuickShareStatus { .. } => debug!("not router business, from {name}"),
        }
    }

    /// Enter/leave tablet control: swallow local input and route it to the
    /// connected Android device (or restore local control).
    fn set_tablet_control(&mut self, on: bool) {
        if on {
            if !crate::android::is_connected() {
                return;
            }
            self.release_all();
            self.send_to_focus(Msg::Leave);
            self.focus = None;
            self.tablet_active = true;
            self.ctl.tablet_forwarding.store(true,Ordering::SeqCst);
            crate::android::wake(); // light up a slept screen
            self.ctl.forwarding.store(true, Ordering::SeqCst);
            platform::set_forwarding_visuals(true);
            crate::ui::set_focus(Some("tablet".into()));
            info!("controlling tablet");
        } else if self.tablet_active {
            self.tablet_active = false;
            self.ctl.tablet_forwarding.store(false,Ordering::SeqCst);
            self.ctl.navigation.lock().unwrap().drive(None);
            self.exit_forwarding();
            let b = self.ctl.bounds();
            platform::warp_cursor_settled(b.x + b.w / 2, b.y + b.h / 2);
            crate::ui::set_focus(None);
            info!("tablet control released");
        }
    }

    /// If `edge` is the tablet's edge, hand control to the tablet. Returns true
    /// if the crossing was for the tablet (handled).
    fn try_tablet_cross(&mut self, edge: Edge, ratio: f32) -> bool {
        let Some(te) = *self.ctl.tablet_edge.read().unwrap() else { return false };
        if edge != te {
            return false;
        }
        if !crate::android::is_connected() {
            // Not ready — connect in the background and bounce the cursor back
            // so the next crossing works.
            std::thread::spawn(|| {
                crate::android::ensure_connected();
            });
            self.exit_forwarding();
            let (x,y)=point_on_edge(self.ctl.bounds(),edge,ratio,0);
            platform::warp_cursor_settled(x,y);
            return true;
        }
        let (tw, th) = crate::android::size().unwrap_or((2560, 1600));
        self.tablet_size = (tw, th);
        self.tablet_entry_ratio = ratio;
        // Enter a little INSIDE the edge (not right on it), so pushing back
        // toward that edge returns cleanly and there's no instant bounce.
        let ins = 160.min(tw / 3).min(th / 3);
        self.tablet_vpos = match edge {
            Edge::Right => (ins, (ratio * th as f32) as i32),
            Edge::Left => (tw - ins, (ratio * th as f32) as i32),
            Edge::Top => ((ratio * tw as f32) as i32, th - ins),
            Edge::Bottom => ((ratio * tw as f32) as i32, ins),
        };
        self.set_tablet_control(true);
        true
    }

    /// Track the tablet's virtual cursor and, when it walks back to the entry
    /// edge, return control to this desktop.
    fn tablet_track(&mut self, dx: i32, dy: i32) {
        let (tw, th) = self.tablet_size;
        self.tablet_vpos.0 = (self.tablet_vpos.0 + dx).clamp(0, tw);
        self.tablet_vpos.1 = (self.tablet_vpos.1 + dy).clamp(0, th);
        crate::android::mouse_move(dx, dy);
        let te = *self.ctl.tablet_edge.read().unwrap();
        let (vx, vy) = self.tablet_vpos;
        // Return to the desktop when the cursor walks back to the entry edge.
        let back = match te {
            Some(Edge::Right) => vx <= 0,
            Some(Edge::Left) => vx >= tw,
            Some(Edge::Top) => vy >= th,
            Some(Edge::Bottom) => vy <= 0,
            None => false,
        };
        if back {
            // Return to this desktop's edge at the same relative position.
            // Return to the SAME desktop edge we crossed out of (not the
            // opposite one), at the entry position — that's where the cursor left.
            let entry = te.unwrap_or(Edge::Left);
            self.set_tablet_control(false);
            let (x, y) = kayiver_core::layout::point_on_edge(self.ctl.bounds(), entry, self.tablet_entry_ratio, EDGE_INSET);
            platform::warp_cursor_settled(x, y);
        }
    }

    fn on_captured(&mut self, ev: Captured, _cap_rx: &mut UnboundedReceiver<Captured>) {
        match ev {
            Captured::Motion(frame) => self.on_motion_frame(frame),
            Captured::OrderedInput {stamp,target,event}=> {
                if self.tablet_active {self.on_captured(Captured::Input(event),_cap_rx);return;}
                match event {
                    InputEvent::Key {key,pressed}=>{if pressed {self.down_keys.insert(key);}else{self.down_keys.remove(&key);}},
                    InputEvent::MouseButton {button,pressed}=>{if pressed {self.down_buttons.insert(button);}else{self.down_buttons.remove(&button);}},_=>{}
                }
                if target==self.focus {
                    if let Some(peer)=target {
                        let event=match event {
                            InputEvent::Key {key,pressed}=> {
                                let mapped=self.forwarded_keys.get(&key).copied().unwrap_or_else(||self.remap_key(key));
                                if pressed {
                                    let existing=self.forwarded_keys.contains_key(&key);
                                    let held=self.forwarded_keys.values().any(|v|*v==mapped);
                                    self.forwarded_keys.insert(key,mapped);
                                    if !existing && held {return;}
                                } else {
                                    self.forwarded_keys.remove(&key);
                                    if self.forwarded_keys.values().any(|v|*v==mapped) {return;}
                                }
                                InputEvent::Key {key:mapped,pressed}
                            },other=>other
                        };
                        self.send_to(&peer,Msg::ControlledInput {stamp,event});
                        if matches!(event,InputEvent::MouseButton {button:MouseButton::Left,pressed:false}) {
                            if let Some(url)=self.pending_drop_url.take() {self.send_to(&peer,Msg::OpenUrl {url});}
                        }
                    }
                }
            }
            Captured::Input(InputEvent::MouseMove {dx,dy}) => {
                if self.tablet_active {self.tablet_track(dx,dy);}
            }
            Captured::Input(ev) if self.tablet_active => {
                // Tablet control: mouse + keyboard become UHID reports.
                match ev {
                    InputEvent::MouseButton { button, pressed } => {
                        crate::android::mouse_button(button_index(button), pressed);
                    }
                    InputEvent::Wheel { dx, dy } => crate::android::mouse_scroll(dx, dy),
                    InputEvent::Key { key, pressed } => crate::android::key(key, pressed),
                    _ => {}
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
                // A link dragged across is "dropped" when the left button comes
                // up on the peer: open it there.
                if let InputEvent::MouseButton { button: MouseButton::Left, pressed: false } = ev {
                    if let Some(url) = self.pending_drop_url.take() {
                        info!("dropped link on peer -> open {url}");
                        self.send_to_focus(Msg::OpenUrl { url });
                    }
                }
            }
            Captured::EdgeHit {edge,ratio}=> {self.try_tablet_cross(edge,ratio);}
            Captured::CaptureFailure {generation,reason} => {
                let nav=self.ctl.navigation.lock().unwrap();
                let current=nav.generation==generation && nav.control==platform::navigation::Control::Recovering;
                drop(nav);
                if current {self.recover_local(reason);}
            },
            Captured::Panic => {
                if self.tablet_active {self.set_tablet_control(false);}
                self.recover_local("escape shortcut restored local control");
            }
            Captured::SharedHotkey => self.request_shared_owner("toggle"),
            Captured::TabletHotkey => {
                if self.tablet_active {
                    self.set_tablet_control(false);
                } else if crate::android::is_connected() {
                    self.set_tablet_control(true);
                } else {
                    // Connect in the background (blocking); press again once up.
                    std::thread::spawn(|| { crate::android::ensure_connected(); });
                    crate::ui::set_link_error(Some("tablet connecting — try again".into()));
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

        // Persist so a restart resumes with the panel's real state instead of
        // silently claiming it back for this machine.
        self.persist_shared_owner(&owner);
        crate::ui::set_shared_owner(Some(owner));
        crate::ui::set_shared_error(None);
        self.broadcast_state();

        // Cursor-skip model (no display is ever touched): the machine that is
        // NOT being shown blocks its shared rect so the cursor skips over it.
        // Local (host): block local_rect unless the host owns the panel.
        self.set_local_block(if to_me { None } else { sm.local_rect });
        self.refresh_navigation();

        // Peer: block its rect when the host owns the panel; clear when it does.
        let block = if to_me { sm.peer_rect } else { None };
        let msg = Msg::SharedBlock { rect: block };
        let sent = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(&peer).map(|(_, tx)| tx.send(msg).is_ok()).unwrap_or(false)
        };
        if !sent {
            warn!("shared monitor: peer '{peer}' offline");
            crate::ui::set_shared_error(Some(format!("{peer} offline")));
        }

        // The panel just stopped showing the peer we are currently forwarding
        // ALL our input to (crossed onto the panel, then the panel was flipped
        // back to us — hotkey, editor button, or the physical KVM switch).
        // Forwarding is global once crossed, not scoped to the panel rect, so
        // without this pull-back input keeps going to the peer while the
        // panel visibly shows us again: the desk looks "stuck" on the peer's
        // side with no way back short of a hard panic escape. Runs LAST, after
        // `blocked` above is already cleared for `to_me` — otherwise warping
        // the cursor onto the (still blocked) panel makes the cursor guard
        // immediately re-trigger the very handover this is undoing.
        if to_me && self.focus.as_deref() == Some(peer.as_str()) {
            info!("shared monitor reclaimed while forwarding to {peer} — pulling input home");
            self.release_all();
            self.send_to_focus(Msg::Leave);
            self.focus = None;
            self.exit_forwarding();
            if let Some(lr) = sm.local_rect {
                platform::warp_cursor_settled(lr.x + lr.w / 2, lr.y + lr.h / 2);
            }
        }
    }

    fn on_session_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Connected { name,connection } => {
                if !self.sessions.lock().unwrap().get(&name).is_some_and(|(id,_)|*id==connection) {return;}
                self.motion_receivers.remove(&name);
                if !self.arbiter {self.ctl.navigation.lock().unwrap().topology=kayiver_core::motion::Topology::default();}
                info!("client connected: {name}");
                // The peer's Hello may carry a different geometry than we
                // last saw (primary display switched while disconnected).
                self.refresh_shared_rects();
                self.refresh_portals();
                // Re-establish the shared-monitor block on the (re)connected
                // peer to match the current owner — the ARBITER's owner. The
                // other side waits to be told (SharedBlock + StateSync) rather
                // than pushing a stale, restart-restored guess of its own.
                if self.arbiter && self.shared.read().unwrap().configured() {
                    let owner = self.shared_owner.clone();
                    self.set_shared_owner(&owner);
                }
                self.broadcast_state();
            }
            SessionEvent::Disconnected { name } => {
                self.motion_receivers.remove(&name);
                self.monitor_identities.remove(&name);
                info!("client disconnected: {name}");
                // If it was driving us, take our own desk back: portals re-arm
                // and any key it left held is released.
                if self.driven.as_ref().is_some_and(|d| d.peer == name) {
                    self.leave_driven();
                }
                if self.focus.as_deref() == Some(name.as_str()) {
                    // Never leave the user with no cursor: pull input home.
                    self.recover_local("connection lost");
                }
                self.refresh_portals();
            }
            SessionEvent::SharedRequest { name, owner } => {
                info!("{name} asked for shared panel -> {owner}");
                self.set_shared_owner(&owner);
            }
            SessionEvent::Inbound { name, msg, connection } => {
                let current=self.sessions.lock().unwrap().get(&name).is_some_and(|(id,_)|*id==connection);
                if current {self.on_inbound(name,msg);}
            },
            SessionEvent::LocalMonitorsChanged => {
                let msg = Msg::Monitors { screen: platform::desktop_bounds(), monitors: platform::monitors() };
                for (_, tx) in self.sessions.lock().unwrap().values() { let _ = tx.send(msg.clone()); }
                self.on_session_event(SessionEvent::LayoutChanged);
            }
            SessionEvent::LayoutChanged => {
                self.local_screens = platform::monitors();
                self.ctl.update_bounds(platform::desktop_bounds());
                self.refresh_shared_rects();
                self.refresh_portals();
                self.broadcast_state();
            }

        }
    }

    /// Mac-modifier remap for Windows peers (macOS host, `mac_shortcuts`
    /// on): each of ⌃/⌥/⌘ sends whatever the user picked in Settings
    /// (defaults ⌃→Ctrl, ⌥→Win, ⌘→Ctrl). Identity everywhere else.
    fn remap_key(&self, key: u16) -> u16 {
        if !cfg!(target_os = "macos") || !self.ctl.mac_shortcuts.load(Ordering::Relaxed) {
            return key;
        }
        let win_peer = self
            .focus
            .as_ref()
            .and_then(|f| self.cfg.peers.iter().find(|p| &p.name == f))
            .map_or(false, |p| p.os.as_deref() == Some("windows"));
        if !win_peer {
            return key;
        }
        let (ctrl_to, opt_to, cmd_to) = *self.ctl.win_mods.read().unwrap();
        match key {
            // LEFT modifiers carry the user's shortcut muscle memory.
            0xE0 => ctrl_to, // LCtrl
            0xE2 => opt_to,  // LAlt/⌥
            0xE3 => cmd_to,  // LCmd
            // RIGHT modifiers stay untouched: right Alt is AltGr on Windows
            // layouts (Turkish-Q types @ € with it) — remapping it to the Win
            // key silently broke those characters.
            k => k,
        }
    }

    /// Push the host's editor view to every client so their editors mirror
    /// this one (machines with real shapes, links, shared panel, owner).
    fn broadcast_state(&self) {
        // The editor view has one author: the machine that arbitrates. The
        // other side renders what it is told, so it never pushes back.
        if !self.arbiter {
            return;
        }
        let Ok(state) = crate::ui::state_json() else { return };
        let configured = self.shared.read().unwrap().configured();
        let msg = Msg::StateSync {
            state,
            shared_configured: configured,
            owner: self.shared_owner.clone(),
        };
        let sessions = self.sessions.lock().unwrap();
        for (_, tx) in sessions.values() {
            let _ = tx.send(msg.clone());
        }
    }

    fn session_exists(&self, name: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(name)
    }

    /// Resolve local identity before geometry or indices. Keep last known
    /// geometry on disk while unplugged, but remove the effective cursor block.
    fn refresh_shared_rects(&mut self) {
        let sm = self.shared.read().unwrap().clone();
        if !sm.configured() { return; }
        let resolved = platform::resolve_shared_local(&sm);
        let new_local = resolved.map(|(_, r)| r);
        let new_index = resolved.map(|(i, _)| if cfg!(target_os = "macos") { i as u32 + 1 } else { i as u32 }).or(sm.local_index);
        let saved = Config::load_or_init().ok().map(|c| c.shared_monitor);
        let mut new_peer = sm.peer_rect;
        let peer_rect = sm.peer_rect.or_else(|| saved.as_ref().and_then(|s| s.peer_rect));
        if let (Some(pr), Some(peer)) = (peer_rect, shared_peer_name(&self.cfg, &sm)) {
            let screens = self.peer_screens.read().unwrap().get(&peer).cloned().unwrap_or_default();
            let displays: Vec<_> = screens.iter().map(|r| (None, *r)).collect();
            new_peer = kayiver_core::layout::resolve_monitor(&displays, None, Some(pr)).map(|i| screens[i]);
            if let Some(r) = new_peer.filter(|r| *r != pr) {
                info!("shared panel moved on {peer}: {pr:?} -> {r:?}");
            }
        }
        if new_local == sm.local_rect && new_peer == sm.peer_rect && new_index == sm.local_index { return; }
        {
            let mut s = self.shared.write().unwrap();
            s.local_rect = new_local;
            s.local_index = new_index;
            s.peer_rect = new_peer;
        }
        if let Ok(mut c) = Config::load_or_init() {
            let before = c.shared_monitor.clone();
            if let Some(r) = new_local { c.shared_monitor.local_rect = Some(r); }
            if let Some(r) = new_peer { c.shared_monitor.peer_rect = Some(r); }
            c.shared_monitor.local_index = new_index;
            if c.shared_monitor != before {
                if let Err(e) = c.save() { warn!("could not persist shared rects: {e:#}"); }
            }
        }
        let owner = self.shared_owner.clone();
        if self.arbiter { self.set_shared_owner(&owner); }
        else {
            let blocked = self.ctl.blocked.read().unwrap().is_some();
            if blocked { self.apply_peer_block(new_local); }
        }
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



    fn exit_forwarding(&self) {
        self.ctl.forwarding.store(false, Ordering::SeqCst);
        platform::set_forwarding_visuals(false);
    }

    /// Portal edges are only armed when the machine behind them is online —
    /// and never when they're a shared-panel wall (the panel fills that whole
    /// desktop edge and nothing sits beyond it). Arming a wall would let the
    /// capture thread grab the cursor only for the router to bounce it back:
    /// the brief stutter felt at B's far edge. Leaving it unarmed makes it a
    /// plain desktop edge the cursor rests against.
    fn refresh_portals(&mut self) {
        self.refresh_navigation();
        let nav=self.ctl.navigation.lock().unwrap();
        let mut edges=Vec::new();
        for seam in &nav.topology.seams {
            if nav.topology.surface(&seam.from).is_some_and(|s|s.machine==self.cfg.name)
                && nav.topology.surface(&seam.to).is_some_and(|s|s.machine!=self.cfg.name)
                && !edges.contains(&seam.edge) {edges.push(seam.edge);}
        }
        *self.ctl.portals.write().unwrap()=if self.driven.is_some(){Vec::new()}else{edges};
    }

    fn refresh_navigation(&mut self) {
        if !self.arbiter { return; }
        use kayiver_core::motion::{Desk, SharedPanel, Topology};
        let mut local: Vec<_> = platform::identified_monitors().into_iter().enumerate()
            .map(|(index,(id,r))|(id.unwrap_or_else(||format!("legacy-{index}")),r)).collect();
        let configured=self.shared.read().unwrap().clone();
        if self.shared_owner!=self.cfg.name && shared_peer_name(&self.cfg,&configured).is_some_and(|p|!self.session_exists(&p)) {
            local.retain(|(_,r)|Some(*r)!=configured.local_rect);
        }
        let mut desks=vec![Desk {machine:self.cfg.name.clone(),monitors:local.clone()}];
        for (name,monitors) in &self.monitor_identities {
            if self.session_exists(name) { desks.push(Desk {machine:name.clone(),monitors:monitors.clone()}); }
        }
        desks.sort_by(|a,b|a.machine.cmp(&b.machine));
        for d in &mut desks {d.monitors.sort_by(|a,b|a.0.cmp(&b.0));}
        let sm=self.shared.read().unwrap().clone();
        let panel=(|| {
            let peer=shared_peer_name(&self.cfg,&sm)?;
            let lr=sm.local_rect?;let pr=sm.peer_rect?;
            let l:Vec<_>=local.iter().filter(|(_,r)|*r==lr).collect();
            let remote=self.monitor_identities.get(&peer)?;
            let r:Vec<_>=remote.iter().filter(|(_,r)|*r==pr).collect();
            if l.len()!=1 || r.len()!=1 {return None;}
            Some(SharedPanel {a:format!("{}:{}",self.cfg.name,l[0].0),b:format!("{}:{}",peer,r[0].0),owner:self.shared_owner.clone()})
        })();
        let links:Vec<_>=self.layout.read().unwrap().links.iter().map(|l|(l.from.clone(),l.edge,l.to.clone())).collect();
        let mut nav=self.ctl.navigation.lock().unwrap();
        let mut topology=Topology::compile(nav.topology.revision,&desks,panel.as_ref(),&links);
        let changed=topology!=nav.topology;
        if changed {topology.revision=nav.topology.revision+1;}
        nav.install(self.cfg.name.clone(),topology.clone());
        let mut edges=Vec::new();
        if !matches!(nav.control,platform::navigation::Control::Driven(_)) {
            for seam in &nav.topology.seams {
                if nav.topology.surface(&seam.from).is_some_and(|s|s.machine==self.cfg.name) && nav.topology.surface(&seam.to).is_some_and(|s|s.machine!=self.cfg.name) && !edges.contains(&seam.edge) {edges.push(seam.edge);}
            }
        }
        *self.ctl.portals.write().unwrap()=edges;
        let lost=matches!(nav.control,platform::navigation::Control::Recovering) || (changed && self.focus.is_some() && matches!(nav.control,platform::navigation::Control::Local));
        drop(nav);
        if lost {self.recover_local("active display changed");}
        let names:Vec<_>=self.sessions.lock().unwrap().keys().cloned().collect();
        for name in names {self.send_navigation(&name,&topology);}
    }

    fn send_navigation(&self, name:&str, topology:&kayiver_core::motion::Topology) {
        // Called without taking the session map a second time.
        if let Some((_,tx))=self.sessions.lock().unwrap().get(name) {let _=tx.send(Msg::Navigation {topology:topology.clone()});}
    }

    fn recover_local(&mut self, reason:&str) {
        warn!("restoring local cursor: {reason}");
        self.release_all();self.send_to_focus(Msg::Leave);self.focus=None;
        self.leave_driven();
        {
            let _gate=self.ctl.motion_gate.lock().unwrap();let mut nav=self.ctl.navigation.lock().unwrap();
            nav.drive(None);nav.keys.clear();nav.buttons.clear();
            self.ctl.forwarding.store(false,Ordering::SeqCst);
        }
        self.forwarded_keys.clear();
        self.exit_forwarding();
        let blocked=*self.ctl.blocked.read().unwrap();
        if let Some(r)=self.local_screens.iter().find(|r|Some(**r)!=blocked) {
            platform::warp_cursor_settled(r.x+r.w/2,r.y+r.h/2);
        }
        crate::ui::set_link_error(Some(reason.into()));
    }

    fn on_motion_frame(&mut self, frame:platform::navigation::Frame) {
        let nav=self.ctl.navigation.lock().unwrap();
        let current=frame.stamp.session==nav.session && frame.stamp.generation==nav.generation;
        drop(nav);
        if !current {return;}

        let next=if frame.machine==self.cfg.name {None}else{Some(frame.machine.clone())};
        if self.focus!=next {
            let old=self.focus.clone();
            self.down_keys.clear();self.down_buttons.clear();
            if let Some(old)=old {self.send_to(&old,Msg::CursorRelease {stamp:frame.stamp});}
            self.forwarded_keys.clear();
            self.focus=next.clone();
            if next.is_some() {for &key in &frame.keys {let mapped=self.remap_key(key);self.forwarded_keys.insert(key,mapped);}}
            self.pending_drop_url=None;
            if let Some(peer)=&next {
                self.try_send_quick_share(peer);
                if frame.buttons.contains(&MouseButton::Left) {self.pending_drop_url=platform::drag_url();}
            }
            self.down_keys=frame.keys.iter().copied().collect();
            self.down_buttons=frame.buttons.iter().copied().collect();
            info!(target=%frame.machine,x=frame.x,y=frame.y,sequence=frame.stamp.sequence,path=?frame.path,"cursor control changed");
        }
        if let Some(peer)=next {
            if !self.session_exists(&peer) {
                self.recover_local("target disconnected during crossing");
                return;
            }
            let mut keys:Vec<_>=frame.keys.iter().map(|k|self.forwarded_keys.get(k).copied().unwrap_or_else(||self.remap_key(*k))).collect();
            keys.sort_by_key(|key|(!(0xe0..=0xe7).contains(key),*key));keys.dedup();
            self.send_to(&peer,Msg::CursorFrame {stamp:frame.stamp,surface:frame.surface,x:frame.x,y:frame.y,keys,buttons:frame.buttons});
        }
        crate::ui::set_focus(self.focus.clone());
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
    let mut last_monitors = platform::identified_monitors();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let current_monitors = platform::identified_monitors();
        if current_monitors != last_monitors {
            last_monitors = current_monitors;
            let _ = evt_tx.send(SessionEvent::LocalMonitorsChanged);
        }
        let cur = mtime(&path);
        if cur == last {
            continue;
        }
        last = cur;
        match Config::load_or_init() {
            Ok(new_cfg) => {
                ctl.mac_shortcuts.store(new_cfg.mac_shortcuts, Ordering::Relaxed);
                *ctl.win_mods.write().unwrap() = (
                    mod_hid(&new_cfg.win_modifiers.ctrl, 0xE0),
                    mod_hid(&new_cfg.win_modifiers.opt, 0xE2),
                    mod_hid(&new_cfg.win_modifiers.cmd, 0xE3),
                );
                *ctl.tablet_edge.write().unwrap() = new_cfg.tablet_edge.as_deref().and_then(parse_edge);
                let _ = evt_tx.send(SessionEvent::LayoutChanged); // re-arm portals
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

async fn accept_loop(listener: TcpListener, cfg: Arc<Config>, layout: SharedLayout, sessions: Sessions, peer_screens: PeerScreens, clip: crate::engine::clipsync::ClipState, evt_tx: UnboundedSender<SessionEvent>, ctl: Arc<CaptureCtl>) {
    loop {
        let Ok((stream, addr)) = listener.accept().await else { return };
        let cfg = cfg.clone();
        let layout = layout.clone();
        let sessions = sessions.clone();
        let peer_screens = peer_screens.clone();
        let clip = clip.clone();
        let evt_tx = evt_tx.clone();
        let ctl = ctl.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, cfg, layout, sessions, peer_screens, clip, evt_tx, ctl).await {
                debug!("connection from {addr}: {e}");
            }
        });
    }
}

/// Candidate addresses in preference order. The CONFIGURED primary is a
/// deliberate user choice (the editor's path picker), so it gets the full
/// timeout: on a bad Wi-Fi day one lost SYN would otherwise silently demote it.
/// Learned fallbacks get a short probe, then mDNS as the zero-config path.
async fn find_peer(peer: &Peer) -> std::result::Result<std::net::SocketAddr, String> {
    let mut candidates: Vec<(String, Duration)> = Vec::new();
    let push = |a: &str, t: Duration, v: &mut Vec<(String, Duration)>| {
        if !a.is_empty() && !v.iter().any(|(x, _)| x == a) {
            v.push((a.to_string(), t));
        }
    };
    if let Some(a) = &peer.addr {
        push(a, CONNECT_TIMEOUT, &mut candidates);
    }
    if let Some(a) = &peer.last_good {
        push(a, PROBE_TIMEOUT, &mut candidates);
    }
    for a in &peer.addrs {
        push(a, PROBE_TIMEOUT, &mut candidates);
    }

    let mut failures: Vec<String> = Vec::new();
    for (cand, timeout) in &candidates {
        match cand.to_socket_addrs().ok().and_then(|mut it| it.next()) {
            Some(a) => match tokio::time::timeout(*timeout, TcpStream::connect(a)).await {
                Ok(Ok(_)) => return Ok(a),
                Ok(Err(e)) => failures.push(format!("{cand}: {e}")),
                Err(_) => failures.push(format!("{cand}: timed out")),
            },
            None => failures.push(format!("{cand}: could not resolve")),
        }
    }
    if let Some(a) = kayiver_core::discovery::resolve(&peer.name, Duration::from_secs(3)).await {
        return Ok(a);
    }
    failures.push("mDNS: no answer".into());
    Err(format!("'{}' unreachable — tried: {}", peer.name, failures.join(" · ")))
}

/// Remember the address a session actually succeeded over, so the next
/// reconnect tries it first (and it survives restarts).
fn remember_good_addr(peer_name: &str, addr: std::net::SocketAddr) {
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

/// A desk paired before both sides wrote layout links has none of its own, so
/// `portals(me)` is empty and this machine could never START a crossing — it
/// could only ever be driven. The peer's `Welcome` names exactly which of OUR
/// edges lead to it, so adopt them once and persist. New pairings get this
/// from `join` directly; this is the no-re-pairing path for existing desks.
fn bootstrap_links(me: &str, peer: &str, edges: &[Edge], layout: &SharedLayout) {
    if edges.is_empty() || !layout.read().unwrap().portals(me).is_empty() {
        return;
    }
    let Ok(mut cfg) = Config::load_or_init() else { return };
    if !cfg.layout.portals(me).is_empty() {
        return;
    }
    for &edge in edges {
        cfg.layout.links.push(Link { from: me.to_string(), edge, to: peer.to_string() });
    }
    info!("layout: adopted {edges:?} from {peer} — this machine had no links of its own");
    if let Err(e) = cfg.save() {
        warn!("could not persist adopted links: {e:#}");
    }
    *layout.write().unwrap() = cfg.layout;
}

/// Dialer side: keep exactly one session to `peer_name` alive, reconnecting
/// with backoff. The peer entry is re-read each round so an address learned at
/// runtime (or pushed with `UseAddr`) applies without a restart.
#[allow(clippy::too_many_arguments)]
async fn dial_loop(peer_name: String, cfg: Arc<Config>, layout: SharedLayout, sessions: Sessions, peer_screens: PeerScreens, clip: crate::engine::clipsync::ClipState, evt_tx: UnboundedSender<SessionEvent>, ctl: Arc<CaptureCtl>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if sessions.lock().unwrap().contains_key(&peer_name) {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        let peer = Config::load_or_init()
            .ok()
            .and_then(|c| c.peer(&peer_name).cloned())
            .or_else(|| cfg.peer(&peer_name).cloned());
        let Some(peer) = peer else {
            warn!("no peer '{peer_name}' in config");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        };
        match dial_once(&peer, cfg.clone(), layout.clone(), sessions.clone(), peer_screens.clone(), clip.clone(), evt_tx.clone(), ctl.clone()).await {
            Ok(()) => {
                info!("session ended, reconnecting");
                crate::ui::set_link_error(Some("session ended — reconnecting".into()));
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                warn!("connection failed: {e:#}");
                crate::ui::set_link_error(Some(format!("{e:#}")));
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
        platform::indicator::set_state(false, false);
        // Connectivity is not ownership. A failed/duplicate dial must not
        // hide a notice while the local panel remains blocked.
        tokio::time::sleep(backoff).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn dial_once(peer: &Peer, cfg: Arc<Config>, layout: SharedLayout, sessions: Sessions, peer_screens: PeerScreens, clip: crate::engine::clipsync::ClipState, evt_tx: UnboundedSender<SessionEvent>, ctl: Arc<CaptureCtl>) -> Result<()> {
    let addr = find_peer(peer).await.map_err(|e| anyhow::anyhow!(e))?;
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .context("connect timeout")??;
    stream.set_nodelay(true)?;
    let (link_local, link_remote) = (stream.local_addr().ok(), stream.peer_addr().ok());

    write_frame(&mut stream, &Intro::Session { name: cfg.name.clone() }.encode()?).await?;
    let psk = peer.psk_bytes()?;
    let (mut reader, mut writer) = secure::handshake_initiator(stream, &psk).await?;

    writer
        .send(&Msg::Hello {
            version: PROTOCOL_VERSION,
            name: cfg.name.clone(),
            os: std::env::consts::OS.to_string(),
            screen: platform::desktop_bounds(),
            monitors: platform::monitors(),
        })
        .await?;

    let portal_edges = match tokio::time::timeout(Duration::from_secs(5), reader.recv()).await?? {
        Msg::Welcome { version, portal_edges, .. } => {
            anyhow::ensure!(version == PROTOCOL_VERSION, "protocol version mismatch: {version} != {PROTOCOL_VERSION}");
            portal_edges
        }
        other => anyhow::bail!("expected Welcome, got {other:?}"),
    };

    info!("connected to '{}' at {addr}", peer.name);
    crate::ui::set_link_error(None);
    remember_good_addr(&peer.name, addr);
    bootstrap_links(&cfg.name, &peer.name, &portal_edges, &layout);

    run_session(peer.name.clone(), reader, writer, link_local, link_remote, cfg, sessions, peer_screens, clip, evt_tx, ctl).await
}

/// Listener side of a session: identify the caller, complete the handshake,
/// then hand over to the shared `run_session`.
async fn handle_conn(mut stream: TcpStream, cfg: Arc<Config>, layout: SharedLayout, sessions: Sessions, peer_screens: PeerScreens, clip: crate::engine::clipsync::ClipState, evt_tx: UnboundedSender<SessionEvent>, ctl: Arc<CaptureCtl>) -> Result<()> {
    stream.set_nodelay(true)?;
    let (link_local, link_remote) = (stream.local_addr().ok(), stream.peer_addr().ok());
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
    debug!(?client_screen, %os, "peer hello");

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

    run_session(name, reader, writer, link_local, link_remote, cfg, sessions, peer_screens, clip, evt_tx, ctl).await
}

/// Everything above the handshake is symmetric, so both the machine that
/// accepted the connection and the one that dialed it run this: the writer
/// task, the ping/heartbeat task, and the reader loop. Which side dialed is
/// invisible from here — the session is full duplex and either end may take
/// control of the other.
#[allow(clippy::too_many_arguments)]
async fn run_session(
    name: String,
    mut reader: secure::SecureReader,
    writer: secure::SecureWriter,
    link_local: Option<std::net::SocketAddr>,
    link_remote: Option<std::net::SocketAddr>,
    cfg: Arc<Config>,
    sessions: Sessions,
    peer_screens: PeerScreens,
    clip: crate::engine::clipsync::ClipState,
    evt_tx: UnboundedSender<SessionEvent>,
    ctl: Arc<CaptureCtl>,
) -> Result<()> {
    let mut writer = writer;
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Msg>();
    let session_id = SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
    sessions.lock().unwrap().insert(name.clone(), (session_id, out_tx.clone()));
    let _ = evt_tx.send(SessionEvent::Connected { name: name.clone(), connection:session_id });
    crate::ui::set_connected(&name, true);
    crate::ui::set_link(&name, link_local, link_remote);
    let _ = out_tx.send(Msg::Builtin { flags: platform::builtin_flags() });

    // Ping seq -> send time, so a Pong yields a round-trip measurement.
    let pending: Arc<Mutex<HashMap<u64, Instant>>> = Arc::new(Mutex::new(HashMap::new()));

    // Ping task: pushes keep-alive pings through the SAME channel the router
    // uses, so the writer has ONE source. Never let a timer share the writer's
    // `select!` with an in-flight `send` — cancelling a half-written frame
    // desyncs the Noise nonce and the peer drops with `decrypt error`.
    //
    // The cadence is adaptive: 1s while idle (liveness, as before), but 125Hz
    // while forwarding / near a portal edge / for HEARTBEAT_TAIL after, to
    // keep a Wi-Fi radio out of doze — the same trick the tablet path uses.
    // Fast pings are version-neutral: any client just answers Pong, and the
    // pongs keep both NICs hot and feed the RTT badge at input rate.
    let ping_pending = pending.clone();
    let ping_tx = out_tx.clone();
    let ping_ctl = ctl.clone();
    let heartbeat = cfg.heartbeat.clone();
    let ping_task = tokio::spawn(async move {
        let mut seq = 0u64;
        // Not `now - TAIL`: Instant can't represent times before boot.
        let mut warm_until = Instant::now();
        let mut idle_ticks = 0u32;
        loop {
            let forwarding = ping_ctl.forwarding.load(Ordering::SeqCst);
            if forwarding {
                warm_until = Instant::now() + HEARTBEAT_TAIL;
            }
            let fast = match heartbeat.as_str() {
                "always" => true,
                "off" => false,
                _ => forwarding || Instant::now() < warm_until,
            };
            let send = if fast {
                tokio::time::sleep(HEARTBEAT_INTERVAL).await;
                true
            } else {
                tokio::time::sleep(HEARTBEAT_TICK).await;
                if heartbeat.as_str() != "off" && cursor_near_portal(&ping_ctl) {
                    // Pre-warm: the radio wakes while the cursor is still
                    // approaching the edge, so the crossing itself is hot.
                    warm_until = Instant::now() + HEARTBEAT_TAIL;
                }
                idle_ticks += 1;
                idle_ticks >= 10
            };
            if !send {
                continue;
            }
            idle_ticks = 0;
            seq += 1;
            {
                let mut p = ping_pending.lock().unwrap();
                p.insert(seq, Instant::now());
                // Bound the map if pongs stop coming; at 125Hz anything older
                // than a few seconds is a dead measurement anyway.
                if p.len() > 64 {
                    let cutoff = Instant::now() - Duration::from_secs(5);
                    p.retain(|_, t| *t > cutoff);
                }
            }
            if ping_tx.send(Msg::Ping(seq)).is_err() {
                return; // writer gone
            }
        }
    });

    // Geometry watcher: a display attached/detached, or the PRIMARY switched
    // (which re-anchors every rect on Windows), so the peer's crossing math
    // uses current monitors instead of the ones from the initial Hello. It
    // pushes through the SAME channel as everything else — never a timer in a
    // `select!` with `reader.recv()`, which reads with `read_exact` and is not
    // cancel-safe: a timer firing mid-read desyncs the Noise nonce.
    let geo_tx = out_tx.clone();
    let geo_task = tokio::spawn(async move {
        let mut last = platform::monitors();
        // (current shape, desired arrangement) we already tried to apply —
        // once per pair, so an OS that refuses cannot keep us retrying, but
        // a NEW arrangement (or the OS forgetting again) is always tried.
        let mut tried: Option<(Vec<kayiver_core::proto::Rect>, Vec<kayiver_core::proto::Rect>)> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            // Compare the monitor LIST, not the bounding box: a monitor can
            // move without changing the union, and the crossing math cares
            // about each rect.
            let mons = platform::monitors();
            // The OS forgot the desk arrangement (Windows does, every time
            // the panel's input is switched away): put it back first and
            // report only the settled geometry.
            let desired = Config::load_or_init().map(|c| c.arrangement).unwrap_or_default();
            if !desired.is_empty() && tried.as_ref() != Some(&(mons.clone(), desired.clone())) {
                tried = Some((mons.clone(), desired.clone()));
                if reapply_arrangement(&desired) {
                    continue;
                }
            }
            if mons == last {
                continue;
            }
            info!("desktop geometry changed: {last:?} -> {mons:?}");
            last = mons.clone();
            let screen = platform::desktop_bounds();
            if geo_tx.send(Msg::Monitors { screen, monitors: mons }).is_err() {
                return;
            }
            let identities=platform::identified_monitors().into_iter().enumerate()
                .map(|(i,(id,r))|(id.unwrap_or_else(||format!("legacy-{i}")),r)).collect();
            let _=geo_tx.send(Msg::MonitorIdentity {monitors:identities});
            let _ = geo_tx.send(Msg::Builtin { flags: platform::builtin_flags() });
        }
    });

    let identities = platform::identified_monitors().into_iter().enumerate()
        .map(|(index,(id,rect))|(id.unwrap_or_else(||format!("legacy-{index}")),rect)).collect();
    let _ = out_tx.send(Msg::MonitorIdentity { monitors: identities });

    // Writer task: a single serial writer, no competing branch — every frame
    // reaches the socket whole, so the nonce stays in lockstep with the peer.
    let writer_task = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            #[cfg(feature = "sim")]
            if matches!(m, Msg::CursorFrame { .. } | Msg::CursorRelease { .. }) {
                tokio::time::sleep(Duration::from_millis(platform::handoff_delay_ms())).await;
            }
            if writer.send(&m).await.is_err() {
                return;
            }
        }
        let _ = writer.send(&Msg::Bye).await;
    });

    // Reader loop with a liveness watchdog.
    let result: Result<()> = async {
        loop {
            let msg = tokio::time::timeout(SESSION_TIMEOUT, reader.recv()).await??;
            match msg {
                Msg::Pong(seq) => {
                    let sent = pending.lock().unwrap().remove(&seq);
                    if let Some(sent) = sent {
                        crate::ui::set_rtt(&name, sent.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                Msg::Monitors { monitors, .. } => {
                    // The peer's desktop changed (a display was attached/detached,
                    // or its PRIMARY switched — which re-anchors every rect);
                    // refresh the cache and let the router re-derive rects.
                    info!("{name}: geometry update, {} monitors: {monitors:?}", monitors.len());
                    cache_peer_screens(&name, &monitors, None, &peer_screens);
                    let _ = evt_tx.send(SessionEvent::LayoutChanged);
                }
                Msg::Builtin { flags } => {
                    crate::ui::set_builtin(&name, flags);
                    let _ = evt_tx.send(SessionEvent::LayoutChanged);
                }
                Msg::SharedRequest { owner } => {
                    let _ = evt_tx.send(SessionEvent::SharedRequest { name: name.clone(), owner });
                }
                Msg::Clipboard { text } => crate::engine::clipsync::apply_remote(&clip, &text),
                Msg::OpenUrl { url } => {
                    info!("{name}: open url {url}");
                    platform::open_url(&url);
                }
                Msg::QuickShareOffer(offer) => {
                    info!("{name}: quick share offer #{}: {:?}", offer.id, offer.payload);
                    crate::ui::present_quick_share(name.clone(), offer);
                }
                Msg::QuickShareAccept { id } => {
                    info!("{name}: peer accepted quick share file #{id}");
                    if let Some(source_path) = crate::engine::quickshare::engine().find_source_file(id) {
                        let out_tx = out_tx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = crate::engine::quickshare::engine().stream_file(id, source_path, out_tx.clone()).await {
                                warn!("quickshare stream error for #{id}: {e:#}");
                                let _ = out_tx.send(Msg::QuickShareStatus { id, success: false, message: Some(e.to_string()) });
                            }
                        });
                    } else {
                        warn!("{name}: source file for #{id} not found");
                        let _ = out_tx.send(Msg::QuickShareStatus { id, success: false, message: Some("Dosya bulunamadı".into()) });
                    }
                }
                Msg::QuickShareChunk { id, offset, data, is_eof } => {
                    match crate::engine::quickshare::engine().handle_inbound_chunk(id, offset, &data, is_eof) {
                        Ok(Some(final_path)) => {
                            info!("quickshare: file #{id} received successfully: {}", final_path.display());
                            crate::platform::reveal_path(&final_path.to_string_lossy());
                            crate::ui::complete_quick_share_file(id, true, Some(final_path.display().to_string()));
                            let _ = out_tx.send(Msg::QuickShareStatus { id, success: true, message: None });
                        }
                        Ok(None) => {}
                        Err(e) => {
                            warn!("quickshare error writing chunk #{id}: {e:#}");
                            crate::ui::complete_quick_share_file(id, false, Some(e.to_string()));
                            let _ = out_tx.send(Msg::QuickShareStatus { id, success: false, message: Some(e.to_string()) });
                        }
                    }
                }
                Msg::QuickShareStatus { id, success, message } => {
                    info!("{name}: quick share status #{id}: success={success}, msg={message:?}");
                }
                Msg::Ping(n) => {
                    // BOTH sides run a ping task now, so both must answer one.
                    let _ = out_tx.send(Msg::Pong(n));
                }
                Msg::UseAddr { addr } => {
                    // The user picked a different path (Wi-Fi / cable) in the
                    // peer's editor. Persist it as the primary AND the last-good
                    // so the dial loop tries it first, then drop the session.
                    info!("{name} asked us to reconnect via {addr}");
                    if let Ok(mut c) = Config::load_or_init() {
                        if let Some(p) = c.peers.iter_mut().find(|p| p.name == name) {
                            // Keep the old primary as a fallback; never list the
                            // new primary twice.
                            if let Some(old) = p.addr.clone() {
                                if old != addr && !p.addrs.contains(&old) {
                                    p.addrs.push(old);
                                }
                            }
                            p.addrs.retain(|a| a != &addr);
                            p.addr = Some(addr.clone());
                            p.last_good = Some(addr.clone());
                        }
                        if let Err(e) = c.save() {
                            warn!("could not persist new address: {e:#}");
                        }
                    }
                    crate::ui::set_link_error(Some(format!("address changed — reconnecting via {addr}")));
                    return Ok(());
                }
                Msg::Bye => return Ok(()),
                // Everything else is router business — notably Enter/Input/
                // Leave, i.e. this peer driving US.
                other => {
                    let _ = evt_tx.send(SessionEvent::Inbound { name: name.clone(), msg: other,connection:session_id });
                }
            }
        }
    }
    .await;

    // A peer that reconnects replaces our slot with a NEWER session. When the
    // old one dies afterwards it must not mark the peer offline or fire
    // Disconnected: that tore down the live session's portals/focus and left
    // the editor showing a red dot while input still flowed.
    let still_current = {
        let mut s = sessions.lock().unwrap();
        match s.get(&name) {
            Some((id, _)) if *id == session_id => {
                s.remove(&name);
                true
            }
            Some(_) => false,
            None => true,
        }
    };
    if still_current {
        crate::ui::set_connected(&name, false);
        let _ = evt_tx.send(SessionEvent::Disconnected { name });
    }
    ping_task.abort();
    geo_task.abort();
    writer_task.abort();
    result
}

/// True while the local cursor loiters within WARM_EDGE_PX of an armed portal
/// edge or the shared panel's blocked rect — i.e. a crossing may be imminent.
fn cursor_near_portal(ctl: &CaptureCtl) -> bool {
    let (x, y) = platform::cursor_pos();
    let b = ctl.bounds();
    let near_edge = ctl.portals.read().unwrap().iter().any(|e| match e {
        Edge::Left => x - b.x < WARM_EDGE_PX,
        Edge::Right => b.x + b.w - x < WARM_EDGE_PX,
        Edge::Top => y - b.y < WARM_EDGE_PX,
        Edge::Bottom => b.y + b.h - y < WARM_EDGE_PX,
    });
    if near_edge {
        return true;
    }
    // The shared panel is a portal too: while the peer owns it, entering the
    // blocked rect hands control over, so loitering near it warms the radio.
    ctl.blocked.read().unwrap().map_or(false, |r| {
        x >= r.x - WARM_EDGE_PX
            && x < r.x + r.w + WARM_EDGE_PX
            && y >= r.y - WARM_EDGE_PX
            && y < r.y + r.h + WARM_EDGE_PX
    })
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
