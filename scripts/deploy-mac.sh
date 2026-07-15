#!/usr/bin/env bash
# Build kayiver (release), install into Kayiver.app, and sign with the STABLE
# self-signed identity so macOS keeps the Accessibility / Input-Monitoring grant
# across rebuilds. Without this signing step every fresh binary gets a new
# cdhash, macOS TCC drops the grant, and the engine hangs in ensure_permissions
# (symptom: /api/status shows "running": false, crossing to Windows dies).
set -euo pipefail
cd "$(dirname "$0")/.."

CARGO="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo"
APP="dist/Kayiver.app"
KC="$HOME/Library/Keychains/kayiver-signing.keychain-db"
HASH=02C0C88FD96EB2EF88779C4231B2CD73AB46B3AF   # "Kayiver Self-Signed"
IDENT=app.kayiver

echo "==> building release"
"$CARGO" build --release -p kayiver

echo "==> installing into $APP"
cp target/release/kayiver "$APP/Contents/MacOS/kayiver"

echo "==> signing with stable identity ($IDENT)"
security unlock-keychain -p kayiver-local "$KC"
codesign --force --deep --sign "$HASH" --keychain "$KC" --identifier "$IDENT" "$APP"
codesign -d -r- "$APP" 2>&1 | grep -i designated

echo "==> restarting app"
pkill -f "Kayiver.app/Contents/MacOS/kayiver" 2>/dev/null || true
sleep 1
open "$APP"
echo "==> done. Grant is preserved (same signature) — no re-approval needed."
