#!/usr/bin/env bash
# Assemble dist/cheatu.app from a release build of the GUI.
#
# The bundle gives the Dock and Finder a real icon from the start, including for
# the root instance that "Elevate & attach" relaunches (a bare binary run as
# root keeps the generic "exec" icon). Uses only tools that ship with macOS.
#
# Usage: packaging/macos/build-app.sh [version]
set -euo pipefail

cd "$(dirname "$0")/../.."
VERSION="${1:-$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')}"
APP=dist/cheatu.app
SVG=packaging/assets/io.github.carroarmato0.cheatu.svg

cargo build --release --locked -p cheatu-gui

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m755 target/release/cheatu-gui "$APP/Contents/MacOS/cheatu-gui"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist >"$APP/Contents/Info.plist"

# Rasterise the SVG into an .iconset (sips handles SVG) and pack it as .icns.
ICONSET="$(mktemp -d)/cheatu.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
	sips -s format png -z "$size" "$size" "$SVG" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
	double=$((size * 2))
	sips -s format png -z "$double" "$double" "$SVG" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/cheatu.icns"
rm -rf "$(dirname "$ICONSET")"

# Ad-hoc sign so the bundle (not just the binary) has a valid signature.
codesign --force --sign - "$APP"

echo "Built $APP ($VERSION)"
