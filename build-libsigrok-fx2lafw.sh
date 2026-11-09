#!/bin/bash
# Build the Sipeed libsigrok fork (as pinned by the SLogic superproject) with the
# fx2lafw driver enabled in addition to sipeed-slogic-analyzer, so the SLogic
# Windows apps can drive FX2-based (Saleae Logic-compatible) analyzers.
#
# Run from an MSYS2 UCRT64 shell:
#   MSYSTEM=UCRT64 bash -l build-libsigrok-fx2lafw.sh <SLogic checkout> <build dir>
#
# Only libsigrok-4.dll is produced; it is a drop-in replacement for the one in
# the matching SLogic release (same source commit, same UCRT64 toolchain).
set -euo pipefail

SLOGIC=${1:?usage: $0 <SLogic checkout> <build dir>}
BUILD=${2:?usage: $0 <SLogic checkout> <build dir>}
JOBS=${JOBS:-$(nproc)}

[ "${MSYSTEM:-}" = UCRT64 ] || { echo "run under MSYSTEM=UCRT64" >&2; exit 1; }
# configure needs an absolute --prefix; accept relative arguments.
SLOGIC=$(cd "$SLOGIC" && pwd)
mkdir -p "$BUILD"
BUILD=$(cd "$BUILD" && pwd)
SRC="$SLOGIC/sources/libsigrok"
[ -f "$SRC/configure.ac" ] || { echo "missing $SRC (git submodule update --init sources/libsigrok)" >&2; exit 1; }

echo "libsigrok commit: $( [ -e "$SRC/.git" ] && git -C "$SRC" rev-parse HEAD || echo "see sources/README.md")"
HERE=$(cd "$(dirname "$0")" && pwd)
rsync -a --delete --exclude=.git --exclude=build "$SRC/" "$BUILD/libsigrok/"
cd "$BUILD/libsigrok"
# Local fixes on top of the pinned source (see each patch header).
for p in "$HERE"/*.patch; do
    [ -e "$p" ] || continue
    echo "Applying $(basename "$p")"
    patch -p1 --forward --no-backup-if-mismatch < "$p"
done
sh ./autogen.sh
./configure --prefix="$BUILD/prefix" \
    --disable-all-drivers \
    --enable-sipeed-slogic-analyzer \
    --enable-fx2lafw \
    --disable-bindings --disable-cxx \
    --enable-shared --disable-static
make -j"$JOBS"
make install

echo
echo "Enabled drivers:"
grep -E '^#define HAVE_HW_' config.h || true
echo
echo "Built: $BUILD/prefix/bin/libsigrok-4.dll"
objdump -p "$BUILD/prefix/bin/libsigrok-4.dll" | grep 'DLL Name'
