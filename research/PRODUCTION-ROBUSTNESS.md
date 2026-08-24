# Production Recognition Robustness Contract

**Status:** active engineering contract  
**Updated:** 2026-08-24

Sivana must not claim to be “better than Shazam” from synthetic examples or
a one-song catalog. That claim is earned only by a blinded, paired benchmark
against the same acoustic captures, with predefined recall, false-accept,
latency, and scale targets.

## Incident that triggered this contract

The 2026-08-24 live session produced 9,968 browser fingerprints but failed to
identify the indexed recording. Two implementation defects were isolated:

1. The matcher deduplicated by hash alone. Later occurrences of the same hash
   at different sample times were discarded, destroying timeline evidence in
   repetitive music. A Shazam-style query is a sequence of `hash + sample time`
   records; only an exactly retransmitted `(hash, time)` pair is a duplicate.
2. Every tiny AudioWorklet batch re-ran a full query over the growing evidence
   window. More than 190 matcher evaluations occurred in under six seconds and
   the server fell behind the browser's 13-second deadline. Full evaluations
   now run on a 250 ms audio-time cadence.

Paired regressions now require a degraded phone-path capture to cross the gate
within six seconds (currently three seconds) and deterministic white/pink room
noise to remain below the gate at every intermediate second, not merely at the
end of the capture.

## What the literature actually supports

- Wang's deployed landmark system uses temporally localized, sufficiently
  entropic spectrogram-peak pairs, retains each sample hash with its time, and
  accepts a candidate when aligned offsets form a statistically significant
  histogram peak. It explicitly calibrates the threshold from the distribution
  of the highest-scoring incorrect track at the intended catalog size.
  [Wang, ISMIR 2003](https://www.princeton.edu/~cuff/ele301/files/Wang03-shazam.pdf)
- The Philips robust-hash line shows that dense, sequential sub-fingerprints
  provide a complementary representation and reports roughly three seconds of
  audio as an identification window.
  [Haitsma and Kalker, ISMIR 2002](https://ismir2002.ismir.net/proceedings/02-FP04-2.pdf)
- Panako demonstrates why a separate invariant engine is useful: constant-Q
  triplets and time ratios tolerate pitch and time-scale changes that exact
  landmark-pair hashes do not.
  [Six and Leman, ISMIR 2014](https://archives.ismir.net/ismir2014/paper/000122.pdf)
- Neural Audio Fingerprinting shows that one-second contrastive embeddings plus
  sequence search can reach 98.9% exact song-level retrieval for six-second
  queries in its 100K-song experiment, while training with background noise and
  room/microphone impulse responses.
  [Chang et al., ICASSP 2021](https://arxiv.org/abs/2010.11910)
- Recent neural work finds that realistic room acoustics, microphone responses,
  degradation construction, and metric-learning choices materially change
  robustness; simplified echo-plus-noise simulation is not an adequate proxy
  for device capture.
  [Araz et al., ISMIR 2025](https://arxiv.org/abs/2506.22661)
- The BAF benchmark is a warning against easy claims: on background-music
  broadcast recognition, the evaluated public systems all remained below 47%
  F1. Production evaluation must include speech, television, and competing
  music, not only isolated songs plus stationary noise.
  [Cortès-Sebastià et al., ISMIR 2022](https://ismir2022program.ismir.net/poster_228.html)

## Target architecture

### 1. Landmark candidate generator

Keep the current sparse peak-pair engine as the fast first stage, but enforce:

- occurrence identity is `(hash, query_anchor_time)`;
- reference posting-frequency suppression for low-entropy hashes, including in
  tiny catalogs where document-frequency IDF cannot discriminate;
- bounded 200–300 ms streaming evaluations;
- offset voting in linear or `N log N` time with no growth-by-requery behavior;
- candidate generation and final acceptance remain separate decisions.

### 2. Calibrated candidate verifier

Replace the hand-authored Boolean gate with a calibrated model over raw evidence:

- unique aligned query occurrences;
- weighted aligned mass and reference hash rarity;
- matched query-time span and coverage;
- offset-peak count, width, residual, and local background distribution;
- strongest disjoint offset and strongest competing recording;
- signal level, fingerprint density, and capture duration;
- agreement between independent time slices;
- agreement between recognition engines.

Start with logistic regression plus isotonic calibration. Report a probability
only after reliability diagrams and expected calibration error show that the
number corresponds to observed outcomes. The operating threshold is chosen from
the allowed false-accept rate, not from a visually convenient percentage.

### 3. Scale-invariant fallback

Retain the Panako-inspired triplet engine for pitch/speed candidates, but do not
let it accept independently until its false-positive distribution separates on
the production catalog. Use it as candidate recall or corroborating evidence.

### 4. Neural embedding fallback

Build a one-second embedding sequence engine when the real-music corpus is
available. Train with held-out background audio, real room impulse responses,
measured microphone/speaker responses, codecs, resampling, AGC, clipping,
dropout, pitch, and speed changes. Use ANN retrieval for candidates and temporal
sequence consistency for verification. The deterministic landmark path remains
the low-latency path; the neural path recovers difficult acoustic cases.

## Benchmark required before a superiority claim

### Corpus

- at least 500 licensed development tracks before model selection;
- at least 10,000 disjoint evaluation tracks, plus a 100,000-track distractor
  index for scale and false-positive measurement;
- no artist, recording, noise, room response, or microphone response may cross
  train/calibration/test splits;
- at least 100 hours of negatives: room tone, speech, television, traffic,
  crowd, out-of-catalog music, and mixtures.

### Acoustic conditions

- query lengths: 1, 2, 3, 5, 10, and 15 seconds;
- SNR: -10 through +20 dB with speech, traffic, crowd, and competing music;
- real speaker-to-microphone captures across phones and laptops;
- measured room and device impulse responses;
- AAC/MP3/Opus/GSM, EQ, band limits, clipping, AGC, packet dropout;
- speed 0.90–1.10, pitch ±2 semitones, and independent time stretch;
- starts sampled across intros, quiet passages, repetitive hooks, and outros.

### Metrics and release bars

- Top-1 track recall and correct-time recall at each query length;
- false accepts per query and a 95% upper confidence bound, not merely “0/N”;
- false rejects, ROC/DET curves, precision-recall, and calibration error;
- time-to-first-correct-match distribution;
- fingerprint real-time factor, query p50/p95/p99, memory, and bytes per audio
  hour at 1, 10K, and 100K tracks;
- streaming prefix safety: a negative query must never cross the terminal gate;
- paired black-box comparison with Shazam on the exact same captures. Any
  “better” claim must name the condition and metric where Sivana wins and must
  publish conditions where it loses.

Initial release targets are: at least 99% Top-1 recall on five-second real-device
in-catalog captures, a measured false-accept upper bound below 1e-5, p95 search
latency below 500 ms at 100K tracks, and no regression in the streaming negative
suite. These are targets, not current results.

## Next engineering sequence

1. Persist anonymized fingerprint-evidence summaries for failed sessions (never
   raw audio) so real failures enter the benchmark automatically.
2. Add query-time span, unique aligned occurrences, peak-background statistics,
   and posting-frequency rarity to `MatchOutcome`.
3. Expand the rejection corpus and run the calibration sweep after every matcher
   feature change.
4. Replace the fixed gate with a versioned calibrated verifier artifact.
5. Add real RIR/device capture evaluation and a public benchmark report.
6. Implement and measure the neural fallback only after the corpus and baseline
   are frozen.

