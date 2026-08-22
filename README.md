# Sivana

A production-grade Rust audio recognition system: sparse deterministic
fingerprints, streaming DSP, a custom memory-mapped search index and a
shared Rust/WASM core. Roadmap and engineering rules live in
[research/PLAN.md](research/PLAN.md).

## Layout

```
legacy/            frozen prototype (control implementation, never deleted)
crates/
  sivana-core      shared types: RecordingId, versions, 32-bit hashes,
                   AlgorithmConfig schema
  sivana-audio     deterministic WAV IO + seeded synthetic fixtures
  sivana-dsp       windows, biquads, noise, resampling, level math
  sivana-bench     degradation matrix + baseline runner + reports
research/          papers, algorithm notes, experiments, benchmark history
index-format/      working spec for the future .siv mmap index
```

## Quick start

```bash
# run the legacy CLI (enroll/query/list)
cargo run -p sivana-legacy -- enroll song.wav
cargo run -p sivana-legacy -- list

# benchmark platform (Phase 0 exit criteria): one command
cargo run -p sivana-bench --release -- run --tracks 3 --seconds 15
# -> bench-work/baseline.json + bench-work/BASELINE.md
```

## Engineering rules (short form)

Benchmark before optimizing. No parameter without a benchmark
justification. Preserve the control implementation. Version fingerprint
and index formats. Keep native and WASM code shared. Treat false positives
as seriously as false negatives. See research/PLAN.md §92.
