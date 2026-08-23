# Sivana

A production-grade Rust audio recognition system: sparse deterministic
fingerprints, streaming constant-memory DSP, a custom memory-mapped search
index and a shared Rust/WASM core. Roadmap and engineering rules live in
[research/PLAN.md](research/PLAN.md); measured results in
[research/BENCHMARKS.md](research/BENCHMARKS.md) and
[research/EXPERIMENTS.md](research/EXPERIMENTS.md).

## Layout

```
legacy/            frozen prototype (control implementation, never deleted)
crates/
  sivana-core      shared types: RecordingId, engine/fingerprint versions,
                   32-bit hash packing, AlgorithmConfig schema
  sivana-audio     deterministic WAV IO + seeded fixtures (+ symphonia
                   decode behind the "decode" feature)
  sivana-dsp       streaming STFT, separable sliding max, PeaksV2,
                   WSOLA time-stretch, filters/noise/resampling/levels
  sivana-landmark  Engine A: streaming landmark-pair fingerprints (32-bit)
  sivana-match     flat rarity-weighted matcher: tolerance bucketing,
                   stop hashes, calibrated-gate features
  sivana-invariant Engine B1 (experimental): scale-invariant triplet
                   fingerprints with affine-fit verification
  sivana-index     LMDB (heed) backend + .siv mmap segments + manifest
  sivana-wasm      browser fingerprint engine + SFP1 batch wire format
  sivana-api       Axum recognition service: WS streaming sessions with
                   early exit + hot catalog swap; hosts apps/web
  sivana-ingest    catalog ingestion CLI (parallel, sha256-idempotent,
                   segment compaction)
  sivana-bench     degradation matrix, A/B runners, gate calibration,
                   load generator
apps/web           editorial website (vanilla HTML/CSS/JS + wasm build)
extension          Chrome MV3 tab-capture edition (same engine, same wire)
docs/              deployment notes
research/          papers, algorithm notes, experiments E1-E6, benchmarks
index-format/      .siv format spec (implemented by crates/sivana-index)
```

## Quick start

```bash
# benchmark platform: one command compares engines over degraded queries
cargo run -p sivana-bench --release -- run --tracks 3 --seconds 15 \
  --bands "512" --pitch "2,-2" --stretch "1.10"

# calibrate the acceptance gate from recorded evidence (E4)
cargo run -p sivana-bench --release -- calibrate --bands "512" --tolerance "0,2"

# build a catalog and serve it (hot-swappable manifest)
cargo run -p sivana-bench --release -- fixtures --out /tmp/corpus --tracks 4
cargo run -p sivana-ingest -- add --catalog /tmp/catalog /tmp/corpus
cargo run -p sivana-api --release -- --catalog /tmp/catalog
# -> http://127.0.0.1:8077  (editorial site + /v1/* API)

# load test a running node
cargo run -p sivana-bench --release -- loadgen --url ws://127.0.0.1:8077 \
  --sessions 24 --concurrency 6
```

## Status snapshot

* Engine A (Landmark V2, 512 bands): beats legacy on identity recall and
  gated recall at zero false accepts on the standard grid (E3/E4).
* Engine B1: scale-invariance demonstrated; not yet promotable — see E5.
* Neural engine: evaluated, deferred behind explicit triggers (E6).
* Browser + extension fingerprint locally; only SFP1 batches cross the
  network. Raw audio never leaves the client.

## Engineering rules (short form)

Benchmark before optimizing. No parameter without a benchmark
justification. Preserve the control implementation. Version fingerprint
and index formats. Keep native and WASM code shared. Treat false positives
as seriously as false negatives. See research/PLAN.md §92.
