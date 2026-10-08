# Kayıver 0.3.2 — unified movement engine (prerelease)

This release replaces desktop monitor crossing patches with one deterministic
movement engine shared by macOS, Windows and simulation. Source movement is
processed in order; each segment crosses its first real monitor edge and
continues with the remaining distance. Immediate reversal has no return timer.

For example, a 100px movement starting 10px from D's left edge consumes 10px
on D and continues for 90px on A. Fractional logical coordinates survive the
handoff; OS rounding does not feed a one-pixel debt back into the motor.

## What changed

- Stable monitor identities and one visible surface for a shared physical panel.
- Actual edge segments; gaps, ambiguous links and stale shared-desktop links
  cannot create a crossing into empty space.
- One control state with session/generation/sequence/layout fencing, ordered
  handoffs and held-key/button snapshots. The receiver applies the source's
  result instead of running a second edge algorithm.
- Failed injection, disconnect and recovery shortcuts release held input and
  return the cursor to a visible local surface.
- macOS editor re-entry reuses its existing window and waits for server readiness.
- Windows launches the editor in the signed-in interactive user session with an
  isolated browser profile. Re-entry raises/restores the window and rescues it
  from a hidden or removed monitor.
- The map fits and centers as a whole; resize, zoom and background pan preserve
  relative monitor positions. Narrow headers wrap rather than squeeze controls.
- Windows shows its active connection without an empty unsupported route picker.
- Windows mirrors the canonical host layout. Save stays visible with an explicit
  disabled explanation; shared-owner switching, settings, pan and zoom remain
  available. Layout editing still belongs to the host.
- UI status polling reads cached network labels; interface scans run in a
  background worker instead of blocking the movement/network runtime.
- Optional bounded native motion recordings and a deterministic replay command.

## Validation and limits

The local acceptance run passed 35 core tests, 19 application unit tests and 33
end-to-end simulated desk tests over real TCP and Noise encryption. Coverage
includes generated geometry, 10,000 fractional roundtrips, 100 immediate network
roundtrips, both driver directions, shared ownership, taskbar seams, gaps,
monitor reorder/removal, held-input handoff, injection failure and reconnect.

Both native binaries built and both installed engines connected over the direct
cable. The Mac editor opened, panned and reset its map; the Windows engine
reported its editor visible, not minimized and on a visible monitor. Mac
Accessibility/Input Monitoring/Event Posting permissions remained granted with
the existing signing identity.

This is a prerelease because physical acceleration feel and native held-input
handoff have not had human hands-on acceptance. Programmatic UI movement does
not substitute for hardware mouse reports. Raw-input games are also unverified;
monitor frames currently apply absolute desktop positions. Queues preserve FIFO
order but remain unbounded under prolonged transport congestion.

## Compatibility and rollback

Protocol 16 requires upgrading both desktop computers together. Versions using
an older protocol refuse the new session; do not update just one computer.
Pairing keys and stable monitor selections remain in the existing configuration.

Keep a backup of both prior binaries and each machine's configuration. To roll
back, stop both engines, restore both binaries and their corresponding config
backups, then restart the two engines together. A Mac update must retain its
signing identity or macOS may require permission approval again.

The macOS archive is signed with the existing Kayiver certificate, self-signed
and not notarized. Windows remains an unsigned binary.
