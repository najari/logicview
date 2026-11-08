#!/bin/bash
# Check that every function a DLL imports is exported by the DLL that will be
# loaded next to it in an app folder (catches "entry point not found" before
# launching). Usage: check-imports.sh <dll to check> <app folder>
set -uo pipefail
DLL=${1:?dll}; APP=${2:?app dir}
missing=0
checked=0
current=""
while IFS= read -r line; do
    line=${line%$'\r'}   # native (UCRT64) objdump prints CRLF
    if [[ $line =~ DLL\ Name:\ (.+)$ ]]; then
        current=${BASH_REMATCH[1]}
        lc=${current,,}
        if [[ $lc == kernel32.dll || $lc == ws2_32.dll || $lc == api-ms-win-* ]]; then
            current=""; continue
        fi
        if [ -f "$APP/$current" ]; then
            objdump -p "$APP/$current" | tr -d '\r' | awk '/\[Ordinal\/Name Pointer\] Table/{t=1;next} t&&/^\t\[/{print $NF} t&&/^$/{t=0}' | sort -u > /tmp/exports.txt
            echo "== $current (from app folder)"
        else
            echo "== $current : NOT IN APP FOLDER"; missing=1; current=""
        fi
        continue
    fi
    [ -n "$current" ] || continue
    # e.g. "\t00087f10  <none>  000a  g_array_append_vals"
    if [[ $line =~ ^[[:space:]]+[0-9a-f]+[[:space:]]+(\<none\>|[0-9a-f]+)[[:space:]]+[0-9a-f]+[[:space:]]+([A-Za-z_][A-Za-z0-9_@]*)$ ]]; then
        sym=${BASH_REMATCH[2]}
        checked=$((checked + 1))
        grep -qxF "$sym" /tmp/exports.txt || { echo "   missing symbol: $sym"; missing=1; }
    fi
done < <(objdump -p "$DLL")
echo "symbols checked: ${checked:-0}"
[ $missing -eq 0 ] && echo "ALL IMPORTS RESOLVED" || echo "UNRESOLVED IMPORTS"
exit $missing
