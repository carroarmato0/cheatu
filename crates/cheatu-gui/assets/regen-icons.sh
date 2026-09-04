#!/usr/bin/env bash
# Regenerate the bundled icon font (CheatuIcons.ttf).
#
# We bundle a tiny monochrome subset of Noto Emoji so the UI's icon glyphs
# render reliably on every system AND when relaunched elevated via pkexec —
# egui only rasterizes monochrome outlines, and can't be trusted to find a
# system font covering newer emoji (e.g. 🧪 U+1F9EA). See install_fonts().
#
# When you add a NEW emoji/symbol glyph to the GUI, add its codepoint to CPS
# below and re-run this script, else the new glyph will render as tofu.
#
# Only real emoji codepoints work here: the source is Noto Emoji, so symbols it
# doesn't cover (↶ U+21B6, ▼ U+25BC, ＋ U+FF0B, ● U+25CF, ✓ U+2713, ✗ U+2717 …)
# are dropped from the subset without a word — which is how the Undo button
# ended up rendering as tofu. The verification step below now catches that; use
# an emoji equivalent (↩ ⬇ ⚫ ✔ ✖) or plain ASCII in the UI instead.
#
# Requires: fonttools (pyftsubset, varLib.instancer). Not a build/runtime dep —
# only needed to regenerate this asset.
set -euo pipefail
cd "$(dirname "$0")"

# Codepoints used by the GUI (keep in sync with the emoji/symbols in main.rs):
#   1F50D 🔍  1F512 🔒  2139 ℹ   2699 ⚙   2744 ❄   1F5D1 🗑  1F9EA 🧪
#   21A9 ↩   2B07 ⬇   26AB ⚫  26AA ⚪  2714 ✔  2716 ✖  FE0E/FE0F selectors
CPS="U+1F50D,U+1F512,U+2139,U+2699,U+2744,U+1F5D1,U+1F9EA,U+21A9,U+2B07,U+26AB,U+26AA,U+2714,U+2716,U+FE0E,U+FE0F"

SRC_URL="https://raw.githubusercontent.com/google/fonts/main/ofl/notoemoji/NotoEmoji%5Bwght%5D.ttf"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -sSL "$SRC_URL" -o "$tmp/var.ttf"
curl -sSL "https://raw.githubusercontent.com/google/fonts/main/ofl/notoemoji/OFL.txt" -o NotoEmoji-OFL.txt
fonttools varLib.instancer "$tmp/var.ttf" wght=400 -o "$tmp/static.ttf" >/dev/null
pyftsubset "$tmp/static.ttf" --unicodes="$CPS" \
  --output-file=CheatuIcons.ttf --no-hinting --desubroutinize

# pyftsubset drops requested codepoints the source font lacks, silently. Fail
# instead, so a glyph that would tofu never ships.
python3 - "$CPS" <<'PY'
import sys
from fontTools.ttLib import TTFont

have = set(TTFont("CheatuIcons.ttf").getBestCmap())
missing = [c for c in sys.argv[1].split(",") if int(c[2:], 16) not in have]
if missing:
    sys.exit("Noto Emoji has no glyph for: " + " ".join(missing))
PY

echo "Wrote CheatuIcons.ttf ($(stat -c%s CheatuIcons.ttf) bytes)"
