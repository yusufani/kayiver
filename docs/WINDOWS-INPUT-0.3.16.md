# Windows editor recovery and pointer dispatch

This hotfix retains protocol 18 and is compatible with the installed Mac
0.3.16-dev build. Only Windows was restarted. The navigation engine, source
acceleration, ordered motion stream and shared display ownership are unchanged.

The editor opens on a visible local monitor, excluding the peer-owned shared
panel by resolved monitor identity. Chromium's actual window is checked and
relocated after launch, since an existing browser can ignore startup position.
Editor discovery explicitly enumerates Default with DESKTOP_READOBJECTS;
it does not depend on the HTTP worker's current desktop. GUI initialization
no longer attaches to the input desktop: only the windowless injection worker
follows UAC. Existing editor windows are reused before starting a user process.

The reported lag was reproduced in passive, payload-free counters. In a
10-second live sample there were 1,244 injection commands, no failed commands,
and a mean application time of 1,576 us. SendInput accounted for 1,114 us of
that time. The input actor's interval queue wait was 34,693 us for 1,207 cursor
frames; its maximum since startup reached 511,563 us. Network RTT alone did
not explain the lag.

Continuous absolute movement now uses SetCursorPos with authoritative readback.
Every position is still applied in order. Native absolute SendInput remains
mandatory on entry, after keys/buttons/wheels, after a desktop change and after
100 ms idle. This idle check chooses the dispatch method; it never delays a
movement. All held-button drags and relative/raw-input movements retain native
events on every move. This preserves Terminal's pointer restoration after
typing without paying a SendInput hook round-trip for each ordinary position.
Failed native notification remains pending rather than suppressing a retry.

Driven mouse echoes are discarded before acquiring the capture gate, with a
second check after acquisition for a concurrent ownership change. Diagnostic
GetCursorPos calls run outside the gate, since native cursor operations can
synchronize with hook callbacks. Logical status fields still share a consistent
snapshot; the native coordinate is sampled independently.

Local status adds input_queue count/mean/max/over_8ms and native input stage
timings/direct_moves. These counters contain no keys, clipboard or coordinates.
Means are cumulative; use differences of count multiplied by mean for an
interval. They do not claim a percentile or user-perceived latency.

Validation: 60 application unit tests, 43 two-process regressions, Windows
release build, and the three isolated native desktop/coordinate tests pass.
Pointer policy tests cover continuous movement, idle, typing/desktop invalidation
and failed notification. The native tests use private desktops and do not move
the user's cursor. The editor was observed visible and unminimized at
(616, 56), size 1196 by 934 on the active upper Windows screen.
The final installed executable matched SHA-256
`0b6ff14ed556b546eecc81953242df76c1fd469100126f27acd5fb50fc2bb18c`.
Final ten-second status sampling was responsive, with a visible editor and
connected Ethernet (RTT 0.98 ms), but contained no physical input commands.
Consequently the post-change latency improvement is not yet measured.

Remaining physical acceptance includes Terminal typing, UAC stationary input,
dragging and applications that rely exclusively on modern pointer hover events.
For drags/raw input the native stream is preserved, so their SendInput cost may
remain. A Windows-only rollback is available at
`/tmp/kayiver-windows-ui-20261013/rollback-windows.exe`; it is also protocol 18.
