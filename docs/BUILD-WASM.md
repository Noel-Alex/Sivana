# Building the wasm engine (scripts/build-wasm.sh)

The wasm engine (`crates/sivana-wasm`) ships as **two independent copies**
of the same generated assets — one per surface:

| copy | location |
|---|---|
| web app | `apps/web/wasm/` |
| browser extension | `extension/wasm/` |

Because neither surface reads from the other, the copies can silently
drift when only one gets updated after an engine change. CI guards
against this; this script is how you keep them in sync in the first
place.

## When to run it

Run `scripts/build-wasm.sh` whenever anything under
`crates/sivana-wasm/` changes and you want the change visible in the
web app or extension. It is idempotent: re-running it is always safe
and produces identical output for identical source. Commit the synced
copies together with the engine change so CI's sync check passes.

Requires `wasm-pack` on PATH (`cargo install wasm-pack` if missing).

## What it does

```bash
wasm-pack build crates/sivana-wasm --target web --release \
  --out-dir <repo>/target/wasm-pack
```

then copies four generated files into **both** locations above:

- `sivana_wasm.js`
- `sivana_wasm_bg.wasm`
- `sivana_wasm.d.ts`
- `sivana_wasm_bg.wasm.d.ts`

It finishes by printing the sha256 of every copied file side by side
(web | extension) so any residual drift is immediately visible, and
exits nonzero if wasm-pack fails to produce any expected file.

## pcm-worklet.js is hand-maintained

**Never overwrite or copy over `pcm-worklet.js`.** It is not generated:
it is written by hand per surface, because the two surfaces load it
differently:

- `apps/web/wasm/pcm-worklet.js`
- `extension/pcm-worklet.js` (note: one level up, next to the
  extension entry points)

`build-wasm.sh` never touches either copy. If the AudioWorklet
processor changes, edit each copy deliberately.

## Verifying sync without building

CI runs the comparison alone — no wasm toolchain needed:

```bash
bash scripts/wasm-sync-check.sh   # what ci.yml runs
```

It compares sha256 of the four shared files between the two surfaces
and fails with a side-by-side table if they differ.
