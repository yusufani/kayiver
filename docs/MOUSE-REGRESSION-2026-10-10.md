# Source cursor containment: 2026-10-10

The previous 0.3.10-dev candidate consumed remote input at the session tap,
but the user still reported the source Mac cursor moving while Windows was
controlled. Application delivery suppression did not establish physical
cursor containment. The replacement candidate is 0.3.11-dev and remains development-only.

## Implementation

Retain one session capture/geometry decision. On remote entry, disconnect
physical mouse movement from the Mac cursor with
`CGAssociateMouseAndMouseCursorPosition(false)`. Reconnect on local return,
reuse the current physical return event and rebase its movement reference.
Remote reports consume OS event displacement rather than frozen/parked
absolute coordinates. There is no return timer or another movement algorithm.

The requested association state and native result code are exposed in
`/api/status.mouse_delivery`. `cursor_detached_requested` is the accepted API
request, not independent proof of physical/visual behavior. Hide/show remain
balanced; association is changed at ownership transitions, not per report.
Native visual-state changes are serialized. A failed containment operation
fences queued frames, restores local control and reports its actual reason;
no remote cursor frame is emitted for the failed transition.

Incoming control is admitted under the same capture motion gate using the
navigation owner, rather than the asynchronous router focus. A queued source
crossing cannot be overridden by a competing incoming frame. Delayed rejection
messages must match the active claim's sequence range, session, generation,
layout revision and remote target. Recovery requests carry their generation
and are emitted once; old queued recovery cannot warp a later control claim.

Connected Mac display inventory now uses `CGGetOnlineDisplayList` consistently
for UUIDs, rectangles, built-in flags and display enable/disable indices.
During live display sleep `CGGetActiveDisplayList` returned zero panels while
online enumeration returned all three, unmirrored, with their original
rectangles. Active-only enumeration had destroyed the layout and substituted
one main-display rectangle. The installed candidate retains three panels,
6632-by-1440 desktop bounds, four physical/shared surfaces and six seams while
all displays are asleep. True disconnection still removes the actual UUID.

## Rejected experiment and interpretation

A single HID tap that consumed remote events passed 100 scripted roundtrips,
but a 2,000-report absolute synthetic stress stream produced 618 non-parking
samples out of 6,882 native reads, up to 25 pixels from the parking point.
That experiment was removed. Earlier application-delivery observations cannot
be promoted into a claim that the rendered/native cursor never moves.

Absolute synthetic posting is not hardware input. Apple's open IOHIDSystem
source explicitly permits externally requested cursor position changes even
when normal cursor tracking is disconnected (`cursorCoupled || external`).
Thus an absolute CGEventPost storm cannot establish physical acceleration or
prove that normal hardware tracking ignores the disconnect request. A virtual
HID device could not be created on this host; no device reports were sent and
no new permissions or entitlements were requested.

Primary references:
- https://developer.apple.com/library/archive/documentation/GraphicsImaging/Conceptual/QuartzDisplayServicesConceptual/Articles/MouseCursor.html
- https://github.com/apple-oss-distributions/IOHIDFamily/blob/main/IOHIDSystem/IOHIDSystem.cpp

## Current validation

The session/disconnect prototype passed 100 initial and then 500 additional
native scripted roundtrips between the actual paired machines. The 500-trip
check asserted successful disconnect on every entry, successful reconnect on
every return, stable sampled native parking, exactly one delivered return
report and no remote markers at the independent annotated-session observer.
No physical hardware reports overlapped this fixture, so it is not a physical
acceleration/rendering acceptance result. Association/hide/show/warp errors
were zero after the run.

The installed 0.3.11-dev pair then passed another 100 native scripted
roundtrips with no remote marker delivery, stable sampled parking and restored
local association. No physical hardware acceptance is inferred.

All 41 paired simulated transport tests passed in the release workflow's
serial configuration, including a source containment failure that sends no
cursor frame to the peer and a subsequent successful crossing. The core
geometry implementation and protocol 16 are unchanged. All 39 native app unit
tests passed. Both release targets built successfully; Windows runs in console
session 1, Mac permissions remain granted, and both report 0.3.11-dev with a
live paired connection. The primary-switch fixture now waits for matching
installed topology snapshots, not merely the earlier config-file write.

## Windows source reference and protected release

A passive WH_MOUSE_LL observer was launched with the interactive user's token
and environment in session 1. During an overlap with scripted peer cursor
updates it observed six injected movement notifications and zero unflagged
movement notifications. A prior session-0 run was discarded. The observer
never consumed events, logged only counters/session ID, and was removed after
measurement. This confirms the observed SetCursorPos path is skipped by the
physical capture hook; it does not establish physical Windows motion behavior.

Code inspection found that this skipped path also left the physical capture
position reference at its pre-control location. Windows capture now explicitly
rebases after native injection, warping and before local capture is enabled.
Parking references use the actual native readback. Shared adapter tests assert
that negative-origin peer positioning and hidden-panel parking never become
the next physical movement, and that parked reports preserve immediate reversals.

Receiver release now keeps capture protected through held-input release and
hidden-panel parking, then rebases the adapter and publishes Local under the
motion gate. A paired regression records protection at the parking call and
asserts it completes before capture is re-enabled. OS calls inside this gate
are bounded cursor read/warp operations; there is no network wait or timer.

## Acceptance limits

Physical acceleration/DPI, actual cursor rendering across foreground changes,
and held input on both real sources remain unverified for this change. Keep
paired packages and PR in development. Synthetic/native delivery checks and
simulation have separate scopes and cannot substitute for those gates.

The previous matched signed 0.3.10-dev pair is preserved for rollback; it also
has the reported source cursor defect, so it is not designated stable.
