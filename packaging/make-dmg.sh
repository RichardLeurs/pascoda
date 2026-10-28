#!/usr/bin/env bash
# Builds Pascoda.dmg: a disk image with Pascoda.app and an Applications shortcut to drag it to.
#
# Usage: packaging/make-dmg.sh [output-dir]      (default: ~/Library/Caches/Pascoda/dist)
#
# Runs bundle.sh first, so the signing variables work the same way:
#   CODESIGN_IDENTITY=<id>   also signs the disk image
#   NOTARY_PROFILE=<name>    also notarizes and staples the disk image (never run, no Developer ID)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$HOME/Library/Caches/Pascoda/dist}"
app="$out/Pascoda.app"
dmg="$out/Pascoda.dmg"

"$root/packaging/bundle.sh" "$out"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/pascoda-app/Cargo.toml" | head -1)"
work="$(mktemp -d)"
stage="$work/Pascoda"
mkdir -p "$stage"
# ditto keeps the signature, symlinks and extended attributes intact.
ditto "$app" "$stage/Pascoda.app"
ln -s /Applications "$stage/Applications"

rm -f "$dmg"
# ULFO (LZFSE) is much smaller than zlib for the bundled sources and needs macOS 10.11 or later.
hdiutil create -volname "Pascoda $version" -srcfolder "$stage" -ov -format ULFO "$dmg" >/dev/null
rm -rf "$work"

if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp -s "$CODESIGN_IDENTITY" "$dmg"
fi
if [ -n "${NOTARY_PROFILE:-}" ]; then
    [ -n "${CODESIGN_IDENTITY:-}" ] || { echo "make-dmg.sh: NOTARY_PROFILE needs CODESIGN_IDENTITY" >&2; exit 1; }
    xcrun notarytool submit "$dmg" --wait --keychain-profile "$NOTARY_PROFILE"
    xcrun stapler staple "$dmg"
fi

hdiutil verify "$dmg" >/dev/null
echo "built $dmg ($(du -h "$dmg" | cut -f1))"
