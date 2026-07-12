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

# Ad-hoc signature: macOS TCC (Accessibility / Input Monitoring) tracks the
# binary by cdhash, so every rebuild needs the permissions re-approved once.
codesign --force --deep --sign - "$APP"

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
  echo "NOT: yeni derleme izinleri düşürebilir (ad-hoc imza) — Erişilebilirlik +"
  echo "     Giriş İzleme'de Kayıver'ı işaretle; grant bir kez verilince kalıcıdır."
fi
