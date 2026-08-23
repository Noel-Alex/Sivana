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

## Queued studies (run when scheduled; numbered as reached)

- Target sample-rate sweep {8k, 11.025k, 16k, 22.05k} on the same grid (§7).
- Fanout sweep {5,8,10,12,15} measuring recall per byte of index (§12).
- Hash width 24/32/40-bit collision + latency study (§13).
- Sliding-max peak detector: equivalence vs brute force + speedup (§9.1).
- Quadratic peak interpolation: does it improve cross-degradation stability? (§9.5)
- Time-stretch (WSOLA/phase-vocoder) axis independent of pitch (§49).

## E2. Landmark V2 first cut vs legacy (A/B)

- **Date:** 2026-08-22
- **Method:** cargo run -p sivana-bench --release -- run --tracks 3 --seconds 15; engines legacy vs landmark-v2 (PeaksV2 + scored target zones + 32-bit hashes + flat IDF matcher).
- **Result:** v2 track recall 63.3% / offset 41.7% vs legacy 90/76.7 on clean+degradations; match latency 0.5 ms vs 6.2 ms (flat voting already ~12x faster); fingerprint cost 13 ms vs 1.6 ms (per-frame median sort dominates - optimization target).
- **Finding:** out-of-catalog false accepts 10/10 for v2 first cut. Root cause hypothesis: synthetic fixtures share timbre structure, so (f1,f2,dt) collisions across songs are real, not noise; frequency band mapping (linear-in-log over full range) too coarse.
- **Next:** peak-strength weighting in target scoring; band table benchmark sweep; stop-hash df stats once catalog >100 tracks; distinct per-seed timbres in fixtures.


### E2a. Peak-strength weighting in target scoring
- **Change:** target score = 0.5*df + 64*(magnitude/global_max); Peak now carries magnitude.
- **Result:** track recall unchanged (63.3%), offset 40.0% (was 41.7%) - neutral within noise on the 60-case grid.
- **Decision:** keep (harmless, principled per SS11); the dominant recall limiter is hash collision structure, so next lever is band-table design and timbre-diverse fixtures before more scoring weights.

## E3. Timbre-diverse fixtures + band-table sweep

- **Date:** 2026-08-23
- **Question:** (a) Was E2's 10/10 false-accept rate an artifact of shared
  fixture timbres? (b) How does log-band quantization granularity trade
  recall against out-of-catalog rejection?
- **Also shipped in this run (confounds direct comparison with E2/E2a):**
  streaming landmark pipeline (constant memory, batch==streaming by
  construction), matcher distinct-recording df + deterministic ranking,
  strength term switched from global-max normalization to gain-invariant
  frame prominence (capped 60 dB), quantize_bin fix (old mapping used only
  half the band field; octaves now derived from FFT size).
- **Method:** same command shape as E1/E2 (`run --tracks 3 --seconds 15`,
  seed 2026, 60 cases) with new `--bands 64,128,256,512`. Fixtures now
  derive harmonic mix, brightness and layer balance per seed.
- **Result (track/offset/gated %, false accepts /10):**

  | engine | track | offset | gated | FA | fp ms | match ms |
  |---|---:|---:|---:|---:|---:|---:|
  | legacy | 86.7 | 78.3 | 35.0 | 0/10 | 2.3 | 9.0 |
  | v2-b64 | **96.7** | 60.0 | 96.7 | 10/10 | 10.4 | 0.4 |
  | v2-b128 | 93.3 | 61.7 | 93.3 | 10/10 | 10.6 | 0.3 |
  | v2-b256 | 90.0 | 55.0 | 90.0 | 9/10 | 16.3 | 0.4 |
  | v2-b512 | 93.3 | 56.7 | 91.7 | **3/10** | 16.5 | 0.4 |

- **Findings:**
  1. Shared timbres were a major confound: with diverse fixtures V2 track
     recall jumps 63.3 -> 90-96.7% and now BEATS legacy (96.7 vs 86.7).
  2. Band granularity is a recall/rejection dial: coarse bands (64)
     maximize degraded-audio track recall; fine bands (512) cut false
     accepts 10 -> 3. No single setting wins both.
  3. Legacy still wins offset accuracy (78.3 vs ~60) — V2's exact-offset
     voting is brittle; tolerance bucketing (§24) is the fix, not more
     band tuning.
  4. V2 match latency stays ~20x better than legacy; fingerprint cost
     ~4-7x worse (bench box under ~70% background load; absolute ms
     unreliable, rerun on idle machine before perf claims).
- **Decisions:**
  1. Keep timbre-diverse fixtures as the new baseline corpus (E2's FA
     diagnosis was fixture-driven).
  2. Default band count: 256 stays default for continuity, but b64 is the
     recall-optimal and b512 the rejection-optimal corner — revisit after
     Phase 2 calibration, which should let us run fine bands + calibrated
     gate instead of trading one for the other.
  3. Next lever is matcher verification (offset tolerance, §24) and the
     confidence-calibration harness — band sweeps alone are exhausted.
- **Artifacts:** bench-work/baseline.json (legacy),
  baseline.v2-b{64,128,256,512}.json, BASELINE.md, COMPARISON.md.
