# 0.3.17-dev — macOS modifier recovery

macOS now reads modifier state from each ordered Quartz event rather than
inverting its stored state on every flagsChanged report. For example, duplicate
Shift reports or a missed key release could leave Windows interpreting later
wheel input with Shift still held. Key and wheel events now reconcile held
modifiers first, placing any missing release before the dependent input.
Left/right modifiers remain independent and Caps Lock retains its complete pair.
The continuous pointer fast path is unchanged.

Protocol 18 is unchanged; 0.3.16-dev and 0.3.17-dev peers can communicate without
re-pairing. Both desktop packages carry the new development version. The
behavioral correction is on the macOS source; Windows input behavior is unchanged.

## Validation and limits

The source correction passed 72 application unit tests (2 ignored), 47 real
TCP/Noise two-process simulation tests and a native macOS compile check.
Regressions include duplicate flags, a release without an observed press,
missed releases, both modifier sides and remote release-before-wheel ordering.
Physical scrolling was subsequently confirmed by the user in Brave and File
Explorer. An earlier post-update trial still failed before that confirmation;
not every cause of intermittent application scrolling has been established.
This release addresses the confirmed stale-modifier defect.

## Package security

The macOS Apple-silicon bundle uses the existing self-signed identity. It is
not notarized, so first-launch approval may be required. Existing permission
retention depends on the installed signature and macOS state.

The Windows x64 development prerelease is unsigned. The release workflow scans
the exact executable with current Defender intelligence before allowing it into
a draft release. The attached windows-verification.json records the hash,
signature status, scanner host, exclusion checks, output and protection state. SHA256SUMS covers both
macOS packages, the Windows executable and that report. A static scan does not
exercise runtime behavior or guarantee future Defender/SmartScreen decisions.
Do not bypass a security warning.

Development tags ending in -dev may publish unsigned Windows artifacts only
with the exact-file verification gate passed and the report attached. Stable
tags additionally require a trusted Authenticode signature. The two unresolved
6 November incident hashes remain blocked; this release does not restore them
or claim that Microsoft has cleared those samples. No protection exclusions or
threat allowances are part of the build or installation instructions.

Keep pairing configuration and previous reviewed binaries for rollback.
Neither package automatically reinstalls SYSTEM/boot tasks removed by Defender.
The release does not promise secure-desktop, pre-logon or universal game input.
