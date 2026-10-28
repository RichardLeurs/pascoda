#!/usr/bin/env bash
# Regenerates the icons (needs swift and iconutil):
#   Pascoda.icns / Pascoda-1024.png          the P with a run triangle (used by the app bundle)
#   Pascoda-classic.icns / -classic-1024.png the plain P
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
swift "$here/make-icon.swift" "$tmp/Pascoda.iconset" "$here/Pascoda-1024.png"
iconutil -c icns "$tmp/Pascoda.iconset" -o "$here/Pascoda.icns"
swift "$here/make-icon.swift" "$tmp/Classic.iconset" "$here/Pascoda-classic-1024.png" classic
iconutil -c icns "$tmp/Classic.iconset" -o "$here/Pascoda-classic.icns"
echo "wrote $here/Pascoda.icns and Pascoda-classic.icns"
