#!/bin/bash
# Build Kayiver.app (macOS bundle). Usage:
#   packaging/macos/build-app.sh            # -> dist/Kayiver.app
#   packaging/macos/build-app.sh --install  # also copy to /Applications and
#                                           # point /opt/homebrew/bin/kayiver at it
set -euo pipefail
cd "$(dirname "$0")/../.."

export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build --release

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP=dist/Kayiver.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp target/release/kayiver "$APP/Contents/MacOS/kayiver"
cp assets/icons/Kayiver.icns "$APP/Contents/Resources/Kayiver.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Kayiver</string>
    <key>CFBundleDisplayName</key><string>Kayıver</string>
    <key>CFBundleIdentifier</key><string>app.kayiver</string>
    <key>CFBundleExecutable</key><string>kayiver</string>
    <key>CFBundleIconFile</key><string>Kayiver</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>MIT</string>
</dict>
</plist>
PLIST

# Sign with a stable, self-signed local identity so TCC (Accessibility / Input
# Monitoring) grants PERSIST across rebuilds: the grant is keyed on the code
# signature's designated requirement (identifier + certificate), which stays
# the same as long as we sign with the same cert. Ad-hoc (`-`) changes the
# cdhash every build and would force re-approving permissions each time.
SIGN_ID="Kayiver Self-Signed"
ensure_signing_identity() {
  if security find-certificate -c "$SIGN_ID" >/dev/null 2>&1; then
    return 0
  fi
  echo "creating a local signing identity ($SIGN_ID)…"
  local tmp; tmp=$(mktemp -d)
  openssl req -x509 -newkey rsa:2048 -keyout "$tmp/k.key" -out "$tmp/k.crt" -days 3650 -nodes \
    -subj "/CN=$SIGN_ID" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=critical,codeSigning" \
    -addext "basicConstraints=critical,CA:FALSE" >/dev/null 2>&1
  openssl pkcs12 -export -inkey "$tmp/k.key" -in "$tmp/k.crt" -out "$tmp/k.p12" \
    -passout pass:kayiver -name "$SIGN_ID" -legacy >/dev/null 2>&1
  # -A: let codesign use the key without a per-launch keychain prompt.
  security import "$tmp/k.p12" -k ~/Library/Keychains/login.keychain-db \
    -P kayiver -T /usr/bin/codesign -A >/dev/null 2>&1
  rm -rf "$tmp"
}

if ensure_signing_identity && codesign --force --deep --sign "$SIGN_ID" "$APP" 2>/dev/null; then
  echo "signed with stable identity: $SIGN_ID (permissions persist across rebuilds)"
else
  echo "stable signing unavailable — falling back to ad-hoc (permissions reset each build)"
  codesign --force --deep --sign - "$APP"
fi

echo "built $APP (v${VERSION})"

if [[ "${1:-}" == "--install" || "${1:-}" == "--reset-perms" ]]; then
  rm -rf /Applications/Kayiver.app
  cp -R "$APP" /Applications/Kayiver.app
  ln -sf /Applications/Kayiver.app/Contents/MacOS/kayiver /opt/homebrew/bin/kayiver

  # Register the freshly-copied bundle with LaunchServices so it's known by id.
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -f /Applications/Kayiver.app >/dev/null 2>&1 || true

  # A stale TCC entry (from an old cdhash) can hide the app from the Privacy
  # lists. We DON'T reset on every install (that would force re-granting each
  # time); pass --reset-perms explicitly if the app won't appear / won't take.
  if [[ "${1:-}" == "--reset-perms" ]]; then
    for svc in Accessibility ListenEvent PostEvent; do
      tccutil reset "$svc" app.kayiver >/dev/null 2>&1 || true
    done
    echo "TCC entries reset for app.kayiver"
  fi

  echo "installed to /Applications/Kayiver.app"
  echo "CLI: /opt/homebrew/bin/kayiver -> the app binary"
  echo "NOT: ilk kez stable imzaya geçildiyse Erişilebilirlik + Giriş İzleme'de"
  echo "     Kayıver'ı bir kez işaretle; sonraki derlemeler grant'ı korur."
fi
