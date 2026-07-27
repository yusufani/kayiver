# Wire protocol (version 10)

Transport: TCP, `TCP_NODELAY`, default port **24817**. All frames are
`u16 big-endian length` + payload, max 65535 bytes. Payloads are
[postcard](https://docs.rs/postcard)-serialized Rust enums (varint-based,
so a mouse move is ~6 bytes).

## Connection phases

1. **Intro** (plaintext, client → host, one frame):
   `Intro::Session { name }` — lets the host select the right PSK.
   `Intro::Pair` — only valid against `kayiver pair`, rejected by a running host.
2. **Handshake**: Noise `NNpsk0_25519_ChaChaPoly_BLAKE2s`, client initiates.
   Two frames (`-> psk, e` / `<- e, ee`). Both sides must hold the PSK from
   pairing; a mismatch fails AEAD verification and the connection drops.
3. **Session**: every frame is one encrypted `Msg`. Nonces are the frame
   counters (independent per direction).

## Messages

| Msg | Direction | Purpose |
|---|---|---|
| `Hello { version, name, os, screen: Rect, monitors: [Rect] }` | C → H | First encrypted message. Version mismatch = disconnect. |
| `Welcome { version, name, portal_edges: [Edge] }` | H → C | Which of the client's own desktop edges must report `CursorLeft`. |
| `Enter { edge, ratio }` | H → C | Cursor enters client's screen through `edge` at `ratio` (0..1 along that edge). Client warps its cursor there and starts applying input. |
| `EnterAt { x, y }` | H → C | Warp to an absolute point and take input (crossing onto the shared panel). |
| `Leave` | H → C | Stop applying input; release everything held. |
| `Input(InputEvent)` | H → C | See below. |
| `CursorLeft { edge, ratio }` | C → H | Client cursor pushed through a portal edge; client stops applying input immediately. |
| `Monitors { screen, monitors }` | C → H | The client's desktop geometry changed (display attached/detached, primary switched). |
| `SharedBlock { rect }` | H → C | Treat `rect` as the shared panel showing the OTHER machine: don't rest the cursor on it. `None` clears it. No display is ever detached. |
| `SharedCross { fx, fy }` | C → H | The client's cursor moved onto the shared panel, at relative position in 0..1. |
| `SharedRequest { owner }` | both | Please make `owner` the machine the panel shows (`"toggle"` flips). Lets the hotkey, tray, editor button and `kayiver monitor` work on EITHER machine — the sender asks, the router arbitrates. |
| `StateSync { state, shared_configured, owner }` | H → C | The host's whole editor view as JSON, so both machines draw the same map. |
| `UseAddr { addr }` | H → C | Reconnect to me here (the user picked Wi-Fi vs cable in the editor). |
| `Clipboard { text }` | both | The sender's clipboard changed; mirror it. Echo-guarded on both ends. |
| `OpenUrl { url }` | both | Open this URL — a link was dragged across the boundary. |
| `Ping(u64)` / `Pong(u64)` | H → C / C → H | Liveness + RTT. Cadence is variable and NOT a compatibility surface: 1 s idle, up to 125 Hz while input flows (Wi-Fi radio keepalive). Timeouts 15–20 s. |
| `Bye` | both | Graceful close. |

Directions above describe today's roles, not a protocol constraint: the session
is full duplex and the roles are set at pairing time, not negotiated.

## InputEvent

| Event | Fields | Notes |
|---|---|---|
| `MouseMove` | `dx, dy: i32` | Relative, raw OS deltas. Receiver accumulates, clamps to its bounds. |
| `MouseButton` | `button, pressed` | `Left \| Right \| Middle \| X1 \| X2` |
| `Wheel` | `dx, dy: i32` | 1/120-notch units (Windows convention). Positive = up / right. |
| `Key` | `key: u16, pressed` | **USB HID usage ID, keyboard page (0x07)** — e.g. `A` = 0x04, `LeftShift` = 0xE1. Platform backends translate to native codes. Auto-repeat is transmitted as repeated presses. |

Coordinates & ratios: each machine's `Rect` is the bounding box of all its
monitors, top-left origin (macOS coordinates are already top-left in the CG
global space; nothing else is normalized). `ratio` positions map
proportionally between machines of different sizes.

## Pairing exchange (plaintext TCP, one-shot)

After `Intro::Pair`:

| # | Frame | Notes |
|---|---|---|
| 1 | SPAKE2 message (both directions) | group Ed25519, identity `kayiver-kvm-pairing-v1`, password = 6-digit PIN |
| 2 | `SHA256(key ‖ role-tag)` (both) | key confirmation, direction-tagged (display/input) to kill reflection |
| 3 | `PairInfo { name, port }` (both) | exchanged after confirmation |

Session PSK = `SHA256(key ‖ "kayiver-session-psk-v1")`, stored base64 in the
config of both machines.

## Versioning

`PROTOCOL_VERSION` is checked in `Hello`/`Welcome`. Incompatible changes
bump it; the enums are postcard-encoded by variant index, so **append new
variants at the end** and never reorder existing ones within a version.
