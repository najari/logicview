#!/bin/bash
# Symbol-level load check of a portable SLogic app folder after the DLL swap.
# Usage: verify-app.sh <app folder> [exe name]
set -uo pipefail
A=${1:?app folder}; EXE=${2:-slogicview.exe}
S="$(dirname "$0")/check-imports.sh"
rc=0
files="libsigrok-4.dll libz-1.dll libsigrokcxx-4.dll $EXE"
# Added by assemble-slogicview.ps1 -SigrokCli.
for f in sigrok-cli.exe python.exe; do [ -f "$A/$f" ] && files="$files $f"; done
for f in $files; do
    echo "### $f"
    bash "$S" "$A/$f" "$A" | grep -E 'libsigrok-4|missing|NOT IN|symbols|RESOLVED' || true
    bash "$S" "$A/$f" "$A" >/dev/null || rc=1
done
exit $rc
