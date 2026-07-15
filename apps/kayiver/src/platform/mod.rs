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
                        let fx = (x - b.x) as f32 / b.w.max(1) as f32;
                        let fy = (y - b.y) as f32 / b.h.max(1) as f32;
                        // Park just outside the edge we came in through so the
                        // local cursor isn't left sitting on the hidden panel.
                        let (dx, dy) = (x - prev.0, y - prev.1);
                        let park = skip_out(b, x, y, -dx, -dy);
                        warp_cursor(park.0, park.1);
                        let _ = tx.send(Captured::SharedEnter { fx: fx.clamp(0.0, 1.0), fy: fy.clamp(0.0, 1.0) });
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
