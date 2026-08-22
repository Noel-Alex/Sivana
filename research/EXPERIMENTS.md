# Experiments

Every experiment gets: question, method, result, decision. No folklore.
Append new entries at the bottom; never delete history.

## E1. Legacy baseline vs degradation matrix

- **Date:** 2026-08-22 (Phase 0 milestone)
- **Question:** Where does the frozen prototype actually fail?
- **Method:** `cargo run -p sivana-bench --release -- run --tracks 3 --seconds 15`
  (seed 2026, 8 s excerpts, 10-cell degradation grid, 60 cases).
- **Result:** see BENCHMARKS.md snapshot. Highlights:
  - clean / clip / echo / LP / HP: 100% track recall
  - white noise +20/+10 dB: 100%; pink +10 dB: 83%
  - **speed 0.90: 33%, offset recall 0%; speed 1.05: 50%/0%**
  - legacy score gate (>=100) passes only ~20% of correct matches
  - out-of-catalog false accepts: 0/10; mean match 6.2 ms
- **Decisions:**
  1. Engine B (scale-invariant) is justified by measured failure, not
     aesthetics (§27).
  2. Confidence calibration is mandatory — the hard gate destroys recall
     while rejection stays perfect, i.e. the gate is simply wrong (§26).
  3. Peak detector and matcher rewrites must be validated against this
     exact grid.

## E2. TODO queue

- Target sample-rate sweep {8k, 11.025k, 16k, 22.05k} on the same grid (§7).
- Fanout sweep {5,8,10,12,15} measuring recall per byte of index (§12).
- Hash width 24/32/40-bit collision + latency study (§13).
- Sliding-max peak detector: equivalence vs brute force + speedup (§9.1).
- Quadratic peak interpolation: does it improve cross-degradation stability? (§9.5)
- Time-stretch (WSOLA/phase-vocoder) axis independent of pitch (§49).
