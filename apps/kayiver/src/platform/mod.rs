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
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use kayiver_core::layout::{point_in, skip_out, Edge};
use kayiver_core::proto::Rect;

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
    /// Shared monitor this machine must NOT show right now: the cursor skips
    /// over this rect (never rests on it) so it can't sit on a screen that's
    /// physically displaying the other machine. None = no block.
    pub blocked: RwLock<Option<Rect>>,
    pub bounds: Rect,
}

impl CaptureCtl {
    pub fn new(bounds: Rect) -> Self {
        CaptureCtl {
            forwarding: AtomicBool::new(false),
            portals: RwLock::new(Vec::new()),
            cooldown_until: Mutex::new(Instant::now()),
            shared_hotkey: AtomicBool::new(false),
            blocked: RwLock::new(None),
            bounds,
        }
    }
}

/// Keep the local cursor out of a "blocked" shared-monitor rect: poll the
/// cursor and, whenever it lands inside the block, warp it just past the far
/// edge (in the direction it was moving) so it skips over that monitor as if it
/// weren't there. Cheap busy-poll on its own thread; a no-op while nothing is
/// blocked or while input is being forwarded. Works on any platform that has
/// `cursor_pos` + `warp_cursor`.
pub fn start_cursor_guard(ctl: Arc<CaptureCtl>) {
    std::thread::Builder::new()
        .name("kayiver-cursor-guard".into())
        .spawn(move || {
            let mut prev = cursor_pos();
            loop {
                std::thread::sleep(Duration::from_millis(8));
                if ctl.forwarding.load(Ordering::SeqCst) {
                    prev = cursor_pos();
                    continue;
                }
                let Some(b) = *ctl.blocked.read().unwrap() else {
                    prev = cursor_pos();
                    continue;
                };
                let (x, y) = cursor_pos();
                if point_in(b, x, y) {
                    let (dx, dy) = (x - prev.0, y - prev.1);
                    let mut out = skip_out(b, x, y, dx, dy);
                    // If skipping forward would leave this desktop, bounce back.
                    if !point_in(ctl.bounds, out.0, out.1) {
                        out = skip_out(b, x, y, -dx, -dy);
                    }
                    warp_cursor(out.0, out.1);
                    prev = out;
                } else {
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
