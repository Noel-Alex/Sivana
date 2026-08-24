#!/usr/bin/env bash
# Build the Sivana wasm engine (crates/sivana-wasm) with wasm-pack and sync
# the generated assets into every surface that ships a copy:
#
#   apps/web/wasm/    web app copy     (plus hand-written pcm-worklet.js)
#   extension/wasm/   extension copy   (worklet lives at extension/pcm-worklet.js)
#
# Only the four wasm-pack-generated files are copied. pcm-worklet.js is
# hand-maintained per surface and is NEVER overwritten by this script.
#
# Idempotent: safe to re-run any time crates/sivana-wasm changes.
#
# Usage: scripts/build-wasm.sh   (from anywhere inside the checkout)

set -eu

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)"

if ! ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null)"; then
    echo "error: could not locate repo root via git rev-parse (not a checkout?)" >&2
    exit 1
fi

FILES="sivana_wasm.js sivana_wasm_bg.wasm sivana_wasm.d.ts sivana_wasm_bg.wasm.d.ts"
CRATE="crates/sivana-wasm"
OUT_DIR="$ROOT/target/wasm-pack"
APP_WASM="$ROOT/apps/web/wasm"
EXT_WASM="$ROOT/extension/wasm"

if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "error: wasm-pack not found on PATH." >&2
    echo "  install: cargo install wasm-pack" >&2
    echo "  (or: https://rustwasm.github.io/wasm-pack/installer/)" >&2
    exit 1
fi

echo "==> wasm-pack build $CRATE --target web --release"
(cd "$ROOT" && wasm-pack build "$CRATE" --target web --release --out-dir "$OUT_DIR")

missing=""
for f in $FILES; do
    [ -f "$OUT_DIR/$f" ] || missing="$missing
  $OUT_DIR/$f"
done
if [ -n "$missing" ]; then
    echo "error: wasm-pack output incomplete; missing:$missing" >&2
    exit 1
fi

mkdir -p "$APP_WASM" "$EXT_WASM"
for f in $FILES; do
    cp -f "$OUT_DIR/$f" "$APP_WASM/$f"
    cp -f "$OUT_DIR/$f" "$EXT_WASM/$f"
done
echo "==> synced 4 generated files into:"
echo "    $APP_WASM"
echo "    $EXT_WASM"
echo "    (pcm-worklet.js left untouched: hand-maintained per surface)"

echo
echo "==> sha256 side by side (web copy | extension copy)"
fail=0
printf '  %-24s %-64s %-64s %s\n' FILE "APPS/WEB/WASM" "EXTENSION/WASM" STATUS
for f in $FILES; do
    a="$(sha256sum "$APP_WASM/$f" | cut -d' ' -f1)"
    b="$(sha256sum "$EXT_WASM/$f" | cut -d' ' -f1)"
    if [ "$a" = "$b" ]; then
        status=in-sync
    else
        status=DRIFT
        fail=1
    fi
    printf '  %-24s %-64s %-64s %s\n' "$f" "$a" "$b" "$status"
done

if [ "$fail" -ne 0 ]; then
    echo "error: copies drifted immediately after sync (unexpected)" >&2
    exit 1
fi
echo "OK: both surfaces carry identical wasm assets."
