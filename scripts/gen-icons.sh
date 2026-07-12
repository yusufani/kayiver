#!/bin/bash
# Generate all app icons from the SVG masters in assets/logo/.
# Requires: Google Chrome (rasterizing), iconutil (icns), python3 + PIL (ico).
set -euo pipefail
cd "$(dirname "$0")/.."

CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
OUT=assets/icons
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

render() { # render <svg> <size> <out.png>
  # Wrap the SVG in a page that pins it exactly to the viewport; a bare SVG
  # URL renders at the document's default size and gets cropped.
  local wrapper="$TMP/wrap.html"
  printf '<!doctype html><style>html,body{margin:0;padding:0;overflow:hidden}img{display:block;width:100vw;height:100vh}</style><img src="file://%s/%s">' "$PWD" "$1" > "$wrapper"
  "$CHROME" --headless --disable-gpu --hide-scrollbars \
    --default-background-color=00000000 --force-device-scale-factor=1 \
    --window-size="$2,$2" --screenshot="$3" "file://$wrapper" >/dev/null 2>&1
}

mkdir -p "$OUT"

# --- macOS .icns -------------------------------------------------------------
# Chrome can't open windows smaller than ~500px, so render once at 1024 and
# downscale with PIL (high-quality Lanczos) for every other size.
ICONSET="$TMP/kayiver.iconset"
mkdir -p "$ICONSET"
render assets/logo/kayiver-icon.svg 1024 "$TMP/icon_1024.png"
python3 - "$TMP" <<'PY'
import sys
from PIL import Image
tmp = sys.argv[1]
src = Image.open(f"{tmp}/icon_1024.png").convert("RGBA")
for s in (16, 32, 64, 128, 256, 512):
    src.resize((s, s), Image.LANCZOS).save(f"{tmp}/icon_{s}.png")
PY
cp "$TMP/icon_16.png"   "$ICONSET/icon_16x16.png"
cp "$TMP/icon_32.png"   "$ICONSET/icon_16x16@2x.png"
cp "$TMP/icon_32.png"   "$ICONSET/icon_32x32.png"
cp "$TMP/icon_64.png"   "$ICONSET/icon_32x32@2x.png"
cp "$TMP/icon_128.png"  "$ICONSET/icon_128x128.png"
cp "$TMP/icon_256.png"  "$ICONSET/icon_128x128@2x.png"
cp "$TMP/icon_256.png"  "$ICONSET/icon_256x256.png"
cp "$TMP/icon_512.png"  "$ICONSET/icon_256x256@2x.png"
cp "$TMP/icon_512.png"  "$ICONSET/icon_512x512.png"
cp "$TMP/icon_1024.png" "$ICONSET/icon_512x512@2x.png"
iconutil -c icns "$ICONSET" -o "$OUT/Kayiver.icns"
cp "$TMP/icon_1024.png" "$OUT/kayiver-1024.png"
cp "$TMP/icon_256.png"  "$OUT/kayiver-256.png"

# --- Windows .ico ------------------------------------------------------------
python3 - "$TMP" "$OUT" <<'PY'
import sys
from PIL import Image
tmp, out = sys.argv[1], sys.argv[2]
img = Image.open(f"{tmp}/icon_256.png").convert("RGBA")
img.save(f"{out}/kayiver.ico",
         sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])
print("ico ok")
PY

# --- macOS menu-bar template (black + alpha) ----------------------------------
render assets/logo/kayiver-menubar-template.svg 1024 "$TMP/menubar_1024.png"
python3 - "$TMP" "$OUT" <<'PY'
import sys
from PIL import Image
tmp, out = sys.argv[1], sys.argv[2]
src = Image.open(f"{tmp}/menubar_1024.png").convert("RGBA")
src.resize((22, 22), Image.LANCZOS).save(f"{out}/menubarTemplate.png")
src.resize((44, 44), Image.LANCZOS).save(f"{out}/menubarTemplate@2x.png")
PY

echo "icons written to $OUT/"
ls -la "$OUT"
