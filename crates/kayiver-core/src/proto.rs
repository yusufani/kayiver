//! Wire protocol messages.
//!
//! Key codes are USB HID keyboard usage IDs (page 0x07). Each platform
//! backend maps its native virtual key codes to/from HID usages, so the
//! wire format is platform-neutral. See `docs/PROTOCOL.md`.

use serde::{Deserialize, Serialize};

use crate::layout::Edge;

/// Bumped on incompatible changes. Peers with different versions refuse to talk.
pub const PROTOCOL_VERSION: u16 = 4;

/// A rectangle in a machine's own desktop coordinate space (bounding box of
/// all its monitors). Origin is top-left on every platform: platform backends
/// normalize (macOS's flipped global space is converted before it gets here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// Do two monitor rects refer to the same physical display? Position anchors
/// the identity; a small tolerance absorbs rounding / minor mode differences.
pub fn rects_match(a: Rect, b: Rect) -> bool {
    let near = |x: i32, y: i32| (x - y).abs() <= 8;
    near(a.x, b.x) && near(a.y, b.y) && near(a.w, b.w) && near(a.h, b.h)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

/// Input events, host -> client. Mouse motion is always relative: the client
/// owns its cursor position, which is what makes crossing feel native and
/// avoids absolute-coordinate mismatch between different resolutions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    MouseMove { dx: i32, dy: i32 },
    MouseButton { button: MouseButton, pressed: bool },
    /// Wheel deltas in 1/120 notch units (Windows convention), positive = up/right.
    Wheel { dx: i32, dy: i32 },
    /// `key` is a USB HID keyboard usage ID.
    Key { key: u16, pressed: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Msg {
    /// client -> host, first message after the secure handshake.
    Hello {
        version: u16,
        name: String,
        os: String,
        /// Bounding box of the whole desktop.
        screen: Rect,
        /// Individual physical displays (used by the layout editor).
        monitors: Vec<Rect>,
    },
    /// host -> client reply. `portal_edges` are the client's own desktop edges
    /// that lead somewhere in the layout; hitting one must emit `CursorLeft`.
    Welcome {
        version: u16,
        name: String,
        portal_edges: Vec<Edge>,
    },
    /// host -> client: the cursor is entering your screen through `edge` at
    /// `ratio` (0..1 along that edge). Warp your cursor there and start
    /// applying `Input` events.
    Enter { edge: Edge, ratio: f32 },
    /// host -> client: stop applying input (focus moved elsewhere).
    Leave,
    Input(InputEvent),
    /// client -> host: my cursor pushed through my `edge` at `ratio`.
    CursorLeft { edge: Edge, ratio: f32 },
    Ping(u64),
    Pong(u64),
    Bye,
    /// host -> client: attach (`on`) or detach one of your displays. Used for
    /// the shared-monitor flow: the machine the panel is NOT showing detaches
    /// it so its cursor can't wander onto an invisible screen. `expect` is the
    /// geometry of the display we mean — the client refuses to detach if the
    /// display at `index` doesn't match it, so an index/ordering slip can never
    /// turn off the wrong monitor.
    DisplayPower { index: u32, expect: Rect, on: bool },
    /// client -> host: outcome of a `DisplayPower` request.
    DisplayPowerResult { index: u32, on: bool, error: Option<String> },
    /// client -> host: my desktop geometry changed (e.g. a display was
    /// detached/attached), so the layout editor and crossing math use the
    /// current monitors instead of the ones from the initial `Hello`.
    Monitors { screen: Rect, monitors: Vec<Rect> },
    /// host -> client: treat `rect` as a hole your cursor can't rest on — the
    /// shared monitor while the OTHER machine is being shown on it. The cursor
    /// skips over it to the next screen. `None` clears the block (you own the
    /// panel now). No display is ever detached.
    SharedBlock { rect: Option<Rect> },
}

impl Msg {
    pub fn encode(&self) -> crate::Result<Vec<u8>> {
        Ok(postcard::to_allocvec(self)?)
    }

    pub fn decode(buf: &[u8]) -> crate::Result<Self> {
        Ok(postcard::from_bytes(buf)?)
    }
}

/// First frame a client sends on a fresh TCP connection, in plaintext, so the
/// host can select the right PSK before the Noise handshake. See SECURITY.md
/// for why the peer name is the only plaintext metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Intro {
    Session { name: String },
    Pair,
}

impl Intro {
    pub fn encode(&self) -> crate::Result<Vec<u8>> {
        Ok(postcard::to_allocvec(self)?)
    }
    pub fn decode(buf: &[u8]) -> crate::Result<Self> {
        Ok(postcard::from_bytes(buf)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_msg() {
        let msgs = vec![
            Msg::Hello {
                version: PROTOCOL_VERSION,
                name: "win-desktop".into(),
                os: "windows".into(),
                screen: Rect { x: 0, y: 0, w: 5120, h: 1440 },
                monitors: vec![
                    Rect { x: 0, y: 0, w: 2560, h: 1440 },
                    Rect { x: 2560, y: 0, w: 2560, h: 1440 },
                ],
            },
            Msg::Input(InputEvent::MouseMove { dx: -3, dy: 7 }),
            Msg::Input(InputEvent::Key { key: 0x04, pressed: true }),
            Msg::CursorLeft { edge: Edge::Left, ratio: 0.42 },
        ];
        for m in msgs {
            let bytes = m.encode().unwrap();
            assert_eq!(Msg::decode(&bytes).unwrap(), m);
        }
    }

    #[test]
    fn mouse_move_is_tiny_on_the_wire() {
        let bytes = Msg::Input(InputEvent::MouseMove { dx: 5, dy: -2 }).encode().unwrap();
        // Latency budget: a move event must fit in a handful of bytes.
        assert!(bytes.len() <= 8, "move event too big: {} bytes", bytes.len());
    }
}
