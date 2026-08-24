# Algorithms

Working notes on the algorithms Sivana uses and plans to use. Each entry
states the idea, its parameters, and what must be benchmarked before it
ships (PLAN.md §92: no parameter without benchmark justification).

## A. Landmark engine (legacy + V2)

### Legacy pipeline (frozen control)
```
mono -> STFT(2048, hop 1024, Hann symmetric) -> magnitude
     -> brute-force 2D local maxima (t-radius 2, f-radius 5, mag >= 2.0)
     -> first-5 pairs per anchor within dt [1..50], df <= 200 bins
     -> hash = f1:10 | f2:10 | dt:8  (28 bits in u64)
     -> sqlite lookup + nested HashMap offset voting, gate score >= 100
```

Measured baseline (see BENCHMARKS.md): collapses under speed change;
hard gate rejects most correct matches; O(T*F*Wt*Wf) peak detection.

### Planned V2 changes (Phase 1)
1. **Streaming DSP** — ring buffer + preallocated FFT frames; constant
   memory vs source duration (§4.1).
2. **Separable sliding-max peaks** — max-filter along frequency then time
   via monotonic deques: `O(T*F)` instead of `O(T*F*W)` (§9.1). Peak iff
   center == window max AND passes prominence test.
3. **Adaptive noise floor** — replace absolute magnitude threshold with a
   moving percentile/EMA baseline per band; accept on prominence in dB
   (§9.2).
4. **Density control** — target 20–60 peaks/s total with per-band quotas;
   prevents bass/percussion domination (§9.3–9.4).
5. **Scored target selection** — rank candidates by strength, spectral +
   temporal separation, stability, rarity prior; spread across the zone
   instead of taking the first N (§11).
6. **32-bit hashes** — `f1:12 | f2:12 | dt:8` enabling the high-16 bucket
   directory (§13, §19).

## B. Matching (Phase 2)

- Sort/dedup query hashes; drop stop-hashes (df above threshold §15).
- IDF weighting `w(h) = log((N+1)/(df(h)+1))` (§14).
- Flat vote tuples `(recording_id, offset_bucket, weight)` accumulated in
  contiguous storage; candidate shortlist by weighted mass.
- Geometric verification on shortlist: fit `t_db = a*t_q + b`, count
  weighted inliers, margin over second-best (§24).
- Confidence = calibrated function of match features (§26), not a bare
  threshold.

## C. Scale-invariant engine (Phase 7)

Triplet events `(p1,p2,p3)`: hash includes frequency ratios and the
temporal ratio `Rt=(t2-t1)/(t3-t1)`, invariant to global speed scaling.
Quads additionally estimate the scale factors themselves. Both engines emit
the same posting structure as Engine A so the matcher is shared.

## D. Index (Phase 3)

LMDB (`heed`) first; then immutable mmap segments `.siv`:
high16 directory of bucket offsets, binary search on low16, contiguous
postings `recording_id:32 | anchor_time:24 | flags:8` (§17–21).
