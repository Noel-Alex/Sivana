# Sivana

A Rust-native audio recognition system: streaming fingerprints, a custom
memory-mapped search index, a browser/WASM engine, and an editorial web
product on top. Design document: [research/PLAN.md](research/PLAN.md).

## Status

Phases 0–6 and 10 are implemented and measured. Phases 7–9 are partial or
deferred — by recorded decision, not omission:

| phase | scope | where | status |
|---|---|---|---|
| 0 benchmark platform | degradation grid, legacy control, one-command reports | `crates/sivana-bench` | done |
| 1 landmark engine v2 | streaming STFT → adaptive peaks → scored landmarks | `sivana-dsp`, `sivana-landmark` | done |
| 2 matcher v2 | rarity weights, offset tolerance, calibrated zero-FA gate (E4) | `sivana-match` | done; calibration work continues per [research/PRODUCTION-ROBUSTNESS.md](research/PRODUCTION-ROBUSTNESS.md) |
| 3 production index | LMDB backend + custom `.siv` mmap segments, atomic manifests | `sivana-index`, [index-format/SPEC.md](index-format/SPEC.md) | done at dev scale; large-catalog p99 unmeasured |
| 4 wasm engine | same core compiled to wasm32, SFP1 wire format | `sivana-wasm` | done; WASM SIMD not attempted |
| 5 production website | Axum API + streaming recognition + editorial UI | `sivana-api`, `apps/web` | done |
| 6 catalog platform | parallel idempotent ingest, delta segments, compaction | `sivana-ingest` | done |
| 7 scale-invariant engine | B1 triplets + affine verification; measured, **not promoted** (E5) | `sivana-invariant` | partial: B1 built + benchmarked; B2 quads not implemented; Engine A stays primary |
| 8 neural evaluation | evaluation-only; deferred with re-entry triggers T1–T3 | [research/NEURAL-EVAL.md](research/NEURAL-EVAL.md) | deferred by design (E6) |
| 9 chrome extension | MV3 tabCapture → offscreen, reuses the same engine | `extension/` | functional gaps being closed |
| 10 scale-out | hot catalog swap, load generator, deployment guide | `docs/DEPLOY.md` | done for single-region |

Experiment log with every measured decision: [research/EXPERIMENTS.md](research/EXPERIMENTS.md).
Where EXPERIMENTS.md and PRODUCTION-ROBUSTNESS.md disagree, the latter wins.

## Quick start

```bash
cargo test --workspace                 # 100+ tests across all crates
cargo run -p sivana-bench --release -- run --tracks 3 --seconds 15 \
  --bands "512"                        # recognition A/B vs legacy
```

Serve the website against an ingested catalog:

```bash
cargo run -p sivana-bench --release -- fixtures --out /tmp/songs --tracks 4
cargo run -p sivana-ingest --release -- add --catalog /tmp/catalog /tmp/songs
cargo run -p sivana-api  --release -- --catalog /tmp/catalog --web apps/web
# open http://127.0.0.1:8077 — Hear it. Name it.
```

The browser fingerprints locally (wasm); only compact fingerprint batches
cross the network. Raw audio never leaves the page.

## Workspace

`legacy/` is the frozen first prototype kept as the benchmark control.
Everything under `crates/` is the rebuild it measures against.
Deployment: [docs/DEPLOY.md](docs/DEPLOY.md).
