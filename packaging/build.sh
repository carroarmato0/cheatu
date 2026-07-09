#!/usr/bin/env bash
# Build cheatu's native packages. Designed to run inside the container image
# (packaging/Containerfile), but works on any host that has the matching tools
# installed (cargo, cargo-deb, cargo-generate-rpm, linuxdeploy, appimagetool).
#
#   packaging/build.sh deb        # -> dist/cheatu_<ver>_<arch>.deb
#   packaging/build.sh rpm        # -> dist/cheatu-<ver>-1.<arch>.rpm
#   packaging/build.sh appimage   # -> dist/cheatu-<ver>-<arch>.AppImage
#   packaging/build.sh all        # all of the above
#
# Run from the repository root. VERSION defaults to the workspace version; CI
# overrides it from the release tag.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

APP_ID="io.github.carroarmato0.cheatu"
ARCH="$(uname -m)"                     # x86_64, aarch64, ...
DIST="$REPO_ROOT/dist"
BINS=(cheatu cheatu-gui cheatu-inject)

VERSION="${VERSION:-$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')}"
echo ">> cheatu $VERSION ($ARCH)"
mkdir -p "$DIST"

# --- shared: build the release binaries once ------------------------------
build_release() {
    echo ">> cargo build --release (workspace)"
    cargo build --release --locked
    # Strip to keep every package small; safe to repeat.
    for b in "${BINS[@]}"; do
        strip --strip-unneeded "target/release/$b" 2>/dev/null || true
    done
}

# --- deb (cargo-deb) -------------------------------------------------------
build_deb() {
    echo ">> cargo deb"
    cargo deb --no-build --locked -p cheatu-cli
    cp -v target/debian/*.deb "$DIST/"
}

# --- rpm (cargo-generate-rpm) ---------------------------------------------
build_rpm() {
    echo ">> cargo generate-rpm"
    cargo generate-rpm -p crates/cheatu-cli
    cp -v target/generate-rpm/*.rpm "$DIST/"
}

# --- AppImage (linuxdeploy + appimagetool) --------------------------------
build_appimage() {
    echo ">> AppImage"
    local appdir="$REPO_ROOT/target/AppDir"
    rm -rf "$appdir"
    mkdir -p "$appdir/usr/bin"
    for b in "${BINS[@]}"; do
        install -m755 "target/release/$b" "$appdir/usr/bin/$b"
    done

    # linuxdeploy bundles the shared libraries the GUI needs and lays out the
    # desktop file + icon. Our custom AppRun then dispatches to any binary.
    linuxdeploy \
        --appdir "$appdir" \
        --executable "$appdir/usr/bin/cheatu-gui" \
        --desktop-file "packaging/assets/$APP_ID.desktop" \
        --icon-file "packaging/assets/$APP_ID.svg" \
        --custom-apprun "packaging/appimage/AppRun"

    # linuxdeploy only tracks the executable it was given; make sure the other
    # two binaries survived the AppDir shuffle.
    for b in "${BINS[@]}"; do
        install -m755 "target/release/$b" "$appdir/usr/bin/$b"
    done

    local out="$DIST/cheatu-${VERSION}-${ARCH}.AppImage"
    ARCH="$ARCH" appimagetool "$appdir" "$out"
    echo ">> $out"
}

target="${1:-all}"
case "$target" in
    deb)      build_release; build_deb ;;
    rpm)      build_release; build_rpm ;;
    appimage) build_release; build_appimage ;;
    all)      build_release; build_deb; build_rpm; build_appimage ;;
    *) echo "usage: $0 {deb|rpm|appimage|all}" >&2; exit 2 ;;
esac

echo ">> done. Artifacts in $DIST:"
ls -1 "$DIST"
