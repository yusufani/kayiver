# Mouse crossing regression: 2026-10-09

Status: development change; physical crossing acceptance remains incomplete.
Do not publish this as a fixed release or replace the installed applications.

Both installed devices were returned together from 0.3.9 to the exact saved
0.3.8 artifacts. Windows runs in interactive session 1; macOS retains its
original signature and app-process Accessibility/Input Monitoring grants.
0.3.8 also has known reported problems and is not a stable acceptance baseline.
The public 0.3.9 prerelease has been returned to draft following the report.

## Confirmed defect

A read-only HID observer recorded 1,180 real physical motion reports without
posting events, moving the pointer, or reading keyboard/clipboard content.
Of the 1,179 adjacent position differences, the previous remote reconstruction
formula disagreed on 553 reports. The old formula combined an integer OS delta
with the difference between fractional absolute cursor coordinates. Those
rounding phases are independent.

Example: y=832.125 -> 831.8671875, OS dy=0. The real movement is -0.2578125;
the previous formula instead produces +0.7421875. This can create an unintended
reverse crossing at a seam. It proves a source-motion defect, not the complete
cause of every reported jump.

Remote reports now use the source OS event displacement directly. Parking,
queued coordinates, and fractional position cannot add movement or reverse
an event. Local native position differences still retain local subpixels.
A quantized zero report remains zero; subsequent OS reports supply the
quantized displacement. This does not assert exact fractional remote gain.

The unaccepted 0.3.9 session-tap suppression change was reverted. Native
parking and return rebasing still need physical verification; reverting it
does not solve the source cursor visibility issue reported on 0.3.8.

## Validation required before deployment

- Unit regressions must reject the captured reversed-direction example and
  remain independent of native parking phase.
- Core geometry and paired simulated transport tests must pass.
- Native macOS and Windows builds must compile.
- Physical Mac -> Windows -> Mac motion must preserve direction and distance,
  including rapid reversals and held buttons; verify the actual source cursor
  stays contained and hidden while remote.
- Test source acceleration with physical events, not fabricated synthetic
  integer delta/position pairs. A synthetic delivery test alone cannot prove
  physical acceleration, cursor visibility, or queued-report correctness.

Risks remaining: quantization near a seam, native warp/report ordering, and
source cursor containment. No new public release should claim these passed
until physically observed.
