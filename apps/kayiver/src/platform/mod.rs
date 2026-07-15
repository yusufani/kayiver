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

use kayiver_core::layout::{point_in, skip_out, Edge};
use kayiver_core::proto::Rect;

use crate::engine::Captured;

pub struct CaptureCtl {
    /// True while input is being forwarded to a remote machine.
    pub forwarding: AtomicBool,
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
    pub bounds: Rect,
}

impl CaptureCtl {
    pub fn new(bounds: Rect) -> Self {
        CaptureCtl {
            forwarding: AtomicBool::new(false),
            portals: RwLock::new(Vec::new()),
            cooldown_until: Mutex::new(Instant::now()),
            shared_hotkey: AtomicBool::new(false),
            edge_dwell_ms: AtomicU64::new(0),
            blocked: RwLock::new(None),
            tablet_edge: RwLock::new(None),
            bounds,
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
            loop {
                std::thread::sleep(Duration::from_millis(8));
                if ctl.forwarding.load(Ordering::SeqCst) {
                    prev = cursor_pos();
                    inside = false;
                    continue;
                }
                let Some(b) = *ctl.blocked.read().unwrap() else {
                    prev = cursor_pos();
                    inside = false;
                    continue;
                };
                let (x, y) = cursor_pos();
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

/// Where the segment `from`→`to` (ending inside `b`) enters the rect, as
/// fractions across it — the entered edge pinned to exactly 0.0 / 1.0 and the
/// crossing point preserved along it. Every side the segment could have
/// crossed is intersected and the first hit along the travel (smallest t)
/// wins, so a diagonal entry near a corner still resolves to the side that was
/// physically hit first. When there is no crossing to measure (`from` already
/// inside, or no motion — e.g. the block appeared under a resting cursor),
/// falls back to pinning the nearest side of the caught position.
fn entry_on_rect(b: Rect, from: (i32, i32), to: (i32, i32)) -> (f32, f32) {
    let (px, py) = (from.0 as f32, from.1 as f32);
    let (dx, dy) = (to.0 as f32 - px, to.1 as f32 - py);
    let w = b.w.max(1) as f32;
    let h = b.h.max(1) as f32;
    let (x0, y0) = (b.x as f32, b.y as f32);
    let (x1, y1) = ((b.x + b.w) as f32, (b.y + b.h) as f32);

    let mut best: Option<(f32, (f32, f32))> = None;
    let mut consider = |t: f32, fx: f32, fy: f32| {
        // A candidate is a real entry only if the crossing point sits on the
        // rect's side (small tolerance for float rounding at corners).
        let on_side = (-0.01..=1.01).contains(&fx) && (-0.01..=1.01).contains(&fy);
        if (0.0..=1.0).contains(&t) && on_side && best.map_or(true, |(bt, _)| t < bt) {
            best = Some((t, (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0))));
        }
    };
    if dx > 0.0 && px < x0 {
        let t = (x0 - px) / dx;
        consider(t, 0.0, (py + t * dy - y0) / h);
    }
    if dx < 0.0 && px >= x1 {
        let t = (x1 - px) / dx;
        consider(t, 1.0, (py + t * dy - y0) / h);
    }
    if dy > 0.0 && py < y0 {
        let t = (y0 - py) / dy;
        consider(t, (px + t * dx - x0) / w, 0.0);
    }
    if dy < 0.0 && py >= y1 {
        let t = (y1 - py) / dy;
        consider(t, (px + t * dx - x0) / w, 1.0);
    }
    if let Some((_, f)) = best {
        return f;
    }
    let fx = ((to.0 - b.x) as f32 / w).clamp(0.0, 1.0);
    let fy = ((to.1 - b.y) as f32 / h).clamp(0.0, 1.0);
    let (dl, dr, dt, db) = (fx, 1.0 - fx, fy, 1.0 - fy);
    let m = dl.min(dr).min(dt).min(db);
    if m == dl {
        (0.0, fy)
    } else if m == dr {
        (1.0, fy)
    } else if m == dt {
        (fx, 0.0)
    } else {
        (fx, 1.0)
    }
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod stub;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use stub::*;

#[cfg(target_os = "windows")]
mod tray_windows;
#[cfg(target_os = "windows")]
mod passive_windows;

/// A full-screen notice drawn on the shared monitor while it's showing the
/// OTHER machine (this machine's copy is passive). `show(None)` clears it.
/// Implemented on Windows; a no-op elsewhere for now.
pub mod passive {
    use kayiver_core::proto::Rect;
    pub fn show(_state: Option<(Rect, String)>) {
        #[cfg(target_os = "windows")]
        super::passive_windows::show(_state);
    }
}

/// Cross-platform status indicator (system tray / menu bar). Implemented on
/// Windows; a no-op elsewhere for now.
pub mod indicator {
    /// Start the indicator (call once on the client). Non-fatal.
    pub fn start(_host: &str) {
        #[cfg(target_os = "windows")]
        super::tray_windows::start(_host);
    }

    /// Update the indicator when connection / focus changes.
    pub fn set_state(_connected: bool, _cursor_here: bool) {
        #[cfg(target_os = "windows")]
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
