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

# Sign with a stable, self-signed identity so TCC (Accessibility / Input
# Monitoring) grants PERSIST across rebuilds: the grant is keyed on the code
# signature's designated requirement (identifier + certificate), which stays
# constant as long as we sign with the same cert. Ad-hoc (`-`) changes the
# cdhash every build and would force re-approving permissions each time.
#
# The cert lives in a DEDICATED keychain with a known password (not the login
# keychain, whose password we don't have) so codesign never blocks on an
# interactive keychain prompt.
SIGN_ID="Kayiver Self-Signed"
SIGN_KC="$HOME/Library/Keychains/kayiver-signing.keychain-db"
SIGN_KC_PW="kayiver-local"

ensure_signing_identity() {
  if [[ ! -f "$SIGN_KC" ]]; then
    security create-keychain -p "$SIGN_KC_PW" "$SIGN_KC" || return 1
    security set-keychain-settings "$SIGN_KC"           # no auto-lock timeout
  fi
  security unlock-keychain -p "$SIGN_KC_PW" "$SIGN_KC" || return 1
  # Make codesign search this keychain too (keep the existing list).
  local existing; existing=$(security list-keychains -d user | sed 's/[" ]//g' | tr '\n' ' ')
  if ! echo "$existing" | grep -q "kayiver-signing"; then
    security list-keychains -d user -s "$SIGN_KC" $existing >/dev/null 2>&1
  fi
  if ! security find-certificate -c "$SIGN_ID" "$SIGN_KC" >/dev/null 2>&1; then
    echo "creating a local signing identity ($SIGN_ID)…"
    # macOS's `security import` needs a legacy-MAC PKCS#12, which the system
    # LibreSSL can't produce — use real OpenSSL if available.
    local ssl; ssl=$(command -v openssl)
    for c in /opt/homebrew/opt/openssl@3/bin/openssl /opt/homebrew/bin/openssl /usr/local/opt/openssl@3/bin/openssl; do
      if "$c" version 2>/dev/null | grep -qi "^OpenSSL"; then ssl="$c"; break; fi
    done
    local tmp; tmp=$(mktemp -d)
    "$ssl" req -x509 -newkey rsa:2048 -keyout "$tmp/k.key" -out "$tmp/k.crt" -days 3650 -nodes \
      -subj "/CN=$SIGN_ID" \
      -addext "keyUsage=critical,digitalSignature" \
      -addext "extendedKeyUsage=critical,codeSigning" \
      -addext "basicConstraints=critical,CA:FALSE" >/dev/null 2>&1
    "$ssl" pkcs12 -export -inkey "$tmp/k.key" -in "$tmp/k.crt" -out "$tmp/k.p12" \
      -passout pass:kayiver -name "$SIGN_ID" -legacy >/dev/null 2>&1
    security import "$tmp/k.p12" -k "$SIGN_KC" -P kayiver -T /usr/bin/codesign -A >/dev/null 2>&1
    rm -rf "$tmp"
  fi
  # Let Apple codesign use the key without an interactive prompt.
  security set-key-partition-list -S apple-tool:,apple: -k "$SIGN_KC_PW" "$SIGN_KC" >/dev/null 2>&1
  return 0
}

if ensure_signing_identity && codesign --force --deep --keychain "$SIGN_KC" --sign "$SIGN_ID" "$APP" 2>/dev/null; then
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
