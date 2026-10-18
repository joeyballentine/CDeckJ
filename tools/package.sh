#!/bin/sh
# Package a release build for the Steam Deck: dist/cdeckj-steamdeck-x86_64.tar.gz holding the
# app, install.sh, README, licences and the icon. Used by CI and for local builds.
#
#   tools/package.sh [path/to/cdeckj]   (default: the glibc 2.35 zigbuild output)
set -eu
cd "$(dirname "$0")/.."
bin=${1:-target/x86_64-unknown-linux-gnu/release/cdeckj}
[ -x "$bin" ] || { echo "no release binary at $bin" >&2; exit 1; }
stage=dist/cdeckj-steamdeck
rm -rf "$stage"
mkdir -p "$stage"
cp "$bin" "$stage/cdeckj"
cp install.sh README.md LICENSE "$stage/"
cp assets/fonts/OFL-Barlow.txt assets/fonts/OFL-JetBrainsMono.txt assets/fonts/LICENSE-Phosphor.txt "$stage/"
cp assets/icon/cdeckj-256.png "$stage/cdeckj.png"
chmod 755 "$stage/cdeckj" "$stage/install.sh"
tar -C dist -czf dist/cdeckj-steamdeck-x86_64.tar.gz cdeckj-steamdeck
echo "dist/cdeckj-steamdeck-x86_64.tar.gz"
