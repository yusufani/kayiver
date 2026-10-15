# Testing

## The virtual desk (`--features sim`)

Every bug class that ever bit on the real desk has a regression test that
runs on a **virtual two-machine desk** — no hardware, no displays, no OS
permissions:

```sh
cargo test --features sim -p kayiver
```

Each scenario spawns two real `kayiver run` processes — a host and a client,
each with its own `KAYIVER_CONFIG_DIR`, so each behaves like its own machine —
talking over real TCP with the real Noise handshake. Only the OS layer is
swapped for a scriptable simulator ([`platform/sim.rs`](../apps/kayiver/src/platform/sim.rs)):
virtual monitors, a virtual cursor, and recorded injection, driven through a
JSON-lines control socket.

```
KAYIVER_SIM_MONITORS="0,0,2560,1440;2560,0,2560,1440"   initial displays
KAYIVER_SIM_CTL=27101                                   control port
KAYIVER_CONFIG_DIR=/tmp/simhost                         isolated config
```

Control ops (one JSON object per line): `warp` (move the "physical" cursor —
the common capture engine reacts), `set_monitors` (unplug / re-anchor displays at
runtime), `edge` (hit an armed portal edge), `input_move`/`input_key`
(forwarded traffic), `hotkey`, `state`, `injected` (drain everything the
engine injected, with coordinates).

### Scenarios ([`tests/sim_e2e.rs`](../apps/kayiver/tests/sim_e2e.rs))

| Test | The real-desk bug it guards against |
|---|---|
| `diagonal_cross_lands_at_entry_height` | diagonal entry read as a TOP entry → cursor dumped in the peer's corner |
| `primary_display_switch_rederives_peer_rect` | Windows primary switch re-anchored every rect → crossings landed on the wrong monitor |
| `moving_a_local_monitor_refreshes_the_shared_crossing_boundary` | Moving a laptop screen left capture using the old desktop boundary → the crossing above the shared panel stayed closed |
| `crossing_to_the_windows_screen_above_survives_a_primary_switch` | Crossing onto the separate Windows screen must stay on that screen and return through the shared panel seam after Windows changes its primary display |
| `vanished_panel_never_reanchors_to_same_size_screen` | panel input switched away → size-match glued the peer's screens onto A |
| `owner_survives_host_restart` | deploy reset ownership → notice overlay covered the client's fullscreen game |
| `no_nonce_desync_under_load_and_geometry_churn` | timer racing a half-read frame → Noise nonce desync → `decrypt error` loop |
| `return_preserves_movement_already_in_flight` | delayed return notification must carry movement already sent over the network |
| `return_preserves_motion_still_in_the_native_capture_queue` | return notification overtakes swallowed native motion waiting in the router queue |
| `windows_owned_panel_can_return_to_a_and_reenter_without_bouncing` | repeated A → Windows-owned panel → A crossings must keep inward motion on the panel |
| `a_shared_entry_after_disconnect_does_not_freeze_capture` | disconnect racing synchronous shared entry must restore local capture |

The simulator cannot verify macOS cursor association, event suppression, or
physical pointer acceleration. After changing native capture, test a fast
shared-panel round trip with the same physical mouse in both directions.

The suite runs in seconds and the scenarios are independent (unique ports and
config dirs), so they parallelize under plain `cargo test`.

### Adding a scenario

1. Reproduce the bug's *shape* in sim terms: which monitors, who owns the
   panel, what motion or geometry change.
2. Use the helpers in `sim_e2e.rs` (`desk`, `give_panel_to_client`,
   `cross_diagonally`, `wait_until`) — a scenario is typically ~20 lines.
3. Assert on what the OTHER machine observed (`injected`) or on persisted
   config — never on internal logs alone.

## Unit tests

Editor device/placement checks (no desktop input injection):

```sh
node scripts/verify-device-layout.cjs
node scripts/verify-cursor-preview.cjs
```

These exercise distant free drops, proximity snapping, unknown device names,
portrait/ultrawide ratios, viewport pan, mixed-scale shared aliases and cursor
projection. The device design browser preview was checked at desktop and 360px
widths. Native popup placement, DPI and focus still require platform acceptance.

```sh
cargo test            # geometry/logic unit tests (fast, no processes)
```

The current crossing solver is `Topology::advance` in `kayiver-core::motion`.
Legacy geometry helpers retain regression coverage.

## Unified motion contract (0.3.0)

Run all three gates before packaging:

```sh
cargo test -p kayiver-core
cargo test -p kayiver --features sim --bin kayiver
cargo test -p kayiver --features sim --test sim_e2e -- --test-threads=1
```

Core tests cover 2,000 generated three-surface layouts, 10,000 fractional
roundtrips, gaps, corners, ambiguity, monitor order, and stale generations.
Navigation tests cover clipped native endpoints, held state, immediate reversal,
and passive physical input. Real TCP/Noise simulations include 100 immediate
roundtrips, two-way drag/modifier transfer, failed injection recovery, and a
programmatic warp that cannot start a crossing.

Native recordings are opt-in: launch the app with `KAYIVER_MOTION_TRACE` set to
an absolute JSONL path. At most 10,000 self-contained movement samples are
recorded, with a bounded background writer; full queues omit samples and log a
warning instead of delaying input. No key/button values, clipboard, pairing
secrets, or wall-clock timestamps are recorded. Monitor identities/geometry are
included; treat the recording as local diagnostic data. Unix files are created
with mode 0600.

```sh
kayiver replay-motion /absolute/path/motion.jsonl
```

Replay computes every recorded report again and checks path, wall decision,
destination, and fractional coordinates. It never injects real input. A replay
pass confirms determinism for the captured reports; it does not prove physical
pointer acceleration or real application drag behavior. Perform native desktop,
taskbar, rescue, startup/reopen, and source-mouse acceptance separately on both
platforms. Secure desktop and raw-input games are separate platform gates.

Windows Quick Share regressions: run the native test binary with filter
`quickshare_windows` and `--test-threads=1` in the signed-in console session.
The popup lifecycle test creates only its own window and checks that showing
it preserves foreground focus, fits the monitor work area, leaves the gap
click-through, hides on dismissal, and leaks no GDI objects over 300 paints.
State tests cover duplicate/stale offers and hover-paused offer/completion
expiry. Set `KAYIVER_POPUP_RENDER_DIR` to export synthetic GDI fixtures for
file, URL, receiving, success, error and long names at 100%, 125% and 200%.
These are generated render fixtures, not desktop screenshots or proof of
physical copy/crossing smoothness. The SSH service session is insufficient
for window visibility/focus acceptance. Simulation regressions also cover
accepting a file after disconnect and receiving a file during slow disk I/O.

Device presentation regression: exercise 320×568, 480×640 and 957×664
windows with long adapter labels. Header controls must wrap without horizontal
page overflow; the narrow inspector must retain all fields through page scroll.
At reset zoom, screen bodies must fit the canvas without covering its zoom
controls. Device feet must not intersect any other visible screen (including
sideways laptop keyboard overhang beside taller monitors). Compact shared
screens retain owner letters without a badge obscuring them. Selection borders
stay inside the screen rectangle. Check both Precision and Gallery.

Image clipboard (0.3.14-dev, protocol 17): run
`cargo test -p kayiver --features sim --test sim_e2e image_clipboard_is_bidirectional_chunked_and_never_echoes`.
This checks incompressible RGBA across multiple encrypted frames, both directions,
image→text→same-image replacement, alpha preservation and no echo. Unit tests
reject oversized, incomplete/out-of-order chunks and invalid PNG data. Native
acceptance must confirm an actual Windows bitmap arrives as an image on macOS.
`/api/status.clipboard` reports only counters, dimensions and errors; no image
or clipboard text is exposed there. `POST /api/clipboard/sync` explicitly resends
current clipboard content; startup alone never transmits an old clipboard.

### Windows UAC input desktop

The Windows injector owns a windowless, hook-free worker. Every motion,
button, wheel and key command follows the active input desktop before
injection. The GUI/runtime thread must not be rebound: Windows rejects
SetThreadDesktop when that thread owns windows or hooks. Opened desktop
handles are closed after switching away, and worker shutdown releases held
inputs before restoring its original desktop.

`secure_desktop_tests::gui_thread_cannot_switch_but_windowless_injection_thread_can`
creates an invisible window and isolated desktops; it verifies the GUI
constraint and 200 successful switches on a clean thread without moving
any real cursor or changing the visible desktop. Run this test on Windows.
Physical acceptance still requires a real UAC prompt: with the Mac as input
source, move and click on the consent dialog, dismiss it, and immediately
move/click on the regular desktop. Do not automatically approve elevated
applications as part of this test. Pointer geometry, motion units and the
network protocol are unchanged.

### Windows Terminal pointer visibility

Absolute CursorFrames emit SendInput MOVE|ABSOLUTE|VIRTUALDESK before the
exact SetCursorPos landing. This updates modern pointer input consumers
(e.g. Terminal's WM_POINTERUPDATE visibility handling) without applying
Windows pointer acceleration a second time. Wake activity uses the power
manager and monitor-power message only, never fake Shift/F24 keys or
relative movement pulses. `absolute_pointer_coordinates_preserve_pixels_and_negative_origins`
checks every pixel for several virtual desktops, negative origins and edge
clamping. Physical acceptance: type in Windows Terminal, then move the Mac
mouse; the pointer should reappear and remain visible during motion. Also
verify UAC interaction and screen wake from monitor sleep, since wake
hardware behavior varies by display driver.

Repeated Windows input retains the opened handle for the current desktop;
only a real desktop-name change calls SetThreadDesktop. The
`repeated_probes_keep_the_existing_desktop_binding` test verifies that
1,000 probes keep the same bound handle on an isolated desktop. Its elapsed time is an isolated backend measurement,
not end-to-end mouse latency or a physical acceptance test.

### Ordered Windows injection and latency counters

Windows input commands use a typed, bounded FIFO and one reusable completion
slot. No command closure or reply channel is allocated per motion. One caller
waits for exactly its command result; worker shutdown wakes it with failure,
and shutdown waits for native pressed-input cleanup. The panic-wakeup test
applies to unwinding test builds; production uses the existing panic=abort
policy and relies on peer-disconnect recovery if the process aborts. Tests exercise 10,000
reversing commands, failure wakeup and cleanup. Run the ignored
`benchmark_reusable_completion_against_per_command_reply_channel` explicitly
for a same-machine transport comparison. On the development Windows machine,
five-trial median for 10,000 commands was 15,017 us versus 39,635 us for the
previous per-request reply-channel path. This excludes native injection and
network latency and is not a portable performance guarantee.

Windows `/api/status.injection` exposes command count, failed calls, mean/max
worker round-trip microseconds and calls over 2 ms. It contains no input
payload, keys, clipboard content or window titles. Compare counter deltas
while physically using the mouse; empty counters are not acceptance evidence.

Encrypted sessions reuse plaintext/ciphertext frame buffers and write the
length prefix with ciphertext in one socket call. Protocol 17 and the bytes
on the wire are unchanged. The legacy-framing interoperability test exercises
1,000 alternating movement/bulk messages and checks stable buffer capacity
after warmup; an oversized message is rejected without consuming a nonce.
Oversized plaintext frames are rejected before any socket write as well.

The Windows absolute move path first uses SendInput and reads the actual
cursor once. SetCursorPos is a fallback when that readback differs from the
target, or when relative/raw input needs an exact pin. Capture rebasing uses
that authoritative readback, avoiding a duplicate GetCursorPos call.
`injection.desktop_mean_us` and `native_mean_us` separate desktop tracking
from native event application; `position_fallbacks` counts absolute moves
that required the additional exact warp. Monitor origins, rounding and
clipping must still satisfy the same cursor-landing contract.

### File production backpressure

All senders for a peer share one file-frame permit. The permit is owned by
the queued frame and stays alive until the serial encrypted writer finishes
sending it. Abort/disconnect closes the budget and wakes blocked producers;
cancelling an awaiting producer does not consume a slot. Control events use
the ordinary FIFO and never wait for a file permit. File and image producers
now send 8 KiB payload chunks; image receivers still accept the original
32 KiB chunks from protocol-17 peers.

Tests verify +100/-100 input order while a file producer is blocked, bounded
file production, cancellation, disconnect failure, exact file bytes/offsets,
and empty-file EOF. This bounds the application queue, not TCP's kernel send
buffer or the peer's disk-processing time. Slow-link/slow-receiver acceptance
must measure those independently; do not claim this alone eliminates all
motion latency during transfer.

Run `encrypted_socket_probe_reports_bulk_already_ahead_of_motion` explicitly
with `--ignored --nocapture` to reproduce the remaining kernel-buffer limit
using real TCP and Noise. It pauses the receiver while four 8 KiB frames are
sent, then queues +100/-100 motion and verifies the 32 KiB already ahead of
those events. This exploratory probe reports a limitation and should become
an acceptance regression when receiver-aware flow control is implemented.

### Protocol 18 receiver-aware data credits

See PERFORMANCE-0.3.16.md. The sender-only 0.3.15 budget has been replaced by
one receiver-consumed image/file payload credit per connection. Control never
waits for it. `receiver_credit_bounds_real_tcp_bulk_ahead_of_motion` is the
real TCP/Noise acceptance regression. Tests also cover stale/duplicate receipts,
cancellation closing uncertain data credit while input continues, disconnect,
peer/epoch file acceptance, invalid offsets and EOF before disk creation.

`slow_file_disk_does_not_block_motion_or_corrupt_transfer` now deliberately
blocks the file worker for 800 ms, waits until that stall is active, then requires
40 reversing inputs to arrive within 500 ms while disk work is still active.
The simulated download directory is confined to each test's config directory;
normal user Downloads is never used by these test processes. This makes the
old synchronous-reader disk path fail rather than merely confirming eventual
file completion. Native physical acceptance remains a separate gate.
