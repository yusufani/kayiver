
pub mod clipsync;
pub mod host;
pub mod pairing;
pub mod quickshare;

use kayiver_core::layout::Edge;
use kayiver_core::proto::InputEvent;

/// Events flowing from the platform capture thread to the host router.
#[derive(Debug, Clone)]
pub enum Captured {
    Input(InputEvent),
    OrderedInput {stamp:kayiver_core::motion::Stamp,target:Option<String>,event:InputEvent},
    Motion(crate::platform::navigation::Frame),
    /// The local cursor hit a portal edge. The capture layer has already
    /// flipped itself into forwarding (swallow) mode synchronously, so not a
    /// single event leaks to the local desktop while the router catches up.
    EdgeHit { edge: Edge, ratio: f32 },
    /// Panic escape (triple-Esc): capture already dropped out of forwarding.
    Panic,
    /// Shared-monitor hotkey (Cmd/Ctrl+Alt+M). Only emitted while
    /// `CaptureCtl::shared_hotkey` is set; the keystroke is swallowed.
    SharedHotkey,
    /// Tablet-control hotkey (Cmd/Ctrl+Alt+T): toggle handing input to the
    /// Android tablet. The keystroke is swallowed.
    TabletHotkey,
}
