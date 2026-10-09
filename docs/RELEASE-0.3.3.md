# Kayıver 0.3.3

An unconnected local screen edge now leaves native mouse movement entirely to the operating system. The previous build could suppress that movement and warp a stale logical position to a corner, including at the bottom of C.

Local wall reports resynchronize to the native cursor. A clipped first report establishes a reference without triggering recovery. Disconnected tablet edges do not intercept movement. Local topology is installed at startup even before a peer connects.

Regression coverage includes all four empty edges, repeated clipped motion, fast return and re-entry, held input, queued motion, display changes and reconnection. Physical mouse acceptance remains to be confirmed on the installed devices.

Protocol 16, compatible with 0.3.0–0.3.3. Both devices should use 0.3.3 for this correction. Stable macOS signing is retained. To roll back, restore both devices together to the saved matching packages.
