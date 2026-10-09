# Mouse crossing regression: 2026-10-09

Superseded by `MOUSE-REGRESSION-2026-10-10.md`: the user subsequently reported
source cursor motion despite the application-delivery observations below.

Status: 0.3.10-dev development candidate installed on both devices. This is not
full physical acceptance or a public fixed release. 0.3.9 remains withdrawn.

## Resulting capture behavior

One macOS session tap computes movement and decides delivery in the same
callback. Every remote motion report is consumed, including quantized zero
physical reports and programmatic position reports. Native parking still runs
on those zero reports. A local return reuses its current event and the exact
remaining movement. Native association stays enabled throughout normal
handoffs, so normal crossings do not restart the source acceleration pipeline.

Remote movement consumes the source OS's quantized event displacement. Native
parking coordinates do not contribute fractional phase or movement. Local
native coordinates retain local subpixels. This is not a claim of exact
fractional remote gain across every DPI setting.

Background cursor control is enabled on the application's WindowServer
connection; hide/show remain balanced and their errors are exposed in
`/api/status.mouse_delivery`. Capture status now snapshots navigation, native
position and control flags under the same capture mutex. Without that snapshot,
a rapid return could misleadingly combine a local native point and another
frame's remote forwarding flag.

## Confirmed defects and independent evidence

A read-only HID recording contained 1,180 physical reports. The previous
integer-plus-absolute-fraction formula disagreed with 553 of 1,179 adjacent
native position differences. For example, y=832.125 -> 831.8671875 with OS dy=0
became +0.7421875 rather than a zero quantized remote report. Unit regressions
now reject that false direction and ignore parking phase.

A separate read-only annotated-session observer saw none of 808 recorded
physical remote report identities, while independently matching 4,675 local
physical reports. Its dropped-record count was zero. This proves those recorded
remote reports did not reach Mac applications; it does not prove all possible
future scenarios.

A subsequent controlled-entry window consumed 537 physical remote reports.
The sampled source native position stayed (3840,720), control generation stayed
3, and control returned locally without recovery. A separately observed
scripted return's application coordinate matched the engine's resume position
within floating-point precision (maximum difference 2.85e-14 pixels). The entry
and scripted return are not physical acceleration acceptance tests.

A temporary two-tap experiment was removed after failing real physical input.
HID and session timestamps differed on physical reports, and rewritten event
metadata did not provide a reliable correlation contract. No delivery ledger
or second crossing path remains.

## Live preview

The editor formerly represented any remote pointer by the first visible remote
monitor's center. Receivers could also mistake the physical source for the
pointer's target machine. `/api/cursor` now uses capture's authoritative surface
position and separates physical source from target machine. Shared aliases
project into the same visible panel using their native fractions. Ambiguous
or inactive native surfaces do not produce a guessed pointer.

Backend regressions and `scripts/verify-cursor-preview.cjs` cover remote points,
receiver coordinates, hidden shared aliases, and ambiguous rectangles.

## Reproduction tools

- `scripts/observe-mac-delivery.py` is read-only and captures only mouse event
  identities by default. `--positions` also captures coordinates. It never
  posts events or reads keyboard/clipboard contents. Files are private (0600),
  exclusive-create, and bounded to 120,000 rows and 60 seconds.
- `KAYIVER_NATIVE_MOTION_TRACE` enables private bounded capture recordings with
  `captured`, `suppressed`, and `resumed` stages. Normal capture performs no
  trace file I/O. Recording is lossy under overload and limited to 120,000 rows.
- `scripts/verify-mac-capture.py` exercises synthetic delivery, zero reports,
  return coordinates and repeated queue handling on an idle known layout.
  It must not be treated as physical acceleration or visual acceptance.
  `CGCursorIsVisible` is reported as advisory information, not asserted as
  proof of visible cursor behavior.

## Source and owner matrix

A paired-process regression now runs 100 immediate roundtrips for each of
four source/shared-owner combinations (400 total). Every trip checks the
source parking position, exact return distance, forwarding state and receiver
release. Before switching the input source, it waits for all 100 ordered
releases; a transient not-driven snapshot can belong to an earlier trip and
is not proof that the network queue is empty. This is simulated OS input over
real TCP/Noise, not physical acceleration acceptance.

## Validation and remaining gates

App unit tests (35), paired simulated transport tests (38, with the release
workflow's `--test-threads=1`), and the unchanged
core geometry suite (35) passed. Both native platform builds compiled. macOS
uses the original stable signing identity; Windows runs in console session 1.
Both installed versions identify as 0.3.10-dev with protocol 16.

The unrestricted parallel simulation run passed 36/38: the scaled landing
scenario timed out, and the six-second load scenario received 46 motion
frames instead of its >200 threshold. Both passed in the release workflow's
serial run. This is a test execution limitation, not evidence that physical
movement is correct under arbitrary CPU load.

The broad acceptance matrix remains incomplete: different DPI configurations,
held drag/key transfer, both physical sources and shared owners, and hundreds
of rapid physical roundtrips. Keep the candidate and PR in development until
those gates pass. Native visual cursor visibility across foreground changes
also needs physical acceptance beyond successful hide/show API calls.

macOS bundle version keys use the numeric 0.3.10 prefix while the binary/API
retains 0.3.10-dev. Packaging, deployment and CI share the numeric conversion
helper, following Apple's CFBundleShortVersionString/CFBundleVersion rules.
See https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleshortversionstring
and https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleversion.
