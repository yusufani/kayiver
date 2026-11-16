# Experimental Mac extended display

## Installation status — 16 November 2026

This feature is implemented in the working tree, but is **not available in the
installed Mac application**. Do not present its footer button as an installed
capability. A live Windows upgrade passed a static Defender scan, then Defender
blocked the running application as `Trojan:Win32/Bearfoos.A!ml` and removed its
startup task. The exact affected artifact is blocked in
`scripts/windows-blocked-artifacts.json`; see `WINDOWS-SECURITY.md`.

The subsequent worker-pipe fix built successfully and passed 78 unit tests
(2 ignored), but is withheld from execution following that incident. It does
not establish a cause or resolve the detection. The same-signature Mac upgrade
was also left uninstalled. Live C-monitor streaming and full GUI acceptance
remain unverified. Do not install or run another variant to evade the detector.

Use a Windows-connected monitor as an additional Mac desktop. For example,
drag a Safari window from a physical Mac monitor onto the added desktop:
Safari, the pointer and keyboard input all remain on the Mac. The Windows
application is a display receiver, not a remote-input endpoint.

This feature is **off by default**, creates no startup task, installs no display
driver, and never resumes a stream automatically. First-version support is
one Mac → Windows monitor, native even pixel dimensions up to 2560×1440
(portrait equivalents included), SDR, up to 60 frames/second, without audio.

## Use

Both computers need a build containing this feature and an existing pairing.
The input wire protocol remains 18; older builds still share input with newer
ones but cannot provide the new display service.

1. On Windows, open **Extra display**, select the paired Mac and a local monitor,
   and choose **Accept Mac display session**.
2. On the Mac, open **Extra display**, choose that Windows device, and choose
   **Use Windows as extra display**. Grant macOS Screen Recording permission
   if requested, then retry.
3. Drag a Mac application to the added desktop. Mouse/keyboard sharing need
   not be enabled. The receiver sends no keyboard, pointer or clipboard input.
4. Stop from either editor. Windows also provides **Stop display (Esc)** in
   the receiver window. On the Mac, triple Escape is available through the
   input engine or the helper's listen-only observer when the existing Input
   Monitoring permission is granted; the editor Stop button needs no such grant.

After the Mac stops normally, Windows remains ready for that selected Mac and
monitor for the remainder of this application session, so the next connection
needs just the Mac's Start button. Windows Stop, local receiver Escape, app
exit or restart revokes this readiness. A failed/disconnected session stops
rather than automatically reconnecting.

The selected Windows monitor is occupied by the viewer while streaming;
Windows applications keep running behind it. A physical panel already
configured as cabled to both computers cannot also be a streaming target.
Mirrored/ambiguous targets and unsupported sizes are rejected. The editor
labels/merges the presentation endpoint; it is excluded from the input topology.
Stop the stream before saving a physical desk arrangement, so a temporary
display is never persisted as part of the ordinary monitor inventory.
The added Mac surface starts to the right of the current Mac desktop. Custom
extra-display placement/profiles are deferred; changing its geometry externally
stops the session instead of keeping stale cursor/layout coordinates.

### Network and profiles

The Mac opens TCP **24822** (`configured input port + 5`) only after Start.
The accepted Windows receiver initiates an outgoing connection to that Mac;
no new inbound Windows listener or firewall exception is needed. Both peers
must use the same configured input port. Mac firewall policy must allow the
application on the local network. Kayiver changes no firewall rules.
An active input connection supplies the working Mac IP, including a direct
cable IP; saved numeric peer addresses support screen-only operation. Windows
readiness retries that connection without starting capture: the Mac must also
explicitly enable its source service before a stream can begin.

Initial budgets are 20 Mbps for 1080p and 35 Mbps above 1080p through 1440p.
Dimensions stay native; sustained high latency lowers the capture target to
30 fps and the bitrate to 75% of the initial budget. Good conditions can restore
60 fps. These are tuning defaults, not image-quality or latency guarantees.
An idle desktop need not produce 60 distinct frames each second.

For example, a 100 Mbps Ethernet link can carry a compressed 1080p stream,
but that does not imply enough headroom for lossless images or 144 Hz. File
transfers on the same cable can still compete for bandwidth even though media
and input use different connections.

## Architecture and failure handling

- The app starts an isolated `screen-worker` only on an explicit action. It
  loads the existing pairing identity locally; secrets never enter arguments,
  logs, status responses or media metadata.
- A domain-separated pairing key authenticates a fresh Noise NNpsk0 session.
  The unauthenticated peer-name hint is not authorization. Media version 1
  is independent of input protocol 18, with its own socket and nonce stream.
- macOS's separate `screen-native` process owns the private `CGVirtualDisplay`
  object, ScreenCaptureKit stream and required hardware VideoToolbox H.264
  encoder. At most one frame is being encoded/output; overload skips raw
  frames before encoding, preserving H.264 reference dependencies. Explicit
  per-frame pipe acknowledgments prevent small compressed frames from silently
  accumulating in the operating system pipe. The next capture is released only
  after a presentation slot exists.
- Windows's separate native process uses the selected output's actual DXGI
  adapter, Media Foundation and Direct3D 11. System-memory decoder output
  fails instead of silently enabling CPU decoding. Media Foundation is loaded
  on demand so its absence does not break ordinary Kayiver startup.
- Video access units are limited to 8 MiB, fragmented into 32 KiB encrypted
  chunks with strict sequence/offset/size checks. Queues are bounded, at most
  two video frames await presentation, and a 200 ms enqueue/decoder backlog
  or 500 ms socket-write stall stops the session. Five-second peer liveness
  also applies to a static screen. No old video queue is replayed after reconnect.
- Stop messages are flushed before healthy sessions close. Parent-process
  exit, closed pipes, decoder failure, display loss/mode changes, session lock
  and sleep terminate media. The Mac removes its owned virtual display;
  ordinary windows are rescued best-effort via the existing Accessibility
  adapter and the pointer is moved only if it is on the disappearing display.
  macOS handles the remaining nonstandard/fullscreen window relocation.
- Local display mutations are blocked through the phone/LAN editor API and
  cross-origin browser requests. Accepting a receiver is scoped to the selected
  paired Mac and stable local monitor identity, with geometry revalidation.

The same signed executable runs its native child; there is no extracted helper
with a separate publisher identity. macOS weak-links ScreenCaptureKit so older
systems can still start the input app. The feature needs macOS 12.3+ and runtime
availability of the private virtual-display classes. The native Windows build
currently targets x86_64 GNU, matching the existing release workflow; unsupported
targets and simulator builds report the feature unavailable.

## Risks and acceptance

`CGVirtualDisplay` is a private Apple interface: a macOS update may break it.
H.264 uses 4:2:0 chroma; small colored text can soften even at native resolution.
This release does not promise pixel-identical text, HDR, 4K or high-refresh gaming.
Enabling the feature displays Mac content to anyone looking at the Windows
monitor. Capture/encode/decode add system load. Detaching a display can change
window positions, and custom/fullscreen applications may need manual recovery.

Status reports an **approximate capture-to-Present-submission p95**, estimated
with the handshake clock offset and reported clock uncertainty. This includes
neither reliable GPU scanout completion nor measured display response time.
The 80 ms p95 objective is a target, not a measured glass-to-glass claim.

Before a normal installation/release, check small black/white and colored text,
scrolling, dragging, long-running 1080p/1440p streams, and concurrent input/file
sharing. Check cable loss, sleep, lock, monitor changes, receiver/source crashes,
parent exit, both Stop buttons, Escape and return of windows/pointer. Never
restart the user's working input installation just to preview the design.

### Development checks

```sh
cargo test -p kayiver-core
cargo test -p kayiver --features sim --bin kayiver
cargo test -p kayiver --features sim --test sim_e2e -- --test-threads=1
cargo build -p kayiver
cargo build --release -p kayiver --target x86_64-pc-windows-gnu
target/debug/kayiver screen probe       # Mac availability, no screen creation
target/debug/kayiver screen inspect     # local peers/monitor IDs, no secrets
python3 scripts/test-display-native.py # synthetic encode + pipe backpressure
```

The native compiler is clang++ on macOS and x86_64-w64-mingw32-g++ for the
Windows GNU target. Native source lives in `apps/kayiver/src/display/native`.
The hidden synthetic source produces 180 test frames without capturing the
desktop or creating a virtual monitor. The hidden synthetic sink decodes to an
invisible window in the signed-in Windows session. These are codec/transport
checks, not physical-display visual acceptance:

The native Mac pipe starts with a JSON readiness line, then emits big-endian
`u32 length + u64 capture_time_us + Annex-B H.264` records. Its parent sends one
byte `1` to release the next capture, `0` to stop, or `30`/`60` to change the
frame-rate target. The development check consumes and acknowledges all frames,
including a deliberate 250 ms consumer pause to detect stale-frame buffering.

```sh
# Native Mac encoder; stdout is a readiness JSON line followed by binary frames.
kayiver screen-native test-source 1920 1080 60 20000000
# Screen workers can use synthetic native media for an encrypted LAN check.
# Keep stdin open; close it or write a zero byte to stop the worker.
kayiver screen-worker receive paired-mac 'stable-monitor-id' --synthetic-test
kayiver screen-worker source paired-windows --synthetic-test
```

On 16 November 2026, the Mac encoder produced all 180 synthetic 1080p frames;
the isolated Windows receiver in interactive session 1 hardware-decoded and
submitted all 180 with no decoder error. The candidate passed exact-file
Defender verification before execution. A brief real Mac virtual-display test
created a capturable 1920×1080 screen and restored the original two-display
inventory on stop. Installed daily-use apps were not replaced. The initial
parallel simulation run had three timeouts; all 48 end-to-end cases passed
when run serially. These checks do not establish physical text quality,
glass-to-glass latency, or all lock/sleep scenarios.

An encrypted synthetic check also carried all 180 frames directly over Ethernet,
with Windows initiating the connection to the Mac. The socket snapshot confirmed
the 10.99.0.x cable path; no SSH forwarding or firewall changes were used. The
source finished without needing its stdin closed, Windows returned to its
accepted ready state, and local Stop revoked that state. The final rolling
120-frame diagnostic p95 was about 58 ms (capture to Present submission,
clock-estimated), with about 1.1 ms clock uncertainty. This is not a physical
screen latency or sustained 60 FPS claim. Visible text/motion quality remains
installation acceptance work.
