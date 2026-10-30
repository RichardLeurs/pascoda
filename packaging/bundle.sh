#!/usr/bin/env bash
# Builds Pascoda.app (release build + icon + Info.plist + code signature).
#
# Usage: packaging/bundle.sh [output-dir]      (default: ~/Library/Caches/Pascoda/dist)
#
# Signing:
#   default                  ad-hoc signature (`codesign -s -`): runs on this Mac only.
#   CODESIGN_IDENTITY=<id>   sign with that identity (e.g. "Developer ID Application: Name (TEAM)"),
#                            hardened runtime + secure timestamp, as notarization requires.
#   NOTARY_PROFILE=<name>    additionally zip the app, submit it with
#                            `xcrun notarytool submit --wait --keychain-profile <name>`
#                            (create the profile once with `xcrun notarytool store-credentials`)
#                            and staple the ticket with `xcrun stapler staple`.
#                            Requires CODESIGN_IDENTITY; this path has not been exercised.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$HOME/Library/Caches/Pascoda/dist}"
app="$out/Pascoda.app"

# Single source of truth for version and copyright.
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/pascoda-app/Cargo.toml" | head -1)"
copyright="$(sed -n 's/^pub const COPYRIGHT: &str = "\(.*\)";/\1/p' "$root/crates/pascoda-app/src/about.rs")"
min_macos="$(sed -n 's/^pub const MIN_MACOS: &str = "\(.*\)";/\1/p' "$root/crates/pascoda-fpc/src/lcl.rs")"
[ -n "$version" ] && [ -n "$copyright" ] && [ -n "$min_macos" ] || { echo "bundle.sh: cannot read version/copyright/min macOS" >&2; exit 1; }
if [ -n "${NOTARY_PROFILE:-}" ] && [ -z "${CODESIGN_IDENTITY:-}" ]; then
    echo "bundle.sh: NOTARY_PROFILE needs CODESIGN_IDENTITY" >&2; exit 1
fi

# What the app needs on a Mac without this checkout (the same list as pascoda_fpc::vendor::Part).
# Checked in vendor/ before the long build, and again in the bundle after fpcsrc.exclude trimmed it.
need=(
    lazarus/lcl/interfaces/cocoa
    fpcsrc/rtl
    fpcsrc/packages/fcl-db/src
    fpcsrc/packages/fcl-fpcunit/src
    pasls/dist/aarch64-darwin/pasls
)
check_vendor() { # <folder that holds the entries of $need> <what it is>
    for n in "${need[@]}"; do
        [ -e "$1/$n" ] || { echo "bundle.sh: $n is missing from $2 (set up vendor/ first, see README)" >&2; exit 1; }
    done
}
check_vendor "$root/vendor" "vendor/"
# The Free Pascal compiler and its units are copied from the installed one (see fpc-bundle.sh).
command -v fpc >/dev/null 2>&1 || [ -x /opt/homebrew/bin/fpc ] || { echo "bundle.sh: Free Pascal is not installed (brew install fpc); it is copied into the app" >&2; exit 1; }

(cd "$root" && cargo build --release -p pascoda-app)
# The build directory is set in .cargo/config.toml, so ask cargo where it is.
target="$(cd "$root" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
bin="$target/release/pascoda-app"
[ -x "$bin" ] || { echo "bundle.sh: $bin not found" >&2; exit 1; }

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/pascoda-app"
cp "$root/packaging/Pascoda.icns" "$app/Contents/Resources/Pascoda.icns"

# Toolchain sources the app needs on a Mac without this checkout: the LCL (runs the forms),
# CodeTools + FPC sources + the pasls binary (completion), and the Free Pascal compiler with its units.
res="$app/Contents/Resources/vendor"
mkdir -p "$res/lazarus/components" "$res/pasls/dist/aarch64-darwin"
lz="$root/vendor/lazarus"
cp "$lz"/COPYING*.txt "$res/lazarus/"
rsync -a --exclude '.git' "$lz/lcl" "$res/lazarus/"
for c in lazutils codetools freetype; do rsync -a --exclude '.git' "$lz/components/$c" "$res/lazarus/components/"; done
git -C "$lz" rev-parse --short HEAD > "$res/lazarus/.pascoda-commit" 2>/dev/null || echo bundled > "$res/lazarus/.pascoda-commit"
mkdir -p "$res/fpcsrc"
# The licence texts stay, though fpcsrc.exclude drops *.txt (the first matching rule wins).
rsync -a --include='COPYING*' --exclude '.git' --exclude-from="$root/packaging/fpcsrc.exclude" "$root/vendor/fpcsrc/" "$res/fpcsrc/"
cp "$root/vendor/pasls/dist/aarch64-darwin/pasls" "$res/pasls/dist/aarch64-darwin/"
cp "$root/vendor/pasls/COPYING" "$res/pasls/COPYING"
# The revisions of what was copied, next to the Lazarus one above.
git -C "$root/vendor/fpcsrc" describe --tags --always > "$res/fpcsrc/.pascoda-commit" 2>/dev/null || echo bundled > "$res/fpcsrc/.pascoda-commit"
git -C "$root/vendor/pasls" rev-parse --short HEAD > "$res/pasls/.pascoda-commit" 2>/dev/null || echo bundled > "$res/pasls/.pascoda-commit"
# The compiler that ships with the app (bin/fpc is the compiler itself, plus the unit packages and licences).
"$root/packaging/fpc-bundle.sh" "$res/fpc"
for n in fpc/bin/fpc fpc/.pascoda-fpc fpc/licenses/SOURCE.txt "fpc/lib/fpc/$(cat "$res/fpc/.pascoda-fpc")/units/aarch64-darwin/rtl"; do
    [ -e "$res/$n" ] || { echo "bundle.sh: $n is missing from the bundle" >&2; exit 1; }
done
check_vendor "$res" "the bundle"

# Components that ship with Pascoda (the VLC package; File > Import Component Package... offers it).
mkdir -p "$app/Contents/Resources/components"
rsync -a --exclude '.git' "$root/components/" "$app/Contents/Resources/components/"

# NSPrincipalClass is deliberately absent: eframe/winit create their own NSApplication.
# CFBundleIdentifier is a PLACEHOLDER; replace it with the identifier of the real developer account.
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
<key>CFBundleName</key><string>Pascoda</string>
<key>CFBundleDisplayName</key><string>Pascoda</string>
<key>CFBundleIdentifier</key><string>com.richardleurs.pascoda</string>
<key>CFBundleExecutable</key><string>pascoda-app</string>
<key>CFBundleIconFile</key><string>Pascoda</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>$version</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>LSMinimumSystemVersion</key><string>$min_macos</string>
<key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSDisabledDictationMenuItem</key><true/>
<key>NSDisabledCharacterPaletteMenuItem</key><true/>
<key>NSSupportsAutomaticGraphicsSwitching</key><true/>
<key>NSHumanReadableCopyright</key><string>$copyright</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"

if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    codesign --force --deep --options runtime --timestamp -s "$CODESIGN_IDENTITY" "$app"
else
    codesign --force --deep -s - "$app"
fi
codesign --verify --deep --strict "$app"

if [ -n "${NOTARY_PROFILE:-}" ]; then
    zip="$out/Pascoda.zip"
    ditto -c -k --keepParent "$app" "$zip"
    xcrun notarytool submit "$zip" --wait --keychain-profile "$NOTARY_PROFILE"
    xcrun stapler staple "$app"
    rm -f "$zip"
fi
echo "built $app (version $version)"
