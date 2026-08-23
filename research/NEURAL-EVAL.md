# Neural Engine Evaluation (Phase 8, PLAN §86)

**Date:** 2026-08-23 · **Status:** evaluated — deferred with explicit
trigger criteria. No model training was performed; this document is the
measured decision record §86 requires before any neural engineering starts.

## 1. Quantified deterministic-engine failure classes

From E1-E5 (78-case grid, seed 2026; see EXPERIMENTS.md):

| failure class | best deterministic result | evidence |
|---|---|---|
| playback speed ±5-10% | v2-b512 67% raw identity | E5: speed cells |
| independent pitch shift ±2 st | not covered by A at operating point | E5: legacy 33-67%, B1 33-67% |
| time stretch 1.10x | A/legacy survive (~100% raw) | E5: stretch cell |
| noise to -SNR | solved by A at +10 dB SNR | E3/E5 |
| out-of-catalog rejection | solved by calibrated gate (0 FA) | E4 |

Reading: the *only* unsolved classes are playback-rate and true pitch
shift. B1 attacks both by construction but currently fails the zero-FA
bar (E5) for discriminativity reasons, not invariance reasons.

## 2. Where a neural component would earn its cost

Ranked by (failure-class value ÷ integration cost):

1. **Learned pair verifier** — binary classifier over candidate
   evidence (aligned hash statistics, residual structure) replacing the
   hand-calibrated gate. Small input space, trains on bench-generated
   pairs, plugs into the existing matcher as a score. Does not need to
   run per-frame; runs once per candidate. Cost: ~100 KB model, µs-scale
   inference via Candle/Burn or even logistic regression (no NN runtime).
2. **Contrastive segment embeddings (Engine C)** — addresses pitch shift
   directly. Cost: training pipeline + dataset curation (licensed audio,
   §42), MB-to-GB index growth (one vector per segment), ANN index
   maintenance alongside .siv. This is a product-line decision, not an
   optimization.
3. **Neural peak selection** — replace PeaksV2 acceptance with a learned
   selector. Highest risk: breaks cross-platform determinism guarantees
   (§36) unless inference is fixed-point/bit-exact. Rejected for now.

## 3. Runtime/storage cost model (order-of-magnitude)

- Verifier: logistic/GBDT-class model → deterministic f32 math, fits the
  existing calibration harness (`sivana-bench calibrate`) unchanged.
- Embedding ANN: 1 embedding ≈ 64-256 floats ≈ 256 B-1 KB per 2 s
  window → ~30x the .siv posting cost per second of audio; HNSW query
  adds ~O(log n · deg) distance evals vs today's ~23 ns posting lookups.

## 4. Decision

Defer Engine C. Proceed ONLY when any of these triggers fire:

- T1 — B1's discriminativity is fixed (df-weighted triplets, wider
  prints) AND transformed-audio recall still < 80% on the standard grid
  at zero FA. (Verifier may be built earlier if gate features plateau.)
- T2 — a licensed evaluation set (§42) of real music ≥ 500 tracks is
  available, making contrastive training measurable rather than synthetic.
- T3 — a production requirement demands pitch-shifted recognition with
  calibrated confidence inside one second of capture.

Until then, deterministic engines remain the entire recognition path, and
every claim about neural benefit stays hypothetical — as §86 intends.

## 5. First experiment when triggered (E6 protocol)

Dataset: 500+ licensed tracks, 50-query-per-class matrix (speed {0.9,
1.05}, pitch {±1, ±2 st}, stretch {1.05, 1.15}, noise {-5, 0 dB}).
Baseline rows: A(b512), B1(current). Treatment rows: +verifier,
+embedding re-rank@10. Metrics: recall@1, gated recall at 1e-5 FA,
p99 server latency, bytes/s of catalog. Success: ≥ +10 pts gated recall
on transformed classes at equal FA and < 2x p99 latency.
