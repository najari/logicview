#!/bin/bash
# Build sigrok-cli (and the libsigrokdecode it links against) from the commits
# pinned by the SLogic superproject, into the same prefix as
# build-libsigrok-fx2lafw.sh. The resulting sigrok-cli.exe is dropped into the
# portable SLogicView folder and runs on that folder's DLLs, decoders and
# embedded Python; the MCP server (mcp/) drives it.
#
# Run from an MSYS2 UCRT64 shell after build-libsigrok-fx2lafw.sh:
#   bash build-sigrok-cli.sh <SLogic checkout> <build dir>
# Needs: git -C SLogic submodule update --init sources/libsigrokdecode sources/sigrok-cli
#        pacman -S mingw-w64-ucrt-x86_64-python
set -euo pipefail

SLOGIC=${1:?usage: $0 <SLogic checkout> <build dir>}
BUILD=${2:?usage: $0 <SLogic checkout> <build dir>}
JOBS=${JOBS:-$(nproc)}

[ "${MSYSTEM:-}" = UCRT64 ] || { echo "run under MSYSTEM=UCRT64" >&2; exit 1; }
SLOGIC=$(cd "$SLOGIC" && pwd)
BUILD=$(cd "$BUILD" && pwd)
PREFIX="$BUILD/prefix"
HERE=$(cd "$(dirname "$0")" && pwd)
[ -f "$PREFIX/lib/pkgconfig/libsigrok.pc" ] || { echo "run build-libsigrok-fx2lafw.sh first" >&2; exit 1; }
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

for mod in libsigrokdecode sigrok-cli; do
    src="$SLOGIC/sources/$mod"
    [ -f "$src/configure.ac" ] || { echo "missing $src (git submodule update --init sources/$mod)" >&2; exit 1; }
    echo "== $mod $( [ -e "$src/.git" ] && git -C "$src" rev-parse HEAD || echo "see sources/README.md")"
    rsync -a --delete --exclude=.git "$src/" "$BUILD/$mod/"
    cd "$BUILD/$mod"
    if [ "$mod" = sigrok-cli ]; then
        for p in "$HERE"/sigrok-cli-patches/*.patch; do
            [ -e "$p" ] || continue
            echo "Applying $(basename "$p")"
            # A Windows checkout may have CRLF sources; the patches are LF.
            for f in $(sed -n 's|^+++ b/\([^[:space:]]*\).*|\1|p' "$p"); do sed -i 's/\r$//' "$f"; done
            patch -p1 --forward --no-backup-if-mismatch < "$p"
        done
    fi
    sh ./autogen.sh
    if [ "$mod" = libsigrokdecode ]; then
        ./configure --prefix="$PREFIX" --enable-shared --disable-static
    else
        ./configure --prefix="$PREFIX"
    fi
    make -j"$JOBS"
    make install
done

echo
echo "Built: $PREFIX/bin/sigrok-cli.exe"
objdump -p "$PREFIX/bin/sigrok-cli.exe" | grep 'DLL Name'
