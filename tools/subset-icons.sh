#!/bin/sh
# Rebuild the embedded Phosphor icon fonts with only the glyphs src/icons.rs uses.
# Needs npm and fonttools (pip install fonttools).
set -eu
cd "$(dirname "$0")/.."
VERSION=2.1.2
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$tmp" && npm pack --silent "@phosphor-icons/web@$VERSION" >/dev/null && tar xzf phosphor-icons-web-*.tgz)
codes=$(grep -o '\\u{[0-9a-f]*}' src/icons.rs | tr -d '\\u{}' | sort -u | sed 's/^/U+/' | paste -sd, -)
for w in Bold Fill; do
  lw=$(echo "$w" | tr 'A-Z' 'a-z')
  pyftsubset "$tmp/package/src/$lw/Phosphor-$w.ttf" --unicodes="U+0020,$codes" \
    --layout-features='' --no-hinting --output-file="assets/fonts/Phosphor-$w.ttf"
done
cp "$tmp/package/LICENSE" assets/fonts/LICENSE-Phosphor.txt
ls -l assets/fonts/Phosphor-*.ttf
