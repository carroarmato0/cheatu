#!/usr/bin/env bash
# Pre-release checks for the things that drift silently between releases.
#
# Every one of these has actually gone wrong: the AUR pkgver sat at 0.1.0 for
# two releases, and .SRCINFO — which is generated, not written — was hand-edited
# into disagreeing with its own PKGBUILD. The AUR only rejects some of that at
# push time, long after the tag is public.
#
# Run it before tagging: `make check-release`. Everything here works offline;
# steps that need an Arch tool are skipped with a note when it is missing, so
# this is also useful on non-Arch machines.
set -euo pipefail

cd "$(dirname "$0")/.."
fail=0

note() { printf '  %s\n' "$1"; }
ok() { printf 'ok   %s\n' "$1"; }
bad() {
    printf 'FAIL %s\n' "$1"
    fail=1
}
skip() { printf 'skip %s\n' "$1"; }

# --- one version, everywhere ----------------------------------------------
# Cargo.toml is the source of truth; everything else either derives from it or
# has to be bumped by hand, which is exactly why it gets forgotten.
version=$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
printf 'cheatu %s\n\n' "$version"

check_version() {
    local file="$1" pattern="$2" found
    found=$(grep -m1 -oE "$pattern" "$file" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true)
    if [[ "$found" == "$version" ]]; then
        ok "$file"
    else
        bad "$file has ${found:-no version}, expected $version"
    fi
}

check_version snap/snapcraft.yaml "^version: *'?[0-9.]+"
check_version packaging/aur/PKGBUILD "^pkgver=[0-9.]+"
check_version packaging/aur/.SRCINFO "pkgver = [0-9.]+"

# --- .SRCINFO is generated, not written -----------------------------------
if command -v makepkg >/dev/null; then
    if (cd packaging/aur && makepkg --printsrcinfo) | diff -q - packaging/aur/.SRCINFO >/dev/null; then
        ok "packaging/aur/.SRCINFO matches its PKGBUILD"
    else
        bad "packaging/aur/.SRCINFO is stale — run 'make aur-srcinfo'"
        (cd packaging/aur && makepkg --printsrcinfo) | diff -u packaging/aur/.SRCINFO - || true
    fi
else
    skip ".SRCINFO check (no makepkg)"
fi

# --- what the Arch packaging guidelines ask of a PKGBUILD -----------------
if command -v namcap >/dev/null; then
    out=$(namcap packaging/aur/PKGBUILD || true)
    [[ -n "$out" ]] && printf '%s\n' "$out" | sed 's/^/  /'
    if printf '%s' "$out" | grep -q ' E: '; then
        bad "namcap reported errors"
    else
        ok "namcap (warnings above, if any, are advisory)"
    fi
else
    skip "namcap check (not installed)"
fi

# The GUI dlopens its windowing stack, so neither ldd nor namcap can see those
# dependencies — the sonames in the binary are the only evidence there is.
gui=target/release/cheatu-gui
if [[ -f "$gui" ]]; then
    missing=()
    while read -r soname; do
        case "$soname" in
        libc.so* | libm.so* | libgcc_s.so*) continue ;; # glibc, gcc-libs
        esac
        pkg=$(pacman -Qoq "/usr/lib/$soname" 2>/dev/null || true)
        [[ -z "$pkg" ]] && continue # not installed here; can't judge
        grep -q "'$pkg'" packaging/aur/PKGBUILD || missing+=("$pkg ($soname)")
    done < <(strings -a "$gui" | grep -oE 'lib[A-Za-z0-9_+-]+\.so\.[0-9]+' | sort -u)
    if ((${#missing[@]})); then
        bad "PKGBUILD depends is missing: ${missing[*]}"
    else
        ok "every dlopened library maps to a listed dependency"
    fi
else
    skip "dlopen dependency check (no $gui — run 'make build')"
fi

printf '\n'
if ((fail)); then
    note "fix the above before tagging a release"
    exit 1
fi
note "ready to tag"
