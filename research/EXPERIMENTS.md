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

## E9. Engine B1 discriminativity levers on a 12-track catalog — Phase 7 closure

- **Date:** 2026-08-25
- **Question:** Do the E5-listed next levers (df-weighted triplet rarity,
  per-band frequency pre-quantization) and a larger catalog separate true
  from false evidence enough to promote Engine B1?
- **Method:** `run_e9_b1_levers` (sivana-bench, `sivana-bench e9`): four
  configurations over one shared 12-track x 20 s synthetic corpus —
  E5 baseline, df-weighted candidate ranking (§14 idf on triplets,
  `query_affine_weighted`), per-band pre-quantization
  (`quant_band_power=2`, geometric band collapse before ratio hashing),
  and both combined. Grid: clean + pink10 + speed0.90 + pitch+2st +
  stretch1.10x, 2 positions/track = 120 in-catalog cases; 5 held-out
  songs x 5 degradations = 25 rejection probes; gate = E5's inliers >= 210.
- **Result:**

  | variant | recall | FA @210 | max rejection inliers | median match inliers |
  |---|---:|---:|---:|---:|
  | e5-baseline | 32.5% | 14/25 | 404 | 279 |
  | df-weighted | 31.7% | 11/25 | 404 | 291 |
  | quantized | 34.2% | 19/25 | 467 | 304 |
  | both levers | 33.3% | 18/25 | 467 | 314 |

- **Findings:**
  1. The failure is structural, not a tuning gap: wrong-audio evidence
     (up to 467 affine-consistent pairs after stop-hash filtering)
     *exceeds* genuine transformed-match evidence (median ~300). No gate
     position can separate overlapping distributions.
  2. Df-weighting trims false accepts only marginally (14 -> 11 of 25)
     without touching the overlap; it cannot fix a hash space whose
     effective entropy is too low for the catalog.
  3. Per-band quantization makes rejection WORSE (FA 19/25, max 467):
     collapsing frequencies to coarse bands raises cross-song hash
     collisions — it densifies postings instead of sharpening identity.
     This mirrors the E2 shared-timbre lesson.
  4. Catalog size alone does not rescue it at 12 tracks; the ~20-bit
     hash space saturates long before df statistics become meaningful.
- **Decision: Phase 7 is CLOSED with B1 not promoted** (second and final
  strike; §85 choose-by-measurement rule). The triplet ratio construction
  achieves its invariance goal but cannot meet the zero-false-accept bar
  on any tested catalog size or lever configuration. Consequences:
  - B1 stays in-tree as an experimental fallback with documented limits;
    `sivana-invariant` gains `query_affine_weighted` + `quant_band_power`
    (kept behind config, defaults preserve E5 behaviour).
  - Speed/pitch/stretch coverage remains an acknowledged gap owned by
    Engine A + WSOLA-tolerance (stretch recall 100% in E5); playback-rate
    queries fall back to NO_MATCH until either (a) a constant-Q triplet
    design with >24-bit entropy is proven offline, or (b) neural trigger
    T1-T3 fires (research/NEURAL-EVAL.md).
  - Any future B2 quad work must first show, offline on synthetic data,
    that its hash space exceeds ~28 bits AND wrong-audio evidence stays
    below half of true-match evidence before implementation begins.
- **Artifacts:** `sivana-bench e9 --tracks 12 --seconds 20` (deterministic,
  reproducible via seed 2026).

## E10. Live false accepts on real audio — margin gate recalibration

- **Date:** 2026-08-25
- **Question:** The E4/E8-calibrated gate (margin >= 2.5) produced
  confident WRONG answers in production: playing MEGALOVANIA returned
  hopes-and-dreams, and out-of-catalog tracks (my-castle-town,
  petal-dance) matched as hopes-and-dreams. Where does the gate actually
  fail on real audio?
- **Method:** Full live-capture evidence sweep through the production
  stack (browser wasm fingerprinter -> WS session -> server gate): 9 real
  Toby Fox tracks (7 ingested + 2 held out), starts {0,30,60,90,120}s x
  durations {4,6,10}s = ~135 cases, each labelled TRUE / FALSE / MISS /
  REJECT from the final session event. Recorded every terminal event's
  verifier features.
- **Result:** All 7 in-catalog tracks self-match correctly at every
  tested position (ingest, index, metadata all sound). Every observed
  FALSE accept came from OUT-of-catalog audio winning as
  hopes-and-dreams or MEGALOVANIA with weak-but-gate-clearing evidence:

  | case | inliers | conc | uniq | span | margin |
  |---|---:|---:|---:|---:|---:|
  | petal-dance->h&d @0 | 25 | 1.00 | 25 | 30 | 2.72 |
  | petal-dance->h&d @60 | 12 | 0.57 | 12 | 27 | 2.52 |
  | castle-town->h&d @0 | 15 | 1.00 | 15 | 9 | 2.80 |
  | castle-town->h&d @90 | 20 | 0.70 | 20 | 66 | 2.57 |
  | castle-town->h&d @120 | 12 | 0.67 | 12 | 32 | 2.57 |

  Weakest TRUE matches: core@60 margin **2.75**, everything else >= 3.79.
- **Findings:**
  1. Root cause is the same-franchise collision mode E8 identified, now
     observed at production scale: Toby Fox tracks share instrumentation
     and arrangement style, so out-of-catalog audio accumulates aligned
     hash collisions against the *most fingerprint-dense* catalog track.
     Margin sits just above the old floor; the server locks in a
     terminal ConfidentMatch before stronger evidence arrives.
  2. Margin ALONE cannot separate the worst cases: false 2.72 vs true
     2.75 overlap. But concentration (0.57–1.00), uniqueness (=inliers),
     and span (9–66) of the false accepts are all indistinguishable
     from true matches too — no secondary feature rescues the 2.75 true
     outlier without re-admitting measured false accepts.
  3. The distributions DO separate with one exception: false band
     [2.52, 2.80], true mass [2.75, ∞) but only one true point below
     3.79. A floor of 3.0 rejects all five false accepts and trades away
     exactly one weak true case.
- **Decision: GATE_MIN_MARGIN 2.5 -> 3.0.** A rare miss (the query can
  still succeed with more capture time or a cleaner excerpt) beats a
  confident wrong answer, which destroys product trust. Documented as an
  accepted tradeoff; revisit if real-world recall complaints appear at
  margins in [2.75, 3.0).
- **Artifacts:** sweep harness + raw JSON in job tmp dir;
  recognition.rs carries the calibrated constants with rationale.

## E11b. Acoustic-channel failure: phone-speaker EQ loss vs the solo density gate

- **Date:** 2026-08-25
- **Question:** After E11, a user playing MEGALOVANIA over PHONE SPEAKERS
  into the microphone still got NO_MATCH, even though clean-file queries
  matched instantly and ingestion was verified correct. What does the
  acoustic channel do to the evidence?
- **Method:** Simulated the channel on the query signal only
  (2nd-order Butterworth 300 Hz highpass = small-speaker bass loss;
  + pink noise at 12/20 dB SNR; + room slapback) and streamed through
  the production WS path. Instrumented per-evaluation candidate events,
  plus offline offset-histogram analysis of query-vs-catalog hashes.
- **Result:** Highpass alone is the killer — adding noise on top MATCHES.
  The start=60s trace shows why:

  | capture | inliers | conc | span | density | state |
  |---|---:|---:|---:|---:|---|
  | 4 s | 94 | 1.00 | 46 | 2.0 | candidate |
  | 8 s | 217 | 1.00 | 89 | 2.4 | candidate |
  | 12 s | 268 | 1.00 | 149 | 1.8 | **no_match (timeout)** |

  Alignment stays PERFECT (conc 1.0, hundreds of inliers) but arrives at
  half the clean-audio rate because bass removal strips the loudest
  landmarks. Cumulative density = inliers/(span+1) therefore *decays*
  toward ~1.8-2.4 as capture continues — below the E11 floor of 2.5.
  Longer listening made acceptance HARDER. Offline histograms confirm:
  hp300 keeps 28% hash overlap with clean, spread across cyclic-riff
  offsets rather than lost entirely.
- **Findings:**
  1. Density measured cumulatively conflates alignment quality with
     capture duration. Junk densities top out at 1.36; degraded-but-true
     steady-state sits at ~1.7-2.5. The 2.5 floor sat INSIDE the true
     band (same calibration error class as E10's margin floor).
  2. Noise helping is diagnostic, not paradoxical: it decorrelates the
     weak spurious cyclic alignments while the true alignment survives,
     restoring concentration/density headroom.
- **Decision: GATE_SOLO_MIN_DENSITY 2.5 -> 1.5** (mid-band between junk
  <=1.36 and degraded-true >=~1.7). Inlier floor stays 30; conc stays
  0.8. Validated live: hp300 matches 6/6 positions (was flaky/never),
  out-of-catalog tracks still reject.
- **Artifacts:** ws-probe.js / vote-split.js harnesses in job tmp dir;
  gate constants carry the measured rationale inline.



## E11c. Solo-catalog confirmation: growth + absolute mass floor

- **Date:** 2026-08-25
- **Question:** E11b's density floor of 1.5 admitted a NEW false accept:
  MEGALOVANIA's motif is reprised inside Lost Girl, so a lost-girl query
  accumulates REAL catalog hits at conc 1.0 (density 2.06, 33 inliers by
  12 s). Lowering density to admit degraded-true audio also admitted
  melodic quotation. Can one solo-catalog gate separate
  {junk scatter, quotation} from {clean true, phone-EQ-degraded true}?
- **Measured bands** (live WS traces, single-track catalog):

  | population | density | conc | inliers @ 12 s |
  |---|---|---|---:|
  | junk scatter | <=1.36 | <0.8 | ~40 |
  | lost-girl quotation | 2.06 | 1.00 | 33->110 ceiling |
  | degraded true (hp300) | 1.95-2.5 sustained | ~1.0 | >=264 |
  | clean true | 4-9 | ~1.0 | >=1000 |

- **Why growth alone failed:** the reprise reading eventually dominates
  with conc 1.0 and monotone growth — a growing-but-small match is
  indistinguishable from a growing true match without an absolute scale.
- **Decision: two-stage gate.**
  1. *Arm* when floors clear: inliers>=30, conc>=0.8,
     density>=2.0, unique_aligned>=GATE_MIN_UNIQUE_ALIGNED. Sticky per
     recording; disarmed if top candidate changes.
  2. *Confirm* only when elapsed>=GATE_SOLO_CONFIRM_SECONDS (2.0),
     inlier growth since arming>=GATE_SOLO_CONFIRM_GROWTH (8), AND
     inliers>=GATE_SOLO_CONFIRM_MIN_INLIERS (**150**).
  Density floor back to 2.0: safe now because acceptance no longer rests
  on it alone; the 150-mass requirement does the separation. Quotation
  tops out around ~110 inliers on this corpus (reprises are short);
  degraded-true reaches 220-700 within MAX_CAPTURE_SECONDS.
- **Validation matrix** (15/15): hp300 true matched at starts
  {0,30,60,90,120} (mass 220-697); lost-girl@{0,30} rejected;
  all 7 out-of-catalog tracks rejected; clean true matched (1097).
  Unit tests pin all three populations: dense_solo_...matches (confirm
  reached), stalled_solo_...does_not_confirm (growth w/o mass),
  sparse_solo_...stays_a_candidate (never arms).
- **Known limit:** thresholds calibrated on this corpus; a second catalog
  track switches the session to the margin gate (E10), which separates
  cleanly. Documented for recalibration as the catalog grows.

## E12. Real-channel failure: room tone out-votes the song

- **Date:** 2026-08-25
- **Question:** E11c's gate passed every simulated probe, yet the REAL
  path (phone speakers -> laptop mic) never matched. Is the acoustic
  channel what the hp300 simulation assumed?
- **Method:** Physical captures on this machine (ffmpeg dshow, Realtek
  mic array; playback via SoundPlayer), fingerprinted offline + through
  the live WS at REALTIME pacing (the wall-clock confirm stage makes
  faster-than-realtime probes invalid — a 1.3s burst can never satisfy
  a 2s elapsed requirement).
- **Measured:** pure ROOM TONE emitted 1084 fingerprints/s vs 1117 for
  the actual song — the peak picker had no stationarity concept, and
  stationary noise peaks cleared the per-frame median prominence test as
  easily as music. Silence banked up to 50 aligned inliers against the
  repetitive synth catalog (self-alignment of hum with cyclic riffs);
  the true far-field capture banked only 34. The negative band OVERLAPPED
  AND EXCEEDED the true band: no threshold on those features could work.
- **Failed first fix:** asymmetric-EMA background (rise tau 8 / fall
  tau 43 frames). Measured ZERO fingerprints from a real -39 dB music
  window while post-playback AGC noise bursts emitted thousands — a fast
  rise-tau absorbs any note longer than the tau, so musical sustain
  self-cancels. Inverted discrimination.
- **Fix: minimum-statistics whitening** in PeakStreamer. Each candidate
  peak must exceed its own bin's MINIMUM level over a trailing ~4 s
  window by 6 dB. Music constantly dips below its own peaks (decays,
  vibrato, note changes), refreshing the floor, so sustained tones
  survive; stationary content never dips and stays pinned at the floor,
  rejected. Per-frame floor snapshots keep streaming == batch exactly.
  Both ingest and query share the pipeline -> catalog re-ingested
  (162948 -> ~100k fps; stationary/sustained content legitimately yields
  fewer landmarks).
- **Result:** room tone phantom evidence collapsed to <=10 inliers.
  BUT quotation evidence (lost-girl) survived — it is genuine MUSICAL
  alignment, not noise — so the solo gate needed a new discriminator.

## E13. Solo gate: alignment tightness separates truth from quotation

- **Date:** 2026-08-25
- **Method:** full per-evaluation feature traces under the E12 engine,
  realtime pacing. Mass alone CANNOT separate (negatives reach 65-67 vs
  true 76). The discriminator is offset_concentration measured inside
  the winning +-2-frame bucket:
- **Bands:**

  | population | conc | peak inliers |
  |---|---|---:|
  | true playback (clean/hp300 file) | **0.98-1.00 from first eval** | 76-495 |
  | lost-girl quotation | <=0.88 | 67 |
  | spider-dance shared-patch collision | <=0.904 | 94 |
  | other negatives + room tone | scattered | <=27 |

  True playback reproduces the catalog's exact sample timing, so nearly
  all votes land in one offset bucket; quotation smears (tempo drift).
- **Gate:** conc >= 0.95 (centered in the (0.904, 0.98) gap) with either
  a young spike (>=20 inliers while span <=16 — matches within ~0.5 s)
  or mass >= 64. Arm/confirm machinery retired (growth was a ceiling
  trap: arming late on a saturated window can never grow +8; density
  punished capture length).
- **Validation 15/15** (realtime-paced live WS): megalovania clean +
  hp300 matched at all tested positions {0,30,60,90,120}; lost-girl,
  spider-dance, hopes-and-dreams, my-castle-town, petal-dance, core,
  asgore, fallen-down, room tone all rejected.
- **Environment caveats found en route** (this machine): Realtek mic
  array APO applies aggressive AGC that ducks sustained music toward
  the noise floor (capture RMS swings -38.5 -> -57 dB mid-song);
  ffplay playback routes into VoiceMeeter and never reaches speakers.
  Physical far-field captures remain below-floor (correct rejections):
  their aligned evidence after whitening is genuinely tiny. The browser
  pipeline itself was verified end-to-end healthy via headless Edge +
  fake-mic WAV replay (matched even harshest phone-proxy profiles).

## E12b. Spectral dust: the failure minimum-statistics cannot see

- **Date:** 2026-08-25
- **Discovery:** after E12, a PURE steady sine still emitted thousands of
  landmarks (8780 late anchors on a two-tone fixture). The leaks sit at
  f1 485-511 — near-Nyquist FFT far-sidelobe / rounding bins whose
  magnitudes wobble tens of dB frame-to-frame with DETERMINISTIC phase
  advance. A wobble is indistinguishable from signal to any temporal
  floor tracker: the min stays low, every wiggle reads as above-floor.
- **Fix: relative ceiling floor** (`relative_floor_db = 35`): a candidate
  must also sit within 35 dB of its frame's strongest bin.
  Gain-invariant; measured margins are wide (real musical peaks well
  inside, dust 60+ dB down). rel>=25 zeroes the two-tone leak.
- **Perf:** eager per-frame floor computation O(window*bins) broke CI's
  realtime-factor floor. The trailing-minimum is now computed LAZILY in
  decide() for local-max candidate bins only (~20/frame) — identical
  results, ~50x less work.
- **Contract probes:** golden digest re-pinned (0xA449_337C); tone tests
  rewritten for adaptivity: looping note sequences must keep emitting
  with correct 512-band mapping late in the stream; steady tones must go
  fully silent after the window settles. Shallow AM is NOT sufficient
  non-stationarity (whole spectrum wobbles together and the floor tracks
  it) — note onsets are the honest probe.

## Environment record: this dev machine's audio stack

- VoiceMeeter + SplitCam + Voice.ai virtual devices; ffplay playback
  routes into VoiceMeeter and never reaches physical speakers (verify
  captures with volumedetect BEFORE fingerprinting them).
- Realtek mic array APO applies aggressive AGC that ducks sustained
  music toward the noise floor mid-capture (-38.5 -> -57 dB RMS swings);
  browser constraints cannot disable OS-layer processing (research:
  Chromium tries AUDCLNT_STREAMOPTIONS_RAW with graceful fallback).
- Physical speaker->mic captures remain below-floor rejections under the
  final engine: correct, given their aligned evidence after whitening is
  genuinely tiny (~5-13 inliers). The browser path itself was verified
  end-to-end healthy via headless Edge + fake-device WAV replay of the
  harshest phone-proxy profiles (all match).
