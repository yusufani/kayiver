# Native motion investigation, 11 October 2026

Status: development candidate, physical return acceptance in progress. Not a stable release.

## Reproduced cause

The acceleration preservation change 5f4c8ab removed source cursor disassociation. Later containment restored disassociation but retained a native warp on every remote mouse report. A real PID-zero hardware recording showed device displacement (3, 5) accompanied by CGEvent displacement (474, 708) immediately after a source relocation of (473, 706). Programmatic relocation contaminated the physical report and caused false geometry transitions.

## Change

Detach and hide the source once on remote entry, at its departure position. Do not relocate the source on entry or on each remote report. Consume accelerated source OS displacement in the existing authoritative motor. On local return perform one native relocation, re-associate immediately, and remove that relocation from the first post-relocation physical report. Reports queued before relocation completion retain their own displacement and update the old coordinate origin. Both app event deltas and engine deltas use the normalized report.

The return fence uses the native event timestamp and mach absolute time converted to nanoseconds. It is not a delay or a direction lock. No acceleration multiplier, screen-specific edge exception, or general return debounce was added.

## Evidence and limits

* Forty-one native unit tests passed, including relocation exclusion, queued reports, fractional origins, immediate direction reversal and a replay of the observed queued 815-pixel return contamination.
* Forty-one paired simulation integration tests passed serially. Simulation does not establish native containment or hardware acceleration acceptance.
* Independent native-position polling during genuine remote physical mouse input: 1,482 remote samples all at (3890, 9), with physical reports continuing. This validates removal of per-report remote parking on this Mac.
* An event-location-only return experiment was rejected: app events changed location while subsequent native coordinates stayed in the old coordinate space.
* A pre-relocation timestamp experiment was rejected: queued hardware reports could arrive before the relocation took effect. The candidate fences at relocation completion and updates the old origin from those queued reports.
* A synthetic 100-roundtrip attempt overlapped real physical input and failed. It is not counted as acceptance. The fixture now documents that absolute synthetic posts cannot prove physical containment.

Remaining acceptance: independent native/logical return coherence, physical acceleration continuity in both directions, held-input handoff, Windows physical source and background application matrix. Do not publish this candidate as stable until these gates pass.

Risk: incorrect classification of queued native reports can inject relocation distance into a transition; incorrect return placement can separate click coordinates from visible cursor position. Preserve the previous signed Mac bundle and matching Windows executable together for rollback. Protocol remains 16; the receiver has no geometry ownership change in this patch.

## Follow-up: return pause, same day

The user confirmed remote entry and source containment now feel correct, but Windows-to-Mac return still loses speed. Eight genuine return records showed native positions remaining at the return target for approximately 251–264 ms before movement resumed, while accelerated physical displacement continued arriving. In seven consecutive-position runs, 139–244 real reports remained at the target. This is native post-warp suppression, not a reason to change the acceleration curve or add a machine-specific gain.

The Mac adapter now sets the default Quartz source suppression interval to zero before a local-return warp, then re-associates as before. Injector's separate event source already has a zero interval, but a native CGWarpMouseCursorPosition call has no source argument. The existing remote freeze and relocation exclusion remain. The same preparation also covers explicit local recovery warps. Its native API result is reported as mouse_delivery.suppression_error.

Forty-one native unit tests pass after this change. The signed candidate is installed. Physical post-change return timing still requires a new hardware crossing; an uncalled diagnostic value of zero is not evidence of success. No per-computer coordinates, display letters, speed multiplier or delay threshold configure this change. The deprecated Quartz API is exported by the current SDK; acceptance on other macOS versions remains a compatibility gate. See [SDL's native warp implementation](https://github.com/libsdl-org/SDL/blob/main/src/video/cocoa/SDL_cocoamouse.m) for the distinction between native warp suppression and posted-event source configuration.
