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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};
#[cfg(all(target_os = "macos",not(feature = "sim")))]
use std::time::Duration;

use kayiver_core::layout::{point_in, Edge};
#[cfg(test)]
use kayiver_core::layout::entry_on_rect;
use kayiver_core::proto::Rect;

use crate::engine::Captured;

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

pub mod navigation;
mod cursor_parking;
mod source_motion;
mod motion_trace;

pub struct CaptureCtl {
    pub navigation: Mutex<navigation::Navigation>,
    /// Serialize native capture with a return warp. A callback that already
    /// read forwarding must finish before the router restores the local cursor.
    pub motion_gate: Mutex<()>,
    /// True while input is being forwarded to a remote machine.
    pub forwarding: AtomicBool,
    pub tablet_forwarding: AtomicBool,
    /// Compatibility projection of Navigation::Driven for native suppression.
    pub driven: AtomicBool,
    /// Derived status/prewarming data, never a crossing decision source.
    pub portals: RwLock<Vec<Edge>>,
    /// When set, Cmd/Ctrl+Alt+M is swallowed and reported as
    /// `Captured::SharedHotkey` (shared-monitor ownership toggle).
    pub shared_hotkey: AtomicBool,
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
        motion_trace::initialize();
        CaptureCtl {
            navigation: Mutex::new(navigation::Navigation::default()),
            motion_gate: Mutex::new(()),
            forwarding: AtomicBool::new(false),
            tablet_forwarding: AtomicBool::new(false),
            driven: AtomicBool::new(false),
            portals: RwLock::new(Vec::new()),
            shared_hotkey: AtomicBool::new(false),
            blocked: RwLock::new(None),
            tablet_edge: RwLock::new(None),
            mac_shortcuts: AtomicBool::new(true),
            win_mods: RwLock::new((0xE0, 0xE3, 0xE0)), // ⌃→Ctrl ⌥→Win ⌘→Ctrl
            bounds: RwLock::new(bounds),
        }
    }
}

pub fn capture_input(ctl:&CaptureCtl,tx:&tokio::sync::mpsc::UnboundedSender<Captured>,event:kayiver_core::proto::InputEvent) {
    let (stamp,target)=ctl.navigation.lock().unwrap().input(event);
    let _=tx.send(Captured::OrderedInput {stamp,target,event});
}

// Source-side native state must follow the handoff in the capture callback,
// before a following physical button/key release can overtake it in the router.
thread_local! {static SOURCE_HOLDS: std::cell::RefCell<Option<Injector>> = const {std::cell::RefCell::new(None)};}
fn handoff_source_holds(frame:&navigation::Frame, returning:bool) -> bool {
    SOURCE_HOLDS.with(|slot| {
        let mut slot=slot.borrow_mut();
        if slot.is_none() {match Injector::new() {Ok(i)=>*slot=Some(i),Err(_)=>return false}}
        let injector=slot.as_mut().unwrap();
        // Set coordinates before any release/press can be posted. Native
        // local movement does not update this injector's cached position.
        let (x,y)=if returning {(frame.x,frame.y)}else{cursor_pos()};
        injector.rebase_position(x,y);
        injector.release_all();
        if returning {
            for &key in &frame.keys {injector.key(key,true);}
            for &button in &frame.buttons {injector.button(button,true);}
        } else {
            for &key in &frame.keys {injector.key(key,false);}
            for &button in &frame.buttons {injector.button(button,false);}
        }
        true
    })
}

/// One synchronous decision for all native backends. Call while holding motion_gate.
/// Native warps/association happen after the navigation lock is released.
pub fn route_motion(ctl: &CaptureCtl, tx: &tokio::sync::mpsc::UnboundedSender<Captured>, native:(i32,i32), dx:i32, dy:i32) -> bool {
    route_motion_precise(ctl,tx,kayiver_core::motion::Point::new(native.0 as f64,native.1 as f64),kayiver_core::motion::Point::new(dx as f64,dy as f64),false).handled
}

pub struct RoutedMotion {
    pub handled: bool,
    pub resume: Option<(kayiver_core::motion::Point,kayiver_core::motion::Point)>,
}
impl RoutedMotion {
    fn handled() -> Self {Self {handled:true,resume:None}}
    fn passthrough() -> Self {Self {handled:false,resume:None}}
}
/// A backend can reuse the current physical event for a local return, avoiding
/// a synthetic report or an association change that resets native acceleration.
pub fn route_motion_precise(ctl:&CaptureCtl,tx:&tokio::sync::mpsc::UnboundedSender<Captured>,native:kayiver_core::motion::Point,delta:kayiver_core::motion::Point,reuse_event:bool) -> RoutedMotion {
    let dx=delta.x.round() as i32; let dy=delta.y.round() as i32;
    if ctl.tablet_forwarding.load(Ordering::SeqCst) {
        let _=tx.send(Captured::Input(kayiver_core::proto::InputEvent::MouseMove {dx,dy}));
        return RoutedMotion::handled();
    }
    let frame=ctl.navigation.lock().unwrap().sample_precise(native,delta);
    let Some(frame)=frame else {
        if matches!(ctl.navigation.lock().unwrap().control,navigation::Control::Recovering) {
            ctl.forwarding.store(false,Ordering::SeqCst);set_forwarding_visuals(false);
            let _=tx.send(Captured::Panic);return RoutedMotion::handled();
        }
        return RoutedMotion::passthrough();
    };
    let local=frame.machine==ctl.navigation.lock().unwrap().machine;
    // Android remains a peripheral adapter. Its edge is considered only after
    // the shared geometry engine has established a real local outer wall.
    if local && frame.wall && crate::android::is_connected() {
        let edge=*ctl.tablet_edge.read().unwrap();let b=ctl.bounds();
        let hit=match edge {
            Some(Edge::Left)=>dx<0 && frame.x==b.x,
            Some(Edge::Right)=>dx>0 && frame.x==b.right()-1,
            Some(Edge::Top)=>dy<0 && frame.y==b.y,
            Some(Edge::Bottom)=>dy>0 && frame.y==b.bottom()-1,
            None=>false,
        };
        if hit {
            let edge=edge.unwrap();
            let ratio=match edge {Edge::Left|Edge::Right=>(frame.y-b.y) as f32/b.h.max(1) as f32,_=>(frame.x-b.x) as f32/b.w.max(1) as f32};
            if crate::android::is_connected() {
                ctl.tablet_forwarding.store(true,Ordering::SeqCst);
                ctl.forwarding.store(true,Ordering::SeqCst);set_forwarding_visuals(true);
            }
            let _=tx.send(Captured::EdgeHit {edge,ratio});
            return RoutedMotion::handled();
        }
    }
    // No connected destination was traversed: do not suppress, enqueue or
    // warp local movement, even when the model reports a wall or a corner.
    if local && !frame.handoff {return RoutedMotion::passthrough();}
    let was=ctl.forwarding.swap(!local,Ordering::SeqCst);
    if was != !local {
        set_forwarding_visuals(!local);
        if (!frame.keys.is_empty() || !frame.buttons.is_empty() || local) && !handoff_source_holds(&frame,local) {
            ctl.navigation.lock().unwrap().control=navigation::Control::Recovering;
            let _=tx.send(Captured::Panic);return RoutedMotion::handled();
        }
    }
    let resume=if local && reuse_event {Some((frame.position,frame.local_delta))} else {None};
    if local && !reuse_event {warp_cursor_settled(frame.x,frame.y);}
    if tx.send(Captured::Motion(frame)).is_err() {
        ctl.navigation.lock().unwrap().drive(None);
        ctl.forwarding.store(false,Ordering::SeqCst);set_forwarding_visuals(false);
    }
    RoutedMotion {handled:true,resume}
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
mod window_rescue_macos;
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
/// Implemented on macOS and Windows.
#[cfg(all(target_os = "macos", not(feature = "sim")))]
pub(crate) mod passive_macos;

pub mod passive {
    use kayiver_core::proto::Rect;
    pub fn show(_state: Option<(Rect, String)>) {
        #[cfg(all(target_os = "windows", not(feature = "sim")))]
        super::passive_windows::show(_state);
        #[cfg(all(target_os = "macos", not(feature = "sim")))]
        super::passive_macos::show(_state);
        #[cfg(feature = "sim")]
        super::sim::show_passive_notice(_state);
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
#[cfg(any(feature = "sim", not(any(target_os = "macos", target_os = "windows"))))]
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
