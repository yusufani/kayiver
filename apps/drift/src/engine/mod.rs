pub mod client;
pub mod host;
pub mod pairing;

use drift_core::layout::Edge;
use drift_core::proto::InputEvent;

/// Events flowing from the platform capture thread to the host router.
#[derive(Debug, Clone, Copy)]
pub enum Captured {
    Input(InputEvent),
    /// The local cursor hit a portal edge. The capture layer has already
    /// flipped itself into forwarding (swallow) mode synchronously, so not a
    /// single event leaks to the local desktop while the router catches up.
    EdgeHit { edge: Edge, ratio: f32 },
    /// Panic escape (triple-Esc): capture already dropped out of forwarding.
    Panic,
    /// Shared-monitor hotkey (Cmd/Ctrl+Alt+M). Only emitted while
    /// `CaptureCtl::shared_hotkey` is set; the keystroke is swallowed.
    SharedHotkey,
}
