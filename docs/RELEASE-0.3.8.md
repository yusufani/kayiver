# Kayıver 0.3.8

Mouse sharing now keeps the macOS source acceleration pipeline associated during remote control. Screen-space motion retains fractional position instead of rounding every native report before navigation. Receivers continue to apply absolute positions, avoiding a second acceleration pass.

Native cursor parking uses a real monitor's center. Queued reports keep their own accelerated displacement: a late report from the departure edge does not include the distance to the park. Fractional phase is consumed once, including immediate direction reversals. A local return also normalizes queued reports still expressed in the remote parking coordinate system, preserving their movement until native coordinates catch up. There is no time-based return lock. Native references reset when the control generation changes. Programmatic reports with no physical displacement or unaccelerated input update local references without initiating crossings; generated park reports do not advance remote navigation.

A local return reuses the current physical event with only the movement consumed on the final local surface. It does not post a second motion event or toggle mouse association during the handoff. Empty local screen edges remain under native OS control.

Validation: 105 automated tests (33 application, 35 core, 37 simulated network/end-to-end). Native macOS event-tap checks cover cursor containment and return position with synthetic motion reports. These checks do **not** establish physical mouse acceleration feel; this remains a prerelease pending hardware acceptance.

Protocol 16 is unchanged. Update both devices together; stable macOS signing is preserved. Roll back both devices to the matching 0.3.6 packages if necessary. Native diagnostics are opt-in, bounded, and disabled in normal startup; keyboard and screen contents are not recorded.
