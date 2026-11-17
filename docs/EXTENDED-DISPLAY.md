# Experimental Mac extended display

## Installation status — 16 November 2026

The Mac preview is now installed as `0.3.17-dev`, signed by the same certificate
and designated requirement as the previous application. It was built by the
existing signing workflow, without recovering or exporting the local keychain
password. The footer **Extra display** option is available.

A first, monolithic Windows upgrade passed a static Defender scan, then Defender
blocked the running application as `Trojan:Win32/Bearfoos.A!ml` and removed its
startup task. The exact affected artifact is blocked in
`scripts/windows-blocked-artifacts.json`; see `WINDOWS-SECURITY.md`.

That disputed KVM executable remains blocked. Its detection has not been
resolved or declared a false positive. Windows input sharing was restored to
the independently published `0.3.16-dev` executable and is not replaced for
the display preview.

### Receive-only companion

`kayiver-display-receiver` is an independently built entry point for Windows
that contains no KVM engine, clipboard service, input hooks, key polling or
input injection. It shares the authenticated display transport/decoder with
the integrated application, reads the existing pairing, and accepts only the
receiver role. It never provides/captures a desktop. Native PE imports were
checked for absence of `SendInput`, `SetWindowsHookExW`, `GetAsyncKeyState`,
clipboard access, session impersonation and process-memory writes. This is
functional isolation for screen-only operation, not a clearance of the earlier
detected sample or a guarantee of antivirus acceptance.

Build with `cargo build --release -p kayiver --bin kayiver-display-receiver
--target x86_64-pc-windows-gnu`. `--list` reports paired peers and stable monitor
identities. Explicit `--peer NAME --monitor-id ID` launch accepts that endpoint
only for the lifetime of the process. A loopback-only instance guard prevents
two companion viewers competing for the panel; there is no control server.

`scripts/install-display-receiver.ps1 -Path EXE -Peer NAME -MonitorId ID` scans
the original and installed bytes, preserves the input executable, and creates
a limited-user interactive task with **no triggers** plus a **Kayiver Extra
Display** Start Menu shortcut. Launching that shortcut explicitly starts
receiver readiness; Windows login does not. Mac Stop leaves the receiver ready;
Windows Escape/Stop closes its accepted session. Close or restart the companion
to revoke a waiting acceptance. There is no automatic stream restart.

The currently installed companion SHA-256 is
`d59134ce718efec64de2baa27673b809e6799392223de76a28321e41ab7b76a3`.
Exact-file Defender checks passed at staging and installation; protection is
still enabled and the original KVM is unchanged. The companion uses the active
KVM cable route when available, with saved peer addresses as a screen-only
fallback. Parent exit stops its children.

The initial no-image acceptance failure was traced to the selected BenQ
reporting standby while the software stream was healthy. Resetting the Windows
display idle timer, showing the viewer, holding a continuous playback request,
and waking only the selected panel produced an on reading past the five-minute
idle timeout. On 17 November the user confirmed visible output and typed `abc`
into a temporary Mac text field placed on C using their physical Mac mouse and
keyboard. Cursor telemetry located those samples on the virtual Mac surface
with control local, forwarding false, and no Windows focus handoff. This proves
that interaction path; it does not prove all long-running failure scenarios or
physical color/latency acceptance.

Automatic D3D video enhancement was responsible for excessive brightness in
the synthetic color fixture. Disabling it reduced sampled patch-interior error
against software decoding from a maximum of 79 to 1 channel value. This does
not establish physical panel calibration or user acceptance of the live image.

The signed Mac preview now captures NV12 and changes the ScreenCaptureKit
capture rate with the adaptive encoder target. In a verified 15-second motion
probe on the virtual screen over the cable, 379 frames were presented. Native
capture/encoding averaged 2.26% of one CPU core and transport 0.72%; the Mac has
12 logical cores. Global WindowServer usage averaged 43.04% of one core and
includes all displays and other applications, so it cannot be excluded when
assessing overall Mac load. Estimated rolling software p95 varied about
77–91 ms and ended at 84 ms. Sustained 60 fps, the under-80-ms target, and
physical scanout latency are not established. A later visible-first startup test
on the same cable advanced 817 frames in roughly 15 seconds at the 60 fps target
(about 54 presented frames/s), ending with rolling software p95 10.135 ms.
Capture/encode averaged 5.42% of one core, transport 1.60%, and global
WindowServer 38.62%. These are separate short samples under different running
conditions, not a controlled before/after comparison. Earlier motion probes without
verified window placement must not be used as before/after CPU evidence.

Mac Stop removed the virtual display and restored the two physical surfaces
in under a second in an earlier check. Full sleep/lock/cable-loss and
long-running visual acceptance remain separate checks; the default remains
experimental and off after app restart.

Use a Windows-connected monitor as an additional Mac desktop. For example,
drag a Safari window from a physical Mac monitor onto the added desktop:
Safari, the pointer and keyboard input all remain on the Mac. The Windows
application is a display receiver, not a remote-input endpoint.

This feature is **off by default**, creates no startup task, installs no display
driver, and never resumes a stream automatically. First-version support is
one Mac → Windows monitor, native even pixel dimensions up to 2560×1440
(portrait equivalents included), SDR, up to 60 frames/second, without audio.

## Positioning follow-up — 17 November 2026

Signed preview commit `18716cf` (workflow `44033907313`) was installed with the
same Mac designated requirement. Dragging C above B in the actual editor moved
the actual macOS virtual display from `(5120, 0)` to `(2781, -1080)` while
preserving both physical Mac displays at `(0, 0)` and `(2560, 0)`. The source
and Windows receiver both reported the new rectangle and continued streaming
over `10.99.0.2 → 10.99.0.1`. The rendered editor showed C touching B's top edge,
C identified as a Mac extended display, Save enabled for that position, and no
shared-panel suggestion. Overlapping and out-of-range position API calls were
rejected without moving the display or ending capture. The source's five
unit tests, companion's six tests, editor syntax check and editor overlap/
coordinate regressions passed. Positions remain scoped to the current stream;
this does not add saved desk profiles or claim physical latency/color acceptance.

## Content latency and connection balloons — 17 November 2026

The currently installed receive-only companion uses flip-sequential presentation
with a waitable swap chain and maximum frame latency of one when supported.
It reacquires the output surface after pacing and uses `Present(0, 0)` without
the tearing flag; unsupported systems retain the old blt/vsync path. The live
receiver log confirmed the flip path on C. Synthetic decoding completed and
476 patch interiors matched the software reference within one channel value.
The same renderer source was retained through the final routing/logging build.

The receiver marks its own viewer fullscreen through `ITaskbarList2` and clears
only Kayiver's legacy connection balloon via an empty `NIF_INFO` for the known
app tray identity. Live logs reported both marking and balloon clearing
accepted. That legacy dismissal did not remove the user's visible banner:
Windows had converted it to a modern toast. A later app-scoped history removal
changed the known Kayiver notification count from one to zero, and the user
confirmed it disappeared. This does not prove that every third-party toast
will be suppressed or change global settings. The KVM tray source also
omits connection/lost balloons while an actual visible receiver window exists;
that change is checked for Windows but is **not** deployed over the published
0.3.16 KVM binary. The receive-only companion remains isolated from input hooks.

A live input route now takes precedence over saved Wi-Fi and stale manager
addresses while waiting for the Mac source. Screen-only operation without a
live input route retains saved-address fallbacks. The bounded optional HTTP
hint accepts only a complete parsed matching-peer JSON body, even if HTTP close
is delayed. A regression covers live cable preference and independent saved
fallbacks. The final live media connection was verified as
`10.99.0.2 → 10.99.0.1:24822`.

A verified 15-second moving-pattern window on the virtual surface advanced at
least 747 frames at the 60 fps target, ending with estimated software p95
10.402 ms. Capture/encode averaged 4.92% of one core and transport 1.34%
(about 0.52% of the Mac's 12-core total capacity). Global WindowServer was
52.54% of one core during this separate sample and includes other display/app
activity; this is not a controlled before/after CPU or scanout-latency result.
The user's physical A/B and C panels run at 144 Hz, while the virtual source
remains 60 Hz. Flip presentation does not remove that cadence difference or
prove a physical content-latency improvement. No 120/144 fps mode was added.
The companion's seven tests passed and the main Windows source checked.

## FPS/quality controls and modern banner guard — 17 November 2026

Signed Mac preview `919d164` (workflow `44044117741`) is installed with the same
designated requirement and existing input permissions. The selected-screen inspector's **Picture settings** section provides automatic 60/30 fps, fixed-target 30 fps, and
fixed-target 60 fps, plus Economy / Balanced / High quality. Their bitrate
budgets for 1080p / 1440p are 12/20, 20/35, and 30/45 Mbps respectively.
Resolution stays native; no 120/144 fps, audio, or lossless layer was added.
Preferences persist under `extra_display` in the ordinary config. Old configs
still default to adaptive/balanced without enabling a display or auto-start.
Invalid FPS/quality values are rejected before saving. Initial 30 fps configures
both ScreenCaptureKit and VideoToolbox at 30, rather than only dropping frames.
Only the automatic profile uses latency-driven 60↔30 adaptation.

Save during a streaming session explicitly restarts the extra display and
restores its screen position; Mac app windows can move during that interruption.
Starting a source also saves the current selector choices first. Live browser
UI testing selected 30 fps / High, clicked Save & restart, and verified a new
hardware stream at 30 fps and 30 Mbps with C at `(2781, -1080)`. It then restored
Automatic / Balanced through the same UI, obtaining a 60 fps target / 20 Mbps
stream with the same position. The native app panel displays those controls.
A screenshot is retained under the ignored settings CI artifact directory.

The receive-only companion now includes a modern notification guard. It maps
only NotifyIconSettings entries whose executable matches our installed KVM
path to their generated notification identity. While streaming, it checks at
most twice per second and clears only a sender whose entire history consists
of the known Kayiver connected/disconnected titles. Mixed/unknown titles and
other app identities are left alone. It changes no registry value or global
notification policy and requests no notification-listener permission. A legacy
KVM may briefly show a banner before this bridge clears it; the future KVM tray
source also avoids generating banners while a visible receiver exists.

A controlled own-app toast probe observed history counts 0 → 1 → 0 within two
seconds, with the receiver diagnostic confirming it cleared one connection
banner. The user had already confirmed the earlier targeted removal worked.
The final companion passed staging and installed-path Defender checks; runtime
protection remains enabled and no active threat was reported. The published
0.3.16 KVM binary/task remains unchanged. Validation passed: nine companion
tests, seven source/display tests, config compatibility/roundtrip checks,
editor regression and syntax checks, live UI apply/restart, invalid-preference
API rejection, cable routing, and the modern banner probe.

## Inspector follow-up — 17 November 2026

Mac preview `b7e963b` (successful signed workflow `44050650566`) replaces the
extra-display modal with a section in the existing right-hand screen inspector.
Selecting C reveals FPS, quality, apply/restart, and stop controls there. The
footer Extra display action selects the active virtual screen and focuses that
same inspector. Physical display details and explanatory text are collapsible;
ordinary A/B selection retains its screen information. There is no extra-display
`dialog`, modal backdrop, or Close button to get trapped below the viewport.
Actions remain sticky in the inspector's own scroll area. The source also
explicitly hides the Mac-managed preferences action on Windows receiver editors;
that receiver-only visibility refinement does not change the installed Mac UI.

Read-only preview testing at normal size and 1000×560 confirmed no open modal,
no horizontal page overflow, controls inside `deviceInspector`, the stop button
within the visible inspector bounds, and an unapplied FPS value surviving
polling/refit. Selecting A hides the display controls. Editor syntax and existing
regressions passed. The installed native Mac UI was then visually checked: C
selected, FPS/quality and both actions in the right panel, canvas unobstructed.
The signed update retained the existing preferences and restored C's native
position; no Windows binary was replaced for this UI-only update. Proof is in
the ignored `dist/extra-display-sidebar-ci-44050650566/sidebar-proof.jpg` artifact.

The preferences apply flow now keeps the selected surface and inspector while
the temporary display is recreated, then waits for its origin to be restored
before completing. Polling does not overwrite an unapplied selector choice.

## Use

The Mac needs a build containing this feature and an existing pairing. Windows
can use either a compatible integrated build or the receive-only companion.
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
The added Mac surface starts to the right of the current Mac desktop. Drag its
tile in the Mac editor to apply the temporary surface's real macOS position;
Save also applies that position while streaming. For example, placing C against
B's top edge moves the actual virtual display above B, so the pointer crosses
that edge without handing control to Windows. Existing physical Mac screens
are preserved; rearranging those still requires stopping the extra display.
Virtual display positions are session-only; saved profiles remain deferred.
Origin changes keep capture alive and update the source/receiver rectangles
and cleanup region by stable display identity. Resolution changes still stop
the stream. Hidden presentation endpoints and virtual surfaces are excluded
from physical shared-panel suggestions; eligible visible physical screens must
actually overlap before a suggestion appears.

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
