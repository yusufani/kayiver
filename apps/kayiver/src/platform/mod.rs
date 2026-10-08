//! Platform abstraction. Each OS backend provides the same surface:
//!
//! - `desktop_bounds()` — bounding box of all monitors, top-left origin
//! - `start_capture(ctl, tx)` — host side: grab input, detect portal edges,
//!   swallow events while forwarding
//! - `set_forwarding_visuals(on)` — hide/detach the local cursor while the
//!   input is being forwarded
//! - `warp_cursor(x, y)` / `cursor_pos()`
//! - `Injector` — client side: synthesize input events
//!
//! The capture thread flips `CaptureCtl::forwarding` *synchronously inside
//! the OS callback* when the cursor crosses a portal edge. That is the core
//! latency trick: no round trip to the router before events are swallowed,
//! so nothing ever double-applies locally and remotely.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use kayiver_core::layout::{entry_on_rect, point_in, skip_out, Edge};
use kayiver_core::proto::Rect;

use crate::engine::Captured;

/// A cursor parked on a seam must only cross when pushed OUT of the desk.
/// macOS can report the previous edge position briefly after a return warp.
pub fn motion_towards_edge(edge: Edge, dx: i32, dy: i32) -> bool {
    match edge {
        Edge::Left => dx < 0,
        Edge::Right => dx > 0,
        Edge::Top => dy < 0,
        Edge::Bottom => dy > 0,
    }
}

/// Match the native cursor: desktop-union gaps are walls, not screens.
pub fn clamp_monitor_move(monitors: &[Rect], from: (i32, i32), to: (i32, i32)) -> (i32, i32) {
    if monitors.iter().any(|m| point_in(*m, to.0, to.1)) { return to; }
    let clamp = |m: &Rect| (to.0.clamp(m.x, m.right() - 1), to.1.clamp(m.y, m.bottom() - 1));
    if let Some(m) = monitors.iter().find(|m| point_in(**m, from.0, from.1)) { return clamp(m); }
    monitors.iter().min_by_key(|m| {
        let p = clamp(m);
        let dx = i64::from(p.0) - i64::from(to.0);
        let dy = i64::from(p.1) - i64::from(to.1);
        dx * dx + dy * dy
    }).map(clamp).unwrap_or(from)
}

pub struct CaptureCtl {
    /// Serialize native capture with a return warp. A callback that already
    /// read forwarding must finish before the router restores the local cursor.
    pub motion_gate: Mutex<()>,
    /// True while input is being forwarded to a remote machine.
    pub forwarding: AtomicBool,
    /// True while a PEER is driving this machine. Local hooks deliberately keep
    /// passing input through (a dying session must never leave this desk with a
    /// frozen mouse), so this is what stops the cursor guard and the portal
    /// edges from also reacting and starting a control fight.
    pub driven: AtomicBool,
    /// Edges that currently lead to a *connected* peer. The capture thread
    /// only triggers on these, so the cursor never disappears into a dead
    /// screen whose machine is offline.
    pub portals: RwLock<Vec<Edge>>,
    /// Portal triggers are ignored until this instant (set when the cursor
    /// returns, to stop instant re-triggering on the same edge).
    pub cooldown_until: Mutex<Instant>,
    /// When set, Cmd/Ctrl+Alt+M is swallowed and reported as
    /// `Captured::SharedHotkey` (shared-monitor ownership toggle).
    pub shared_hotkey: AtomicBool,
    /// Milliseconds the cursor must rest against a portal edge before it
    /// crosses. 0 = cross instantly (the default). A dwell guards against
    /// accidental crossings from brushing the edge.
    pub edge_dwell_ms: AtomicU64,
    /// Shared monitor this machine must NOT show right now: the cursor skips
    /// over this rect (never rests on it) so it can't sit on a screen that's
    /// physically displaying the other machine. None = no block.
    pub blocked: RwLock<Option<Rect>>,
    /// Desktop edge that leads to the Android tablet, if placed. Crossing it
    /// hands control to the tablet (like a peer portal, but local).
    pub tablet_edge: RwLock<Option<Edge>>,
    /// macOS host: remap Mac modifiers while a Windows peer has focus.
    pub mac_shortcuts: AtomicBool,
    /// Target LEFT-hid for (⌃, ⌥, ⌘) on Windows peers (right = left+4).
    pub win_mods: RwLock<(u16, u16, u16)>,
    bounds: RwLock<Rect>,
}

impl CaptureCtl {
    pub fn bounds(&self) -> Rect {
        *self.bounds.read().unwrap()
    }

    pub fn update_bounds(&self, bounds: Rect) {
        *self.bounds.write().unwrap() = bounds;
    }

    pub fn new(bounds: Rect) -> Self {
        CaptureCtl {
            motion_gate: Mutex::new(()),
            forwarding: AtomicBool::new(false),
            driven: AtomicBool::new(false),
            portals: RwLock::new(Vec::new()),
            cooldown_until: Mutex::new(Instant::now()),
            shared_hotkey: AtomicBool::new(false),
            edge_dwell_ms: AtomicU64::new(0),
            blocked: RwLock::new(None),
            tablet_edge: RwLock::new(None),
            mac_shortcuts: AtomicBool::new(true),
            win_mods: RwLock::new((0xE0, 0xE3, 0xE0)), // ⌃→Ctrl ⌥→Win ⌘→Ctrl
            bounds: RwLock::new(bounds),
        }
    }
}

/// Watch the local cursor and, when it moves onto the "blocked" shared-monitor
/// rect (which is showing the peer), hand control to the peer: emit
/// `SharedEnter` with the relative hit position and park the cursor just off the
/// panel so it doesn't sit on an invisible screen. Cheap busy-poll on its own
/// thread; a no-op while nothing is blocked or while input is already
/// forwarding. `tx` is the same channel the capture thread feeds the router.
pub fn start_cursor_guard(ctl: Arc<CaptureCtl>, tx: tokio::sync::mpsc::UnboundedSender<Captured>) {
    std::thread::Builder::new()
        .name("kayiver-cursor-guard".into())
        .spawn(move || {
            let mut prev = cursor_pos();
            let mut inside = false;
            let mut last_block: Option<Rect> = None;
            loop {
                std::thread::sleep(Duration::from_millis(8));
                if ctl.forwarding.load(Ordering::SeqCst) || ctl.driven.load(Ordering::SeqCst) {
                    prev = cursor_pos();
                    // Treat wherever the cursor is when we resume as "already
                    // inside": only a real outside->inside move hands over.
                    // While driven, the cursor is the PEER's proxy, and the
                    // peer routinely reclaims the panel (hotkey / physical
                    // switch) with that proxy still resting on it — the
                    // SharedBlock lands, then the Leave clears `driven`, and
                    // reading that as a fresh entry bounced control straight
                    // back to a peer nobody is sitting at, leaving the peer
                    // "driven" with its own guard disabled (stuck desk).
                    inside = true;
                    continue;
                }
                let Some(b) = *ctl.blocked.read().unwrap() else {
                    prev = cursor_pos();
                    inside = false;
                    last_block = None;
                    continue;
                };
                let (x, y) = cursor_pos();
                if last_block != Some(b) {
                    // The block just appeared (or moved). Wherever the cursor
                    // is right now, it did not MOVE there: only a genuine
                    // outside->inside motion is a request to cross. A cursor
                    // that happened to rest on the panel when the peer took
                    // it must stay put (parking is attempted elsewhere, and
                    // is best-effort — a failed warp used to end here as a
                    // handover to a desk nobody was sitting at).
                    last_block = Some(b);
                    inside = point_in(b, x, y);
                    prev = (x, y);
                    continue;
                }
                if point_in(b, x, y) {
                    if !inside {
                        inside = true;
                        let (dx, dy) = (x - prev.0, y - prev.1);
                        // Hand over at the edge we ENTERED through, at the point
                        // where the prev→cur segment actually crosses the panel
                        // boundary — not wherever the 8 ms poll caught the cursor
                        // inside, and not a guess from the dominant travel axis
                        // (which reads a slightly diagonal left-entry as a TOP
                        // entry and dumps the cursor in the peer's top corner).
                        let (fx, fy) = entry_on_rect(b, prev, (x, y));
                        // Park just outside the edge we came in through so the
                        // local cursor isn't left sitting on the hidden panel.
                        let park = skip_out(b, x, y, -dx, -dy);
                        let _motion = ctl.motion_gate.lock().unwrap();
                        if ctl.forwarding.load(Ordering::SeqCst) || ctl.driven.load(Ordering::SeqCst)
                            || *ctl.blocked.read().unwrap() != Some(b) {
                            continue;
                        }
                        // The Mac callback must not ship input before the
                        // SharedEnter reaches the same queue. Freeze here, not
                        // one async router turn later. Detached Mac motion does
                        // not need a return warp to an old callback park.
                        #[cfg(any(target_os = "macos", feature = "sim"))]
                        {
                            ctl.forwarding.store(true, Ordering::SeqCst);
                            set_forwarding_visuals(true);
                        }
                        #[cfg(not(all(target_os = "macos", not(feature = "sim"))))]
                        warp_cursor(park.0, park.1);
                        let _ = tx.send(Captured::SharedEnter { fx, fy });
                        prev = park;
                    }
                } else {
                    inside = false;
                    prev = (x, y);
                }
            }
        })
        .ok();
}

// The `sim` feature swaps the whole OS backend for a scriptable virtual
// machine (virtual monitors/cursor, recorded injection, a JSON control
// socket). `cargo test --features sim` runs real host↔client sessions —
// real router, real Noise, real TCP — against that virtual desk.
#[cfg(feature = "sim")]
mod sim;
#[cfg(feature = "sim")]
pub use sim::*;

#[cfg(all(target_os = "macos", not(feature = "sim")))]
mod macos;
#[cfg(all(target_os = "macos", not(feature = "sim")))]
pub use macos::*;

#[cfg(all(target_os = "windows", not(feature = "sim")))]
mod windows;
#[cfg(all(target_os = "windows", not(feature = "sim")))]
pub use windows::*;

#[cfg(not(any(target_os = "macos", target_os = "windows", feature = "sim")))]
mod stub;
#[cfg(not(any(target_os = "macos", target_os = "windows", feature = "sim")))]
pub use stub::*;

#[cfg(all(target_os = "windows", not(feature = "sim")))]
mod tray_windows;
#[cfg(all(target_os = "windows", not(feature = "sim")))]
mod passive_windows;
#[cfg(all(target_os = "windows", not(feature = "sim")))]
pub mod quickshare_windows;

/// A full-screen notice drawn on the shared monitor while it's showing the
/// OTHER machine (this machine's copy is passive). `show(None)` clears it.
/// Implemented on Windows; a no-op elsewhere for now.
pub mod passive {
    use kayiver_core::proto::Rect;
    pub fn show(_state: Option<(Rect, String)>) {
        #[cfg(all(target_os = "windows", not(feature = "sim")))]
        super::passive_windows::show(_state);
    }
}

/// Cross-platform status indicator (system tray / menu bar). Implemented on
/// Windows; a no-op elsewhere for now.
pub mod indicator {
    /// Start the indicator (call once on the client). Non-fatal.
    pub fn start(_host: &str) {
        #[cfg(all(target_os = "windows", not(feature = "sim")))]
        super::tray_windows::start(_host);
    }

    /// Update the indicator when connection / focus changes.
    pub fn set_state(_connected: bool, _cursor_here: bool) {
        #[cfg(all(target_os = "windows", not(feature = "sim")))]
        super::tray_windows::set_state(_connected, _cursor_here);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shared panel on this desk: B at (2560,0) 2560x1440, entered from A.
    fn b() -> Rect {
        Rect { x: 2560, y: 0, w: 2560, h: 1440 }
    }

    #[test]
    fn diagonal_left_entry_stays_at_entry_height() {
        // Down-right at >45°: the old dominant-axis guess read this as a TOP
        // entry and dumped the cursor in the peer's top-left corner.
        let (fx, fy) = entry_on_rect(b(), (2550, 700), (2565, 760));
        assert_eq!(fx, 0.0);
        assert!((fy - 740.0 / 1440.0).abs() < 0.01, "fy={fy}");
    }

    #[test]
    fn straight_left_entry() {
        let (fx, fy) = entry_on_rect(b(), (2500, 700), (2600, 700));
        assert_eq!(fx, 0.0);
        assert!((fy - 700.0 / 1440.0).abs() < 0.01);
    }

    #[test]
    fn corner_entry_picks_first_side_hit() {
        // From above the rect near its corner, moving down-right: only the
        // top edge is a real crossing, even though the motion is mostly
        // vertical AND horizontal candidates exist nearby.
        let (fx, fy) = entry_on_rect(
            Rect { x: 2560, y: 100, w: 2560, h: 1340 },
            (2600, 60),
            (2700, 220),
        );
        assert_eq!(fy, 0.0);
        assert!((fx - (2625.0 - 2560.0) / 2560.0).abs() < 0.01, "fx={fx}");
    }

    #[test]
    fn no_motion_falls_back_to_nearest_side() {
        // Block appeared under a resting cursor near the left edge.
        let (fx, fy) = entry_on_rect(b(), (2570, 700), (2570, 700));
        assert_eq!(fx, 0.0);
        assert!((fy - 700.0 / 1440.0).abs() < 0.01);
    }
}

/// Backends without persistent identities use conservative geometry matching.
#[cfg(any(feature = "sim", not(target_os = "macos")))]
pub fn identified_monitors() -> Vec<(Option<String>, Rect)> {
    monitors().into_iter().map(|r| (None, r)).collect()
}

/// Which local monitors (same order as `monitors()`) are built-in laptop
/// panels. Backends that can't tell report all-false.
#[cfg(any(feature = "sim", not(any(target_os = "macos", target_os = "windows"))))]
pub fn builtin_flags() -> Vec<bool> {
    monitors().iter().map(|_| false).collect()
}

pub fn resolve_shared_local(sm: &kayiver_core::config::SharedMonitor) -> Option<(usize, Rect)> {
    let displays = identified_monitors();
    if sm.local_id.is_none() {
        // Local legacy indices cannot distinguish a vanished panel from a
        // same-size remaining screen. Only its exact saved rectangle is safe.
        let saved = sm.local_rect?;
        let mut candidates = displays.iter().enumerate().filter(|(_, (_, r))| *r == saved);
        let first = candidates.next()?;
        return if candidates.next().is_none() { Some((first.0, first.1.1)) } else { None };
    }
    kayiver_core::layout::resolve_monitor(&displays, sm.local_id.as_deref(), sm.local_rect)
        .map(|i| (i, displays[i].1))
}

/// Migrate only an exact, unambiguous saved rectangle; never trust an old index.
pub fn bind_shared_identity(sm: &mut kayiver_core::config::SharedMonitor) -> bool {
    if sm.local_id.is_some() || !sm.configured() { return false; }
    let displays = identified_monitors();
    let mut matches = displays.iter().filter(|(_, r)| Some(*r) == sm.local_rect);
    if let (Some((Some(id), _)), None) = (matches.next(), matches.next()) {
        sm.local_id = Some(id.clone());
        return true;
    }
    false
}

/// Query in the app process, rather than a CLI inheriting Terminal's grants.
pub fn permissions_status() -> serde_json::Value {
    #[cfg(all(target_os = "macos", not(feature = "sim")))]
    {
        let (accessibility, input_monitoring, _) = permission_grants();
        let event_posting = accessibility;
        serde_json::json!({"supported": true, "accessibility": accessibility,
            "input_monitoring": input_monitoring, "event_posting": event_posting,
            "all_granted": accessibility && input_monitoring})
    }
    #[cfg(any(not(target_os = "macos"), feature = "sim"))]
    { serde_json::json!({"supported": false}) }
}

pub fn permission_action(action: &str) -> anyhow::Result<()> {
    anyhow::ensure!(matches!(action, "request" | "accessibility" | "input_monitoring" | "event_posting"), "unknown permission action");
    #[cfg(all(target_os = "macos", not(feature = "sim")))]
    {
        let pane = match action {
            "request" => {
                request_permission_prompts();
                let (accessibility, _, _) = permission_grants();
                if accessibility { "Privacy_ListenEvent" } else { "Privacy_Accessibility" }
            }
            "input_monitoring" => "Privacy_ListenEvent",
            _ => "Privacy_Accessibility",
        };
        let status = std::process::Command::new("open")
            .arg(format!("x-apple.systempreferences:com.apple.preference.security?{pane}"))
            .status()?;
        anyhow::ensure!(status.success(), "could not open macOS permission settings");
        Ok(())
    }
    #[cfg(any(not(target_os = "macos"), feature = "sim"))]
    { anyhow::bail!("macOS permissions are not available on this platform") }
}

/// The GUI exposes permission controls itself; do not steal focus by opening
/// System Settings on a timer. Its engine starts once the user grants access.
#[cfg(target_os = "macos")]
pub fn wait_for_gui_permissions() -> anyhow::Result<()> {
    #[cfg(not(feature = "sim"))]
    {
        loop {
            let (accessibility, input_monitoring, _) = permission_grants();
            if accessibility && input_monitoring { return Ok(()); }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    #[cfg(feature = "sim")]
    { ensure_permissions() }
}
