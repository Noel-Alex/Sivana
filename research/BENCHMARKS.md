# Benchmarks

Reproduce everything here with one command:

```bash
cargo run -p sivana-bench --release -- run --tracks 3 --seconds 15
```

Scale up for publication-grade numbers:

```bash
cargo run -p sivana-bench --release -- run \
  --tracks 12 --seconds 45 --excerpt-seconds 4 \
  --positions-per-track 6 --snr "20,10,5,0,-5" --speeds "0.80,0.90,0.95,1.05,1.10,1.20"
```

Outputs: `bench-work/baseline.json` (full per-case data) and
`bench-work/BASELINE.md` (summary tables). Criterion hot-path benches:
`cargo bench -p sivana-bench`.

## Metric definitions

- **recall(track)** — rank-1 database entry is the correct recording
  (raw argmax over offset votes, no score gate).
- **recall(offset)** — track recall AND matched offset within 2 frames
  (~93 ms at legacy geometry).
- **gated** — would the stock legacy gate (score >= 100) accept it.
- **false accepts** — out-of-catalog probe accepted by the gate.

## Snapshot — Phase 0 baseline (2026-08-22)

Legacy engine, 3 tracks x 15 s @ 22050 Hz, seed 2026, 8 s excerpts:

| cell | n | recall(track) | recall(offset) | gated | mean fp ms | mean match ms |
|---|---:|---:|---:|---:|---:|---:|
| clean | 6 | 100% | 100% | 50% | 1.43 | 2.19 |
| clip0.30 | 6 | 100% | 100% | 0% | 1.50 | 3.70 |
| echo0.15s@0.4 | 6 | 100% | 100% | 17% | 1.40 | 1.99 |
| hp150 | 6 | 100% | 100% | 50% | 1.34 | 2.16 |
| lp3000 | 6 | 100% | 100% | 50% | 1.40 | 1.90 |
| pink+10db | 6 | 83% | 83% | 0% | 1.95 | 15.26 |
| speed0.90 | 6 | 33% | 0% | 0% | 1.61 | 1.70 |
| speed1.05 | 6 | 50% | 0% | 0% | 1.35 | 1.67 |
| white+10db | 6 | 100% | 83% | 0% | 3.15 | 27.96 |
| white+20db | 6 | 100% | 100% | 33% | 1.46 | 3.62 |

Overall: 86.7% / 76.7% / 20.0%, false accepts 0/10, p95 total 31 ms.

Reading: exact-playback recognition is solid even under clipping, echo,
filtering and strong noise; playback-rate changes break both identity and
offset (Engine B motivation); the fixed confidence gate is the single
largest self-inflicted recall loss (calibration motivation).

Update this file after every engine change; keep old snapshots in git
history rather than deleting them.
