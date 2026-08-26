#!/usr/bin/env bash
# Verify the committed wasm assets in apps/web/wasm/ and extension/wasm/
# are byte-identical. Used by CI; also handy locally before committing.
# Never builds anything: pure comparison of the four shared files.

set -eu

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)"

if ! ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"; then
    echo "error: could not locate repo root via git rev-parse (not a checkout?)" >&2
    exit 1
fi

FILES="sivana_wasm.js sivana_wasm_bg.wasm sivana_wasm.d.ts sivana_wasm_bg.wasm.d.ts"
APP_WASM="$ROOT/apps/web/wasm"
EXT_WASM="$ROOT/extension/wasm"

fail=0
printf '  %-24s %-64s %-64s %s\n' FILE "APPS/WEB/WASM" "EXTENSION/WASM" STATUS
for f in $FILES; do
    a="$APP_WASM/$f"
    b="$EXT_WASM/$f"
    if [ ! -f "$a" ] || [ ! -f "$b" ]; then
        [ -f "$a" ] || echo "error: missing $a" >&2
        [ -f "$b" ] || echo "error: missing $b" >&2
        fail=1
        continue
    fi
    ha="$(sha256sum "$a" | cut -d' ' -f1)"
    hb="$(sha256sum "$b" | cut -d' ' -f1)"
    if [ "$ha" = "$hb" ]; then
        status=in-sync
    else
        status=DRIFT
        fail=1
    fi
    printf '  %-24s %-64s %-64s %s\n' "$f" "$ha" "$hb" "$status"
done

if [ "$fail" -ne 0 ]; then
    echo "wasm asset drift detected: re-run scripts/build-wasm.sh and commit both copies." >&2
    exit 1
fi
echo "OK: wasm assets in sync between apps/web/wasm/ and extension/wasm/."
