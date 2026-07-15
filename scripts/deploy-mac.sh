#!/usr/bin/env bash
# Build kayiver (release), install into Kayiver.app, and sign with the STABLE
# self-signed identity so macOS keeps the Accessibility / Input-Monitoring grant
# across rebuilds. Without this signing step every fresh binary gets a new
# cdhash, macOS TCC drops the grant, and the engine hangs in ensure_permissions
# (symptom: /api/status shows "running": false, crossing to Windows dies).
set -euo pipefail
cd "$(dirname "$0")/.."

# The ~/.cargo/bin shims are missing on this box; use the toolchain bin directly
# and put it on PATH so cargo can find rustc.
TOOLCHAIN_BIN="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
export PATH="$TOOLCHAIN_BIN:$PATH"
CARGO="$TOOLCHAIN_BIN/cargo"
# The app the system actually LAUNCHES is /Applications/Kayiver.app; dist is the
# repo copy. Sign both (identical signature) so they never diverge — Launch
# Services resolves the app.kayiver bundle id and may run either one.
APPS=("/Applications/Kayiver.app" "dist/Kayiver.app")
RUN_APP="/Applications/Kayiver.app"
KC="$HOME/Library/Keychains/kayiver-signing.keychain-db"
HASH=02C0C88FD96EB2EF88779C4231B2CD73AB46B3AF   # "Kayiver Self-Signed"
IDENT=app.kayiver

echo "==> building release"
"$CARGO" build --release -p kayiver

echo "==> stopping running app"
pkill -f "Kayiver.app/Contents/MacOS/kayiver" 2>/dev/null || true
sleep 1

security unlock-keychain -p kayiver-local "$KC"
for APP in "${APPS[@]}"; do
  [ -d "$APP" ] || continue
  echo "==> installing + signing $APP"
  cp target/release/kayiver "$APP/Contents/MacOS/kayiver"
  codesign --force --deep --sign "$HASH" --keychain "$KC" --identifier "$IDENT" "$APP"
done
codesign -d -r- "$RUN_APP" 2>&1 | grep -i designated

echo "==> launching $RUN_APP"
open "$RUN_APP"
echo "==> done. Grant is preserved (same signature) — no re-approval needed."
