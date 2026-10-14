#!/usr/bin/env bash
# Tests copy_font_licences (packaging/env.sh) against synthetic craft-fonts checkouts: every font's
# licence file reaches the package under its manifest name (the OFL text and an Apache-2.0 NOTICE
# alike), and a broken manifest is a warning normally but an error for a release build
# (CRAFT_FONTS_REQUIRED). packaging/tests/font-licences.ps1 checks the Windows copy the same way.
# Run: bash packaging/tests/font-licences.sh (CI: .github/workflows/packaging-lint.yml).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export DIST="$work/dist"
unset CRAFT_FONTS_DIR CRAFT_FONTS_REQUIRED
# shellcheck source=../env.sh
. "$here/../env.sh"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# A checkout shaped like craft-fonts: two OFL families (one with two styles sharing a licence) and
# an Apache-2.0 face whose licence file is an upstream NOTICE without an extension.
checkout() {
  local dir="$1"
  mkdir -p "$dir/fonts/serif" "$dir/fonts/sans" "$dir/fonts/fallback"
  printf 'SIL OPEN FONT LICENSE Version 1.1 (serif)\n' >"$dir/fonts/serif/OFL.txt"
  printf 'SIL OPEN FONT LICENSE Version 1.1 (sans)\n' >"$dir/fonts/sans/OFL.txt"
  printf 'Apache License Version 2.0 (fallback)\n' >"$dir/fonts/fallback/NOTICE"
  local sha
  sha="$(printf '0%.0s' {1..64})"
  cat >"$dir/fonts/manifest.txt" <<EOF
# family | style | file | scripts | licence | licence file | sha256 | source
Serif | Regular | fonts/serif/Serif.ttf | Jpan,Latn | OFL-1.1 | fonts/serif/OFL.txt | $sha | https://example.com/serif

Sans | Regular | fonts/sans/Sans-Regular.ttf | Jpan,Latn | OFL-1.1 | fonts/sans/OFL.txt | $sha | https://example.com/sans
Sans | Bold | fonts/sans/Sans-Bold.ttf | Jpan,Latn | OFL-1.1 | fonts/sans/OFL.txt | $sha | https://example.com/sans
Fallback | Regular | fonts/fallback/Fallback.ttf | Hans,Hant | Apache-2.0 | fonts/fallback/NOTICE | $sha | https://example.com/fallback
EOF
}

# 1. Every licence file, once, named after its directory, byte-identical.
checkout "$work/good"
mkdir "$work/out1"
CRAFT_FONTS_DIR="$work/good" copy_font_licences "$work/out1"
got="$(find "$work/out1" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort | tr '\n' ' ')"
[ "$got" = "NOTICE-fallback.txt OFL-sans.txt OFL-serif.txt " ] || fail "copied: $got"
cmp -s "$work/good/fonts/fallback/NOTICE" "$work/out1/NOTICE-fallback.txt" || fail "NOTICE content differs"
cmp -s "$work/good/fonts/sans/OFL.txt" "$work/out1/OFL-sans.txt" || fail "OFL content differs"

# 2. Without CRAFT_FONTS_DIR nothing is copied and nothing fails.
mkdir "$work/out2"
copy_font_licences "$work/out2"
[ -z "$(ls -A "$work/out2")" ] || fail "copied without CRAFT_FONTS_DIR"

# 3. A missing licence file and a path out of the checkout: warnings for a local build, which still
#    copies the rest. The warnings go to stderr, or to stdout as `::warning::` lines under GitHub
#    Actions (`warn` in env.sh), so both are captured; copy_font_licences prints nothing else.
checkout "$work/bad"
printf 'Broken | Regular | fonts/broken/B.ttf | Latn | OFL-1.1 | fonts/broken/OFL.txt | %s | https://example.com\n' "$(printf '0%.0s' {1..64})" >>"$work/bad/fonts/manifest.txt"
printf 'Escape | Regular | fonts/x/E.ttf | Latn | OFL-1.1 | ../outside.txt | %s | https://example.com\n' "$(printf '0%.0s' {1..64})" >>"$work/bad/fonts/manifest.txt"
printf 'not a licence\n' >"$work/outside.txt"
mkdir "$work/out3"
warnings="$(CRAFT_FONTS_DIR="$work/bad" copy_font_licences "$work/out3" 2>&1)" || fail "a local build failed on a broken manifest"
case "$warnings" in *"fonts/broken/OFL.txt is listed in the craft-fonts manifest but missing"*) ;; *) fail "no missing-licence warning: $warnings" ;; esac
case "$warnings" in *"../outside.txt leaves the checkout"*) ;; *) fail "no path warning: $warnings" ;; esac
[ "$(find "$work/out3" -mindepth 1 -maxdepth 1 | wc -l)" -eq 3 ] || fail "a local build did not copy the good licences"
[ ! -e "$work/out3/outside-..txt" ] || fail "copied a file from outside the checkout"

# ...and an error for a release build.
mkdir "$work/out4"
if (CRAFT_FONTS_REQUIRED=1 CRAFT_FONTS_DIR="$work/bad" copy_font_licences "$work/out4") 2>/dev/null; then
  fail "a release build accepted a missing font licence"
fi

# 4. A checkout without a manifest: a warning, or an error for a release build.
mkdir -p "$work/empty" "$work/out5"
CRAFT_FONTS_DIR="$work/empty" copy_font_licences "$work/out5" 2>/dev/null || fail "a local build failed without a manifest"
if (CRAFT_FONTS_REQUIRED=1 CRAFT_FONTS_DIR="$work/empty" copy_font_licences "$work/out5") 2>/dev/null; then
  fail "a release build accepted a checkout without a manifest"
fi

echo "ok font licences (bash): manifest names, Apache NOTICE, warnings locally, errors with CRAFT_FONTS_REQUIRED"
