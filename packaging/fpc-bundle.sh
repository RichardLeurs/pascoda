#!/usr/bin/env bash
# Copies the Free Pascal compiler and the units Pascoda needs into <dest>.
#
# Usage: packaging/fpc-bundle.sh <dest> [fpc-prefix]
#   packaging/fpc-bundle.sh vendor/fpc    a source checkout: Pascoda then uses this compiler, not the one on PATH
#   (packaging/bundle.sh runs it for Pascoda.app/Contents/Resources/vendor/fpc)
#
# The source is the installed Free Pascal (`brew install fpc`), or the prefix given as second argument
# (the folder that has bin/fpc and lib/fpc). The result is relocatable: bin/fpc is the compiler itself
# (ppca64), and Pascoda writes the compiler's fpc.cfg for wherever it is (pascoda_fpc::bundled).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="${1:?usage: fpc-bundle.sh <dest> [fpc-prefix]}"

if [ -n "${2:-}" ]; then
    prefix="$2"
else
    fpc="$(command -v fpc || true)"
    [ -n "$fpc" ] || fpc=/opt/homebrew/bin/fpc
    [ -x "$fpc" ] || { echo "fpc-bundle.sh: Free Pascal is not installed (brew install fpc)" >&2; exit 1; }
    real="$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$fpc")"
    prefix="$(cd "$(dirname "$real")/.." && pwd)"
fi
ver="$("$prefix/bin/fpc" -iV)"
ppc="$prefix/lib/fpc/$ver/ppca64"
units="$prefix/lib/fpc/$ver/units/aarch64-darwin"
[ -x "$ppc" ] && [ -d "$units" ] || { echo "fpc-bundle.sh: no aarch64 compiler or units in $prefix (version $ver)" >&2; exit 1; }

# Unit packages. The first group is the runtime library and the packages of everyday programs; the
# rest is what the LCL needs (images, Cocoa and MacOS headers, zlib, jpeg) and the database and
# test packages the IDE's own features use. Add a name to ship another package (each adds its size).
packages=(
    rtl rtl-objpas rtl-unicode rtl-extra rtl-console rtl-generics
    fcl-base fcl-image fcl-json fcl-process fcl-xml fcl-net fcl-stl fcl-web fcl-extra
    hash iconvenc regexpr openssl unzip zlib
    fcl-db sqlite fcl-fpcunit
    paszlib pasjpeg chm univint cocoaint
)

rm -rf "$dest"
mkdir -p "$dest/bin" "$dest/lib/fpc/$ver/units/aarch64-darwin" "$dest/licenses"
cp "$ppc" "$dest/bin/fpc"
for p in "${packages[@]}"; do
    [ -d "$units/$p" ] || { echo "fpc-bundle.sh: unit package $p is missing from $units" >&2; exit 1; }
    rsync -a "$units/$p" "$dest/lib/fpc/$ver/units/aarch64-darwin/"
done

# Licences: the compiler is GPL, the runtime library and packages LGPL with a linking exception.
for f in "$root/vendor/lazarus/COPYING.GPL.txt" "$root/vendor/lazarus/COPYING.LGPL.txt" \
         "$root/vendor/fpcsrc/rtl/COPYING.FPC" "$root/vendor/fpcsrc/rtl/COPYING.txt"; do
    [ -f "$f" ] && cp "$f" "$dest/licenses/"
done
cat > "$dest/licenses/SOURCE.txt" <<TXT
Free Pascal $ver (compiler ppca64 and the unit packages in lib/) is copied from a Homebrew build of
Free Pascal for macOS arm64. Its source code is available from

  https://downloads.sourceforge.net/project/freepascal/Source/$ver/fpc-$ver.source.tar.gz
  https://gitlab.com/freepascal.org/fpc/source  (tag release_$(echo "$ver" | tr . _))

Homebrew applies its own patches to that source: https://github.com/Homebrew/homebrew-core/blob/master/Formula/f/fpc.rb

The compiler is licensed under the GNU General Public License (licenses/COPYING.GPL.txt). The runtime
library and the packages are licensed under the GNU Lesser General Public License with the linking
exception of licenses/COPYING.FPC, so programs you build with them keep your own licence.
TXT
printf '%s\n' "$ver" > "$dest/.pascoda-fpc"
echo "fpc-bundle.sh: Free Pascal $ver -> $dest ($(du -sh "$dest" | cut -f1))"
