#!/usr/bin/env bash
# Assemble wlmatrix.app from a built binary.
#
# macOS-only: uses `sips` + `iconutil` to turn dist/macos/icon.png into a proper
# .icns. Run on the macOS CI runner (or locally on a Mac):
#
#   dist/macos/make-app.sh <path-to-wlmatrix-binary> <version> [output-dir]
#
# Produces <output-dir>/wlmatrix.app. The bundle is NOT code-signed — see the
# README's macOS section for the Gatekeeper quarantine workaround.
set -euo pipefail

BIN="${1:?usage: make-app.sh <binary> <version> [outdir]}"
VERSION="${2:?usage: make-app.sh <binary> <version> [outdir]}"
OUT="${3:-.}"
HERE="$(cd "$(dirname "$0")" && pwd)"

APP="$OUT/wlmatrix.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# Executable.
cp "$BIN" "$APP/Contents/MacOS/wlmatrix"
chmod +x "$APP/Contents/MacOS/wlmatrix"

# Info.plist with the version substituted in.
sed "s/@VERSION@/$VERSION/g" "$HERE/Info.plist" > "$APP/Contents/Info.plist"

# Icon: PNG -> .icns via a canonical .iconset (needs macOS tools).
if command -v iconutil >/dev/null 2>&1 && [ -f "$HERE/icon.png" ]; then
  ICONSET="$(mktemp -d)/wlmatrix.iconset"
  mkdir -p "$ICONSET"
  for base in 16 32 128 256 512; do
    sips -z "$base" "$base" "$HERE/icon.png" \
      --out "$ICONSET/icon_${base}x${base}.png" >/dev/null
    dbl=$((base * 2))
    sips -z "$dbl" "$dbl" "$HERE/icon.png" \
      --out "$ICONSET/icon_${base}x${base}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/wlmatrix.icns"
else
  echo "make-app.sh: iconutil/icon.png unavailable — bundle will use a generic icon" >&2
fi

echo "built $APP (version $VERSION)"
