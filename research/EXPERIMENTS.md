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

## E4. Gate calibration: measured operating points (Phase 2)

- **Date:** 2026-08-23
- **Question:** What acceptance gate maximizes gated recall at zero
  false accepts, replacing hand-picked thresholds (§26)?
- **Method:** runner records rank-1 matcher features (inliers, offset
  concentration) per case + per out-of-catalog probe; `sivana-bench
  calibrate` sweeps `inliers >= a AND concentration >= b` offline over a ∈
  [1,12], b ∈ {0.0..0.9}; bands {64,256,512} × offset tolerance {0,2}.
- **Result (recommended zero-FA points):**

  | bands | tol | gate | gated recall | FA |
  |---|---:|---|---:|---:|
  | 64 | any | none exists — coarse hashes cannot separate | — | — |
  | 256 | 0 | a=9, b=0.90 | 70.0% | 0/10 |
  | 512 | 0 | a=7, b=0.90 | 75.0% | 0/10 |
  | **512** | **2** | **a=7, b=0.50** | **76.7%** | **0/10** |

- **Findings:**
  1. A measured zero-FA gate more than doubles legacy's gated recall at
     its own zero-FA point (76.7% vs 35%).
  2. Offset tolerance helps twice over: +1.7% recall AND allows halving
     the concentration requirement (0.90 -> 0.50).
  3. Coarse bands (64) have no separating gate at all: their hashes
     collide across songs, so evidence never separates correct matches
     from strangers. Fine bands are mandatory once rejection matters.
- **Decisions:**
  1. Adopt bands=512 + tolerance=2 + gate(a=7,b=0.5) as the V2 default
     operating point; MatchParams::default() tolerance now 2.
  2. Runner gates use the E4 constants; recalibrate after any engine
     change (the sweep is one command).
  3. Phase 2 exit criteria met on this grid: calibrated FPR (zero FA at
     max recall), latency banked, stable rejection.
- **Artifacts:** bench-work/CALIBRATION.md.

## E5. Engine B1 (scale-invariant triplets) vs Engine A on the
speed/pitch/stretch axis

- **Date:** 2026-08-23
- **Question:** Do triplet ratio invariants (§28) beat the landmark pair
  engine on playback-rate and pitch transformations, and can they meet the
  zero-false-accept bar?
- **Method:** new `sivana-invariant` crate — triplets over V2 peaks hashing
  quantized log-frequency ratios + time-gap ratio (±1-bucket neighbour
  variants for frame-rounding jitter); candidates shortlisted by hash
  votes, verified by robust affine fit `t_db = a*t_q + b`. New bench
  axes: `PitchShift` (resample+WSOLA-restore) and `TimeStretch` (WSOLA,
  new `sivana-dsp::wsola`). Grid: 13 cells x 6 = 78 cases, seed 2026.
- **Result (track recall % per cell):**

  | cell | legacy | v2-b512 | b1 |
  |---|---:|---:|---:|
  | clean | 100 | 100 | 100 |
  | clip / echo / hp / lp | 100 | 100 | 50-100 |
  | pink+10db | 100 | 100 | 33 |
  | white+10/20db | 100 | 100 | 33 |
  | speed0.90 | 33 | 67 | 67 |
  | speed1.05 | 33 | 67 | 67 |
  | pitch+2st | 33 | — | 67 |
  | pitch-2st | 67 | — | 33 |
  | stretch1.10x | 100 | — | 67 |

  Overall: legacy 82.1%, v2-b512 92.3%, b1 65.4% (raw identity).
  **Rejection: b1 false accepts 13/13 at every plausible gate** before
  stop-hash filtering; after filtering (§15: hashes present in every
  recording dropped) rejection evidence falls 8x (max inliers 842 -> 205)
  but the zero-FA operating point sits at inliers >= 210, yielding only
  26.9% gated recall with 0% on all transformed cells.
- **Findings:**
  1. The invariance mechanism works: b1 is the only engine whose hashes
     collide across speed/pitch transformations by construction, and it
     ties/beats v2 on speed0.90 raw identity.
  2. Discriminativity, not invariance, is the failure: on a 3-track
     synthetic catalog the effective ~20-bit hash space saturates —
     out-of-catalog audio amasses hundreds of affine-consistent pairs,
     overlapping the evidence of genuinely transformed matches.
  3. WSOLA stretch degrades Engine A far less than resample-speed does
     (legacy stretch 100% vs speed 33%): pitch is preserved, so pair
     hashes survive; only dt warps.
- **Decision: Engine A (Landmark V2) stays the primary engine; B1 is NOT
  promoted** (§85: choose by measured recall + false accepts). B1 is kept
  as the experimental fallback with its calibration documented. Next
  levers, in order: df-weighted rarity for triplets (not just stop-hash),
  per-band frequency quantization before ratio hashing (kills low-bin
  rounding sensitivity), and evaluation on >50-track catalogs where df
  statistics become meaningful.
- **Artifacts:** bench-work/baseline.b1.json, baseline.json,
  baseline.v2.json (13-cell grid).

## E6. Neural engine evaluation (Phase 8) — decision record

- **Date:** 2026-08-23
- **Scope:** §86 evaluation only; no training performed.
- **Method:** failure classes quantified from E1-E5; neural integration
  points ranked by failure-class value vs integration cost; runtime and
  storage cost modeled; explicit trigger criteria defined.
- **Decision:** defer Engine C. Deterministic engines cover every failure
  class except playback-rate/pitch shift; B1 owns that class pending its
  discriminativity fix. Neural work resumes only on trigger T1-T3
  (see research/NEURAL-EVAL.md), with the E6 measurement protocol fixed
  in advance so a future implementation is judged against a pre-registered
  baseline.
- **Artifacts:** research/NEURAL-EVAL.md.
