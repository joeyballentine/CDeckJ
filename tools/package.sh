#!/bin/sh
# Package a release build for the Steam Deck: dist/cdeckj-steamdeck-x86_64.tar.gz holding the
# app, README and licences. The app installs itself when opened. Used by CI and for local builds.
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
cp README.md LICENSE "$stage/"
cp assets/fonts/OFL-Barlow.txt assets/fonts/OFL-RobotoMono.txt assets/fonts/LICENSE-Phosphor.txt "$stage/"
chmod 755 "$stage/cdeckj"
tar -C dist -czf dist/cdeckj-steamdeck-x86_64.tar.gz cdeckj-steamdeck
echo "dist/cdeckj-steamdeck-x86_64.tar.gz"
