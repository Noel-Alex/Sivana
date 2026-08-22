# Sivana
## Production-Grade Rust Audio Recognition System

**Project type:** High-performance audio fingerprinting and recognition engine  
**Primary implementation language:** Rust  
**Initial product:** Website  
**Later product:** Chrome extension  
**UI direction:** Editorial magazine / premium music publication  
**Core principle:** Build the recognition engine first, then make every product surface reuse it.

---

# 1. Vision

Sivana should become far more than a small "Shazam clone written in Rust."

The goal is to build a production-grade, extremely high-performance audio recognition system with a shared Rust core that can run natively and in WebAssembly.

The target end-state is:

> **Sivana = a Rust-native, streaming, multi-engine audio recognition system with a shared Rust/WASM fingerprinting core, designed for extremely low recognition latency, short noisy recordings, and optional resilience to pitch, speed and time-scale changes.**

The product should eventually support:

- extremely fast exact-recording recognition
- short noisy queries
- room acoustics
- lossy codecs
- microphone capture
- browser-tab capture
- low-bandwidth fingerprint-only queries
- speed-up / slow-down recognition
- pitch-shifted audio
- time-stretched audio
- multiple simultaneous candidates
- very large reference catalogs
- private/user-owned catalogs
- statistically calibrated confidence
- continuous/streaming recognition
- a high-performance Rust API
- local browser fingerprint generation using Rust/WASM
- a Chrome extension using the same engine
- reproducible benchmarks proving every optimization

The project should not optimize for "looks impressive on GitHub."

It should optimize for measurable:

- recall
- precision
- false-positive rate
- time-to-result
- throughput
- memory efficiency
- index size
- robustness
- cross-platform determinism

---

# 2. Product Philosophy

Sivana should be developed in this order:

```text
recognition science
        ↓
benchmark harness
        ↓
high-performance engine
        ↓
production index
        ↓
browser/WASM client
        ↓
website
        ↓
catalog tooling
        ↓
scale-invariant engine
        ↓
extension
        ↓
very large-scale deployment
```

Do not begin by making a beautiful landing page around mediocre recognition.

The engine must earn the UI.

---

# 3. Existing Sivana

The existing repository already contains the correct broad family of algorithm.

Current pipeline:

```text
audio file
    ↓
decode
    ↓
mono conversion
    ↓
resample
    ↓
STFT / spectrogram
    ↓
spectral local maxima
    ↓
anchor / target landmark pairs
    ↓
(f1, f2, Δt) hashes
    ↓
SQLite lookup
    ↓
time-offset histogram
    ↓
best match
```

Current important crates:

- `rustfft`
- `symphonia`
- `rubato`
- `rusqlite`
- `clap`

The old code should **not** be deleted.

Freeze it as:

```text
legacy/
```

or retain it as a benchmark branch.

It becomes the control implementation against which all future work is measured.

---

# 4. Why the Current Implementation Must Be Rebuilt

The prototype is useful, but several components are unsuitable for production-scale recognition.

---

## 4.1 Full Spectrogram Allocation

The existing approach constructs an entire spectrogram in memory.

Production Sivana should instead run as a stream.

Target architecture:

```text
PCM stream
   ↓
ring buffer
   ↓
FFT frame
   ↓
log-power bins
   ↓
peak detector
   ↓
landmarks
   ↓
discard old frame
```

Memory should remain approximately constant regardless of source duration.

---

## 4.2 Brute-Force Peak Detection

The current peak detector compares every candidate with an entire two-dimensional neighborhood.

Approximate cost:

```text
O(T × F × Wt × Wf)
```

where:

- `T` = time frames
- `F` = FFT frequency bins
- `Wt` = temporal neighborhood
- `Wf` = frequency neighborhood

This can be replaced by separable sliding-window maximum filters.

Target:

```text
O(T × F)
```

or close to it.

---

## 4.3 Fixed Peak Thresholds

An absolute threshold such as:

```text
magnitude > 2.0
```

will behave differently across:

- microphones
- phones
- recording levels
- clipping
- compression
- EQ
- source volume
- codecs
- normalization

Production peak selection should use local/adaptive statistics.

---

## 4.4 Naive Landmark Pairing

The prototype selects the first few eligible future peaks.

A better target-selection system should deliberately choose fingerprints that maximize:

- stability
- spectral diversity
- temporal diversity
- discriminative power
- rarity
- survival under degradation

---

## 4.5 SQLite in the Recognition Hot Path

The current architecture performs something conceptually similar to:

```sql
SELECT song_id, anchor_time_idx
FROM fingerprints
WHERE hash = ?;
```

for every query fingerprint.

That works for a toy catalog.

At millions of recordings and billions of fingerprints it becomes fundamentally the wrong storage/access model.

---

## 4.6 Nested HashMap Voting

Current matching uses a structure conceptually similar to:

```rust
HashMap<SongId, HashMap<Offset, Count>>
```

This causes:

- allocation overhead
- pointer chasing
- cache misses
- poor SIMD opportunities

Production matching should favor contiguous, compact data.

---

## 4.7 Hard-Coded Match Confidence

A constant match threshold has no statistical meaning.

Production confidence should be calibrated against measured distributions of:

- true matches
- near matches
- false matches
- out-of-catalog audio

---

## 4.8 Whole-File Decode and Resample

Live recognition and large-scale ingestion should both support streaming chunks.

We should never require a complete source recording to be buffered before fingerprinting begins.

---

# 5. Core Recognition Architecture

Sivana should use a multi-engine design.

| Engine | Purpose | Main Strength | Weakness | Role |
|---|---|---|---|---|
| Landmark Pair Engine | Exact recording recognition | Extremely fast, sparse, robust to noise | Sensitive to larger speed/pitch changes | Primary |
| Scale-Invariant Engine | Modified playback recognition | Survives pitch/time scaling | More computation and storage | Secondary |
| Neural Fingerprint Engine | Difficult pathological cases | Strong degradation robustness | ANN/model/runtime complexity | Future fallback |
| Chromaprint Baseline | Full-file/duplicate comparison | Mature and compact | Poor fit for short noisy queries | Benchmark |
| Philips-Style Baseline | Historical comparison | Simple | Dense representations | Benchmark |

The default recognition path should remain deterministic and cheap.

---

# 6. Engine A: Sivana Landmark

This is the main recognition engine.

It should be optimized for common real-world use:

- song playing from speakers
- laptop audio
- YouTube
- Spotify
- films
- games
- restaurants
- cafés
- live microphone input
- browser tab capture
- compressed streaming audio

Primary design goals:

- very sparse fingerprints
- fast exact lookup
- low memory
- low bandwidth
- short query support
- high noise robustness
- predictable CPU cost

---

# 7. Audio Input Normalization

Benchmark multiple target sample rates:

```text
8,000 Hz
11,025 Hz
16,000 Hz
22,050 Hz
```

A likely target is around 16 kHz, but this must be benchmarked.

The fingerprinting task does not require full high-fidelity audio.

---

## 7.1 Input Pipeline

Recommended pipeline:

```text
capture / decode
        ↓
channel mix
        ↓
DC removal
        ↓
high-pass filtering
        ↓
streaming resampling
        ↓
robust level normalization
        ↓
PCM ring buffer
```

Optional experiments:

- pre-emphasis
- soft limiter
- spectral whitening
- band-pass filtering
- automatic gain normalization
- dynamic range normalization

Every stage must survive benchmarking to remain.

---

# 8. FFT Pipeline

Use optimized real-input FFT processing.

Candidate stack:

- `rustfft`
- `realfft`

Target hot loop:

```text
PCM window
    ↓
Hann window multiply
    ↓
real FFT
    ↓
power spectrum
    ↓
log/dB transform
    ↓
peak detector
```

Rules:

- FFT plans created once
- window coefficients precomputed
- buffers preallocated
- no frame-loop heap allocations
- SIMD enabled where possible
- WASM SIMD supported
- native and WASM behavior benchmarked together

---

# 9. Peak Detection V2

---

## 9.1 Separable Sliding Max

Instead of brute-force 2D scans:

1. perform a sliding maximum along frequency
2. perform a sliding maximum along time

Use monotonic queues/deques where appropriate.

Desired complexity:

```text
O(T × F)
```

---

## 9.2 Adaptive Noise Floor

Define something conceptually like:

```text
normalized_spectrum(t,f)
=
spectrum(t,f)
-
local_noise_floor(t,f)
```

Potential local noise estimates:

- moving median
- percentile
- exponentially weighted baseline
- band-specific percentile
- robust trimmed mean

A peak is accepted only if its prominence exceeds a configurable threshold.

---

## 9.3 Peak Density Control

Instead of "everything above a threshold," enforce a controlled fingerprint density.

Initial research range:

```text
20–60 selected peaks / second
```

Exact values are chosen empirically.

---

## 9.4 Frequency-Band Quotas

Split the useful frequency range into perceptual/log-spaced bands.

Each band receives a peak budget.

This prevents:

- bass-heavy tracks dominating
- cymbals dominating
- noisy high-frequency energy producing excessive hashes

---

## 9.5 Peak Refinement

Experiment with local quadratic interpolation around FFT maxima.

Possible benefits:

- improved frequency stability
- improved cross-device reproducibility
- less bin-boundary sensitivity

Keep only if benchmarks show value.

---

# 10. Landmark Generation V2

For each anchor peak, create a future target region:

```text
             target zone
        ┌───────────────────┐
        │ x       x         │
anchor x│    x         x    │
        │       x           │
        └───────────────────┘
```

Classic hash structure:

```text
H = Q(f_anchor, f_target, Δt)
```

Store:

```text
hash
anchor_time
```

Absolute time should not be part of the hash.

---

# 11. Smarter Target Selection

Target candidates should be ranked.

Possible score:

```text
target_score =
    w1 × strength
  + w2 × frequency_separation
  + w3 × temporal_spacing
  + w4 × local_stability
  + w5 × rarity_prior
```

Select targets spread across the target zone rather than simply taking the first valid points.

---

# 12. Fanout Research

Benchmark:

```text
5
8
10
12
15
```

targets per anchor.

Evaluate:

- recall
- false positives
- storage
- lookup cost
- degradation robustness

The final fanout should maximize recognition quality per byte and per CPU cycle.

---

# 13. Hash Representation

A compact hash can encode quantized:

```text
f1
f2
Δt
```

Research candidate widths:

```text
24 bit
32 bit
40 bit
64 bit
```

Trade-offs:

- collision probability
- posting-list length
- index size
- alignment
- CPU decode cost
- cache locality

A 32-bit representation is particularly attractive because it enables a very fast two-level index.

---

# 14. Rare Hash Weighting

Maintain:

```text
df(h)
```

where `df(h)` = number of recordings containing hash `h`.

Weight:

```text
w(h) = log((N + 1) / (df(h) + 1))
```

Recognition score:

```text
Score(recording, offset)
=
Σ aligned_hash_weight
```

This gives rare hashes more authority than common hashes.

---

# 15. Stop Hashes

Some fingerprints will occur across a massive part of the catalog.

Examples might arise from:

- silence boundaries
- broadband impulses
- common tonal patterns
- codec artifacts
- repetitive drum structures

Hashes above a document-frequency threshold become stop hashes.

Possible handling:

```text
ignore
```

or:

```text
cap postings scanned
```

or:

```text
assign almost-zero weight
```

Benefits:

- lower query time
- fewer false positives
- less useless memory traffic

---

# 16. Production Index Strategy

SQLite should remain useful for:

- development
- metadata
- debugging
- small local installations

It should not remain the billion-fingerprint hot index.

---

# 17. Stage 1 Index: LMDB

Use LMDB through a Rust wrapper such as:

```text
heed
```

Advantages:

- memory mapped
- read-heavy friendly
- mature
- predictable
- easy deployment

This provides a production-capable intermediate step before writing a custom index.

---

# 18. Stage 2 Index: Custom Memory-Mapped Format

Design an immutable binary format:

```text
index.siv
│
├── header
│   ├── magic
│   ├── index_format_version
│   ├── fingerprint_version
│   ├── recording_count
│   ├── hash_count
│   ├── posting_count
│   └── checksum
│
├── hash directory
│
├── hash entries
│   ├── hash suffix
│   ├── postings_offset
│   ├── postings_count
│   └── document_frequency
│
└── postings
    ├── recording_id
    └── anchor_time
```

Open with:

```text
memmap2
```

The operating system page cache then becomes the effective index cache.

---

# 19. High-16 Bucket Directory

For 32-bit hashes:

```text
high 16 bits → direct bucket
low 16 bits  → search within bucket
```

Number of buckets:

```text
2^16 = 65,536
```

A directory of 65,537 `u64` offsets is roughly:

```text
512 KB
```

Lookup:

```text
hash
 ↓
extract high16
 ↓
read bucket range
 ↓
binary search low16
 ↓
read contiguous postings
```

This offers excellent cache locality.

---

# 20. Posting Layout

Candidate packed posting:

```text
u64
```

Possible layout:

```text
32 bits → recording_id
24 bits → anchor_time
8 bits  → flags / reserved
```

Benchmark against:

- plain structs
- packed `u64`
- delta encoding
- varints
- frame-time compression

Prefer alignment and decoding speed unless compression gives a clear system-level win.

---

# 21. Immutable Index Segments

Use segment files:

```text
catalog-0001.siv
catalog-0002.siv
catalog-0003.siv
delta-0042.siv
```

Ingestion writes new segments.

Background compaction:

```text
many small segments
       ↓
merge
       ↓
larger immutable segment
```

Query servers load a manifest:

```text
manifest.json / manifest.bin
```

and atomically swap to a new version.

Advantages:

- no giant mutable global index
- no write locking
- easy rollback
- easy deployment
- easy replication
- safe catalog updates

---

# 22. Matching Pipeline V2

Recommended pipeline:

```text
query fingerprints
        ↓
sort
        ↓
deduplicate
        ↓
remove stop hashes
        ↓
batch index lookup
        ↓
posting scan
        ↓
weighted votes
        ↓
candidate shortlist
        ↓
geometric/time verification
        ↓
confidence model
```

---

# 23. Compact Vote Representation

Create tuples such as:

```text
(recording_id, offset_bucket, weight)
```

Possible algorithms:

### Option A
Append compact vote tuples and radix-sort.

### Option B
Use a custom flat open-addressing table.

### Option C
Use segmented per-candidate accumulators after an early track shortlist.

Benchmark all three.

Avoid nested standard-library hash maps in the final hot path unless they unexpectedly win.

---

# 24. Matching as Geometry

A correct match approximately obeys:

```text
t_database = t_query + b
```

where `b` is the recording offset.

For modified playback:

```text
t_database = a × t_query + b
```

where `a` captures time scaling.

After generating a candidate shortlist, perform geometric verification.

Candidate signals:

- inlier count
- weighted inlier count
- inlier percentage
- covered query duration
- covered song duration
- residual error
- slope consistency
- offset consistency
- unique anchors
- best/second-best margin

---

# 25. Early-Exit Streaming Recognition

Sivana should not require a fixed 10-second capture.

Recognition should update incrementally.

Example:

```text
0.0 s → listening
0.5 s → insufficient evidence
1.0 s → candidate A
1.4 s → candidate A strengthened
1.8 s → confidence threshold reached
1.8 s → return match
```

Possible state machine:

```text
LISTENING
NEED_MORE_AUDIO
CANDIDATE
CONFIDENT_MATCH
NO_MATCH
```

The client can send new fingerprint batches every:

```text
200–300 ms
```

Final values should be benchmarked.

---

# 26. Confidence Calibration

Do not return a "confidence percentage" invented from arbitrary scores.

Features may include:

```text
weighted_match_score
unique_matching_hashes
query_coverage
offset_concentration
matched_time_span
best_candidate_score
second_best_candidate_score
best_second_ratio
time_fit_residual
hash_rarity
signal_quality
engine_agreement
```

Calibrate with:

- logistic regression
- isotonic regression
- Platt-style scaling
- empirical probability tables

Output should correspond to measured reliability.

---

# 27. Engine B: Scale-Invariant Recognition

Classic `(f1, f2, Δt)` hashes break when:

- audio speed changes
- pitch changes
- time stretch occurs

Sivana should therefore implement a second fingerprint engine.

---

# 28. Panako-Inspired Event Invariants

For event times:

```text
t1
t2
t3
```

construct a scale-invariant temporal ratio:

```text
Rt = (t2 - t1) / (t3 - t1)
```

If all times are scaled by `s`:

```text
Rt'
=
s(t2 - t1) / s(t3 - t1)
=
Rt
```

Similar invariant relationships can be built for frequency structures.

This family of methods can tolerate:

- speed changes
- pitch shifts
- time stretching
- codec degradation
- filtering

Implementation should be independent, based on published research rather than copied source code.

---

# 29. Quad Fingerprints

Research a second invariant scheme based on geometric quads of spectral events.

Advantages may include:

- estimating time scaling
- estimating frequency scaling
- strong robustness
- sparse hashes
- geometric verification

Implement both:

```text
B1 → event-triplet invariants
B2 → geometric quad invariants
```

Benchmark them on the same transformation matrix.

Do not choose based on elegance.

Choose based on measured:

- recall
- index size
- latency
- false positives
- throughput

---

# 30. Engine C: Neural Fingerprints

Do not make this the default engine initially.

Potential architecture later:

```text
Engine A
   ↓ low confidence
Engine B
   ↓ still uncertain
Engine C neural fallback
```

Research directions:

- contrastive learned audio fingerprints
- sparse spectrogram peak inputs
- PeakNet-style architectures
- compact embeddings
- HNSW
- IVF/PQ
- ScaNN-like ANN structures
- product quantization
- ONNX
- Candle
- Burn

The neural engine should solve a measured failure mode.

It should not exist merely because "AI" improves slide decks.

---

# 31. Shared Rust Core

Create a Rust workspace.

Recommended structure:

```text
Sivana/
│
├── Cargo.toml
├── rust-toolchain.toml
│
├── crates/
│   ├── sivana-core/
│   │   ├── fingerprint types
│   │   ├── version types
│   │   ├── timing units
│   │   └── shared configuration
│   │
│   ├── sivana-dsp/
│   │   ├── streaming resampler
│   │   ├── ring buffer
│   │   ├── STFT
│   │   ├── windows
│   │   └── peak extraction
│   │
│   ├── sivana-landmark/
│   │   ├── target-zone selection
│   │   ├── pair hashing
│   │   └── fingerprint stream
│   │
│   ├── sivana-invariant/
│   │   ├── triplets
│   │   ├── quads
│   │   └── transformation estimates
│   │
│   ├── sivana-index/
│   │   ├── LMDB backend
│   │   ├── mmap backend
│   │   ├── segment format
│   │   └── index builder
│   │
│   ├── sivana-match/
│   │   ├── lookup
│   │   ├── voting
│   │   ├── shortlist
│   │   ├── geometric verification
│   │   └── confidence
│   │
│   ├── sivana-audio/
│   │   └── native decode
│   │
│   ├── sivana-wasm/
│   │   └── browser bindings
│   │
│   ├── sivana-api/
│   │   └── Axum service
│   │
│   ├── sivana-ingest/
│   │   └── catalog ingestion
│   │
│   └── sivana-bench/
│       ├── degradations
│       ├── fixtures
│       ├── benchmark runner
│       └── reports
│
├── apps/
│   ├── web/
│   └── extension/
│
├── research/
│   ├── PAPERS.md
│   ├── ALGORITHMS.md
│   ├── EXPERIMENTS.md
│   └── BENCHMARKS.md
│
├── index-format/
│   └── SPEC.md
│
└── legacy/
    └── old Sivana implementation
```

---

# 32. Native Audio Stack

Modernize behind benchmarks.

Candidate stack:

```text
Symphonia → decode
Rubato    → streaming resampling
RustFFT   → FFT
RealFFT   → real-input FFT wrapper
```

Rules:

- stream chunks
- no unnecessary copies
- avoid converting formats repeatedly
- avoid allocating per packet
- support deterministic test fixtures

---

# 33. Browser / WASM Architecture

The browser should fingerprint audio locally.

Recommended:

```text
browser microphone
       ↓
Web Audio API
       ↓
AudioWorklet
       ↓
PCM ring buffer
       ↓
sivana.wasm
       ↓
fingerprint stream
       ↓
WebSocket
       ↓
Rust matcher
```

Raw microphone audio should not need to leave the client.

---

# 34. Why Local Fingerprinting Matters

Benefits:

- lower upload bandwidth
- lower server CPU
- lower latency
- better privacy
- easier scale-out
- easier extension reuse
- simpler edge deployments
- less raw audio handling liability

The server receives tiny fingerprints rather than encoded audio.

---

# 35. Native and WASM Shared Code

The same algorithms should compile to:

```text
Linux native
Windows native
macOS native
ARM64
wasm32
```

All deterministic algorithmic logic belongs in shared crates.

Browser-specific capture code stays outside the fingerprinting engine.

Native file decoding stays outside the fingerprinting engine.

This prevents separate implementations from drifting.

---

# 36. Cross-Platform Determinism

Fingerprints may be generated on:

```text
Linux x86_64
Windows x86_64
macOS ARM64
Android
WASM
```

Floating-point and SIMD differences can affect borderline peaks.

Every fingerprint stream should carry:

```text
engine_version
dsp_version
sample_rate
fft_config
quantization_version
```

CI should compare fixed audio fixtures across platforms.

Success criteria should include:

- high fingerprint overlap
- identical recognition result
- stable confidence
- no systematic platform bias

Bit-identical intermediate floating point is not necessarily required.

Behavioral determinism is.

---

# 37. Website Architecture

Initial website:

```text
user
 ↓
browser
 ↓
microphone permission
 ↓
AudioWorklet
 ↓
Rust/WASM fingerprinting
 ↓
WebSocket
 ↓
Rust API
 ↓
fingerprint index
 ↓
match metadata
 ↓
editorial result view
```

Suggested backend framework:

```text
Axum
Tokio
Tower
```

Suggested transport:

```text
WebSocket for streaming fingerprint batches
HTTP/JSON for metadata and static API operations
```

Binary WebSocket messages should be considered for fingerprint batches.

---

# 38. API Sketch

Possible public API:

```text
POST /v1/sessions
WS   /v1/identify/{session_id}
GET  /v1/recordings/{recording_id}
GET  /v1/health
```

Possible client message:

```text
FingerprintBatch
├── engine_version
├── sequence
├── capture_timestamp
└── fingerprints[]
```

Possible server events:

```text
Listening
NeedMoreAudio
Candidate
Matched
NoMatch
Error
```

---

# 39. Security and Abuse Resistance

Fingerprint clients are untrusted.

Validate:

- fingerprint count
- fingerprint density
- timestamps
- engine version
- sequence order
- packet size
- posting budget
- query duration
- session lifetime

Protect against:

- crafted high-frequency hashes
- posting-list amplification
- replay floods
- malformed binary packets
- oversized batches
- CPU exhaustion
- memory exhaustion

Use:

```text
rate limits
timeouts
maximum posting budget
stop hashes
session caps
request body caps
authentication for ingestion
```

---

# 40. Catalog Data Model

Separate conceptual songs from fingerprintable recordings.

Recommended:

```text
Recording
├── recording_id
├── fingerprint version
├── duration
├── source hash
└── canonical fingerprint identity

TrackMetadata
├── title
├── artist
├── album
├── release
├── artwork
├── ISRC
├── external IDs
└── metadata source
```

Multiple metadata rows may point to one actual recording.

---

# 41. Catalog Ingestion

Build an idempotent ingestion pipeline.

Pipeline:

```text
source
  ↓
source hash
  ↓
decode
  ↓
normalize
  ↓
fingerprint
  ↓
recording duplicate detection
  ↓
metadata association
  ↓
segment builder
  ↓
manifest update
```

Requirements:

- resumable
- parallel
- idempotent
- versioned
- observable
- rollbackable

---

# 42. Catalog Legality

Sivana can only recognize audio represented in its reference catalog.

For development use:

- self-owned audio
- open-licensed music
- public benchmark datasets
- research datasets within license terms

A consumer-scale arbitrary commercial catalog requires legitimate access to reference audio.

Metadata APIs alone do not confer rights to copy and fingerprint every commercial track.

Do not build catalog acquisition around scraping streaming services.

---

# 43. Recognition Benchmarks

Benchmarking is a first-class subsystem.

Not a post-launch chore.

---

# 44. Query Duration Matrix

Test:

```text
0.5 sec
1.0 sec
1.5 sec
2.0 sec
3.0 sec
5.0 sec
8.0 sec
15.0 sec
```

---

# 45. Noise Matrix

Test SNR:

```text
+20 dB
+10 dB
+5 dB
0 dB
-5 dB
-10 dB
```

Noise sources:

- white noise
- pink noise
- crowds
- traffic
- speech
- keyboard
- fans
- other music
- café ambience

---

# 46. Acoustic Environment Matrix

Use impulse responses or real recordings:

```text
bedroom
office
lecture hall
bathroom
car
large hall
laptop speaker → laptop mic
phone speaker → phone mic
TV → phone mic
```

---

# 47. Codec Matrix

Test:

```text
WAV
FLAC
MP3 320
MP3 192
MP3 128
MP3 64
AAC
Opus
low-bitrate Opus
multiple encode/decode generations
```

---

# 48. DSP Abuse Matrix

Test:

```text
EQ
low-pass
high-pass
band-pass
compression
limiting
clipping
reverb
AGC
sample-rate conversion
stereo → mono
phase changes
```

---

# 49. Speed and Pitch Matrix

Test:

```text
0.80×
0.90×
0.95×
1.05×
1.10×
1.20×
```

Pitch:

```text
±1 semitone
±2 semitones
±4 semitones
±6 semitones
```

Also test time stretch independent of pitch.

---

# 50. Mixture Matrix

Test:

```text
music + speech
music + game audio
music + TV dialogue
music + another song
music + crowd noise
quiet background music
multiple simultaneous recognizable songs
```

Sivana should eventually be capable of exposing multiple candidates where evidence supports it.

---

# 51. Real-World Evaluation

Use research benchmark datasets where licensing permits.

Synthetic degradation must not be the only test source.

Real-world broadcast, acoustic and microphone recordings should be included.

---

# 52. Metrics

Never summarize recognition performance with one "accuracy" number.

Track:

## Recognition

```text
Recall@1
precision
false positive rate
false negative rate
out-of-catalog rejection
top-k recall
```

## Latency

```text
time to first candidate
time to confident match
server p50
server p95
server p99
end-to-end p50
end-to-end p99
```

## CPU

```text
fingerprint real-time factor
cycles per query
allocations per query
postings scanned
cache misses
branch misses
```

## Storage

```text
hashes per second of audio
bytes per second of audio
bytes per recording
index bytes per track
posting-list distribution
```

## Robustness

Break results down by:

```text
noise
codec
speed
pitch
environment
query length
device
browser
engine
```

---

# 53. Initial Engineering Targets

These are targets, not current claims.

| Metric | Initial Target |
|---|---:|
| Clean recognition capture | ~1.5–3 s |
| Normal room/mic | ~2–5 s |
| Severe degradation | ≤8 s where feasible |
| Server matching p50 | <50 ms |
| Server matching p99 at large scale | <200 ms |
| Initial false accept rate | <1e-5 |
| Stretch false accept target | <1e-6 |
| Browser DSP | comfortably faster than realtime |
| Query network bandwidth | KB-scale |
| Allocations in index query path | near zero |

A stretch throughput target should eventually be thousands of recognition requests per second per commodity matcher node.

---

# 54. Catalog Scale Math

Fingerprint density must be treated as an economic variable.

Example:

```text
8 hashes / second
× 240 seconds
= 1,920 hashes / track
```

For:

```text
1,000,000 tracks
```

that becomes:

```text
1.92 billion postings
```

Even 8 bytes per posting:

```text
≈ 15.4 GB
```

before dictionaries, metadata and other structures.

Therefore:

```text
more fingerprints ≠ automatically better
```

Tune jointly:

- peak density
- fanout
- hash width
- rarity filtering
- posting layout
- query latency

---

# 55. Benchmark Tooling

Create:

```text
sivana-bench
```

Capabilities:

```text
generate degraded query
run selected engine
measure recognition
measure latency
measure allocations
compare against baseline
emit JSON/CSV
produce summary report
```

Possible benchmark tools:

- Criterion
- custom wall-clock harness
- Linux `perf`
- `cargo flamegraph`
- `heaptrack` / allocator instrumentation
- cachegrind where useful

---

# 56. Tests

## Golden DSP Tests

Fixed waveform → expected:

```text
sample count
frame count
peak locations
fingerprint overlap
```

---

## Property Tests

Examples:

```text
silence prefix shifts anchor times but preserves internal hashes
moderate amplitude scaling preserves most fingerprints
same audio generates stable fingerprints
query excerpt maps to correct reference offset
```

Use:

```text
proptest
```

---

## Fuzzing

Fuzz:

```text
index parser
binary protocol
fingerprint parser
audio input boundaries
segment metadata
manifest parser
```

Use:

```text
cargo-fuzz
```

---

# 57. CI

Recommended gates:

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
WASM tests
cargo audit
cargo deny
fuzz smoke tests
benchmark regression check
index compatibility tests
cross-platform fingerprint fixtures
```

Release targets:

```text
Linux x86_64
Linux ARM64
Windows x86_64
macOS ARM64
wasm32
```

---

# 58. Observability

Every query should internally report:

```text
capture_duration_ms
fingerprint_count
unique_hash_count
lookup_us
postings_scanned
candidate_count
verification_us
confidence
engine
total_server_us
```

Use:

- `tracing`
- OpenTelemetry
- Prometheus-compatible metrics

Dashboard:

```text
p50/p95/p99 latency
match rate
no-match rate
false-match evaluations
fallback engine usage
CPU
memory
index page faults
catalog version
fingerprint version
```

---

# 59. Chrome Extension Architecture

Later:

```text
user clicks extension
       ↓
service worker
       ↓
chrome.tabCapture
       ↓
offscreen document
       ↓
AudioWorklet
       ↓
sivana.wasm
       ↓
same fingerprint protocol
       ↓
same server
       ↓
result popup
```

The extension must not contain a separate recognition engine.

The website and extension should be alternative audio-capture frontends.

---

# 60. Why Website First

The website forces us to solve:

- browser audio capture
- WASM
- streaming fingerprint generation
- protocol design
- production API
- user-facing confidence states
- deployment
- privacy
- cross-browser behavior

After that, the extension is primarily tab capture and UI integration.

---

# 61. UI / Visual Direction

## Editorial Magazine, Not SaaS

The Sivana interface should look like a **premium editorial music publication** rather than:

- a generic AI dashboard
- a developer console
- a Spotify clone
- a glassmorphism startup template
- a neon cyberpunk gimmick

The reference mood should be closer to:

```text
high-end music magazine
fashion editorial
culture journal
museum catalog
independent print publication
```

The UI should feel deliberately composed.

---

# 62. Editorial Design Principles

Use:

- oversized display typography
- aggressive type scale contrast
- serif + grotesk pairing
- asymmetric grids
- strong editorial spacing
- generous whitespace
- hard rules / dividers
- large album art
- expressive pull-quote-style result text
- restrained animation
- high-contrast black/white base
- one carefully controlled accent system
- page numbers / issue-like microcopy where appropriate
- small uppercase metadata labels
- substantial margins
- intentional empty space

Avoid:

- endless rounded cards
- dashboard tile grids
- excessive gradients
- blobs
- glossy 3D icons
- huge pill buttons everywhere
- fake AI sparkles
- random glass panels

---

# 63. Typography Direction

Suggested structure:

```text
Display serif:
    album/result titles
    hero copy
    large editorial statements

Neutral grotesk / sans:
    controls
    metadata
    timing
    system state
    navigation
```

Potential open-source type combinations should be evaluated later, but the personality should be:

```text
editorial serif + modern Swiss grotesk
```

Examples of *style families* to research:

```text
Bodoni / Didot-like display serif
Newsreader-like editorial serif
Instrument Serif-like display personality
Inter / Geist / IBM Plex / Neue Haas-like sans direction
```

Do not lock fonts before checking:

- license
- performance
- variable font support
- browser rendering
- readability

---

# 64. Homepage Concept

The homepage should almost resemble the opening spread of a magazine.

Possible structure:

```text
────────────────────────────────────────────
SIVANA                                  001
A MACHINE FOR FINDING SOUND

               [ enormous editorial headline ]

        Hear it.
        Name it.

                ◉ LISTEN

────────────────────────────────────────────
Recognition without uploading your raw audio.
Built in Rust. Fingerprinted locally.
────────────────────────────────────────────
```

The hero should be visually restrained.

The product should not immediately drown the user in explanation.

---

# 65. Listening Screen

While capturing audio:

```text
SIVANA / LISTENING                                    01

              WHAT ARE WE
              HEARING?

                  ◉

        01.84 seconds captured

──────────────────────────────────────────────────────

Signal          GOOD
Landmarks       146
Status          BUILDING A MATCH

──────────────────────────────────────────────────────
```

This can feel like an editorial title page rather than a loader.

Avoid generic circular progress spinners.

---

# 66. Recognition Result Layout

A successful result should feel like a magazine feature reveal.

Example:

```text
02 / FOUND

[ LARGE ALBUM ART ]

                         THE SONG

                         TRACK TITLE
                         Artist Name

                         Album
                         2026

                         MATCHED AT
                         01:47

                         CONFIDENCE
                         99.997%

──────────────────────────────────────────────────────

Recognized after 1.84 seconds.

Engine A / Landmark
```

Typography and image hierarchy should do most of the work.

---

# 67. Result Page as an Editorial Spread

Desktop layout can use asymmetric columns:

```text
┌───────────────────────┬─────────────────────────────┐
│                       │                             │
│     ALBUM ART         │   TRACK TITLE               │
│                       │   Artist                    │
│                       │                             │
│                       │   editorial metadata        │
│                       │                             │
│                       │   recognition details       │
│                       │                             │
└───────────────────────┴─────────────────────────────┘
```

Mobile should stack gracefully without becoming card soup.

---

# 68. Recognition History

History should resemble an archive/index rather than a dashboard table.

Example:

```text
ARCHIVE                                                   04

22 AUG 2026

01   Song Title                       Artist        16:42
02   Another Song                     Artist        15:10
03   Another Track                    Artist        13:02

21 AUG 2026

04   Track                             Artist        23:54
```

Album artwork can appear on hover/selection on desktop.

---

# 69. Technical Diagnostics UI

Advanced mode should preserve the editorial identity.

Instead of developer-dashboard cards:

```text
SYSTEM NOTES / RECOGNITION 1F7A

CAPTURE                  1.84 s
LANDMARKS                146
UNIQUE HASHES            132
POSTINGS SCANNED         8,214
INDEX LOOKUP             17.6 ms
GEOMETRIC VERIFY         2.1 ms
TOTAL SERVER             24.8 ms
ENGINE                   A
CATALOG                   v18
```

Think publication colophon / technical appendix.

---

# 70. Motion Design

Motion should be:

- subtle
- typographic
- responsive
- functional

Examples:

- characters/lines easing into place
- thin rule expanding
- album art reveal
- slight mask transitions
- waveform/constellation visualization only when meaningful
- state transitions tied to recognition evidence

Avoid:

- perpetual background particle systems
- heavy parallax
- 3D blobs
- slow page transitions
- animations that delay result visibility

Recognition must always feel faster than the animation.

---

# 71. Audio Visualization

If included, do not use a generic music-player waveform by default.

Potential Sivana-specific visualization:

```text
live constellation map
```

where detected peaks appear sparsely over time.

This is algorithmically meaningful and visually distinctive.

Possible visualization modes:

```text
spectral constellation
landmark connections
candidate convergence
```

Keep it restrained enough to fit the editorial design.

---

# 72. Color Direction

Default recommendation:

```text
warm off-white / paper background
near-black typography
one accent color
```

Alternative dark issue:

```text
near-black background
warm white typography
single saturated accent
```

Do not build the identity around a giant gradient.

The recognition result's album artwork can provide most of the color.

---

# 73. Responsive Design

Desktop:

- strong asymmetric grid
- oversized typography
- large artwork
- editorial margins

Tablet:

- two-column collapse
- preserve type hierarchy

Mobile:

- single-column editorial flow
- oversized headings retained
- controls remain reachable
- artwork becomes full-width or near-full-width
- no tiny dense dashboard elements

---

# 74. Accessibility

Editorial does not mean inaccessible.

Requirements:

- WCAG-conscious contrast
- proper semantic heading hierarchy
- keyboard operability
- visible focus states
- reduced-motion mode
- screen-reader labels for listening state
- no critical information conveyed solely by color
- large touch targets
- transcriptable system states

---

# 75. Frontend Technology

Preferred direction:

```text
TypeScript
React or another mature component framework
Vite / modern bundler
Rust/WASM fingerprint module
Web Audio API
AudioWorklet
WebSocket client
```

The visual system should be handcrafted rather than driven by a generic dashboard component kit.

CSS approach should support:

- custom layout
- grid
- variable type
- typography tokens
- responsive editorial scales

---

# 76. Design Tokens

Define:

```text
type scale
spacing scale
page margins
grid columns
line thickness
radii
motion timing
accent color
paper/background
ink/text
muted text
```

Use few radii.

A magazine aesthetic generally benefits from harder edges.

---

# 77. First Public Website Milestone

The first product milestone should be:

```text
┌─────────────────────────────────────┐
│              SIVANA                 │
│                                     │
│           HEAR IT. NAME IT.         │
│                                     │
│              ◉ LISTEN               │
│                                     │
└─────────────────────────────────────┘
                  ↓
          browser microphone
                  ↓
             Rust/WASM DSP
                  ↓
         fingerprints only
                  ↓
             Rust server
                  ↓
           reference catalog
                  ↓
┌─────────────────────────────────────┐
│              FOUND / 02             │
│                                     │
│        [ LARGE ALBUM ART ]          │
│                                     │
│             TRACK TITLE             │
│              ARTIST                 │
│                                     │
│        MATCHED AT 00:47             │
│       RECOGNIZED IN 1.84s           │
└─────────────────────────────────────┘
```

Behind an advanced diagnostics toggle:

```text
fingerprints
query duration
lookup latency
postings scanned
verification latency
confidence
engine
```

---

# 78. Phase 0: Research and Benchmark Platform

**Goal:** stop guessing.

Build:

- new Cargo workspace
- legacy baseline runner
- benchmark CLI
- degradation generator
- audio fixtures
- Criterion benchmarks
- performance report format
- cross-platform determinism tests
- algorithm configuration schema

Exit criteria:

```text
one command can compare legacy Sivana and new engines over thousands of degraded queries
```

---

# 79. Phase 1: Landmark Engine V2

Build:

- streaming DSP
- RealFFT path
- reusable buffers
- adaptive peak extraction
- density control
- frequency quotas
- smarter target zones
- versioned fingerprints
- benchmark parameter sweeps

Exit criteria:

- faster than legacy
- lower memory
- higher degraded-audio recall
- stable cross-platform results

---

# 80. Phase 2: Matcher V2

Build:

- query hash sort/dedup
- rarity weights
- stop hashes
- batch lookups
- compact voting
- candidate shortlist
- geometric verification
- early stopping
- confidence features

Exit criteria:

- calibrated false positive rate
- major query latency reduction
- stable out-of-catalog rejection

---

# 81. Phase 3: Production Index

Build:

### First
```text
LMDB / heed backend
```

### Then
```text
custom mmap backend
```

Implement:

- bucket directory
- compact hash entries
- compact postings
- immutable segments
- manifest loading
- atomic swaps
- compaction tool
- corruption checks
- version checks

Exit criteria:

- large synthetic/reference catalog
- p99 target achieved
- index size measured and documented
- near-zero query allocations

---

# 82. Phase 4: WASM Engine

Build:

- `sivana-wasm`
- browser PCM ingest
- AudioWorklet bridge
- WASM SIMD
- incremental fingerprint emission
- deterministic fixtures

Exit criteria:

- fingerprinting faster than realtime on normal laptops and phones
- native/WASM recognition parity
- no raw audio server requirement

---

# 83. Phase 5: Production Website

Build:

- editorial design system
- listening page
- result spread
- recognition archive
- diagnostics mode
- WebSocket recognition
- graceful no-match
- permissions UX
- privacy copy
- mobile layouts
- accessibility
- observability
- deployment

Exit criteria:

- complete end-to-end recognition
- production deployment
- polished editorial UX
- measurable latency shown internally

---

# 84. Phase 6: Catalog Platform

Build:

- parallel ingestion
- source SHA-256 IDs
- recording deduplication
- metadata association
- index segment generation
- catalog versioning
- rollback
- incremental updates
- compaction

Exit criteria:

- large catalog can be updated without stopping query servers

---

# 85. Phase 7: Scale-Invariant Engine

Implement:

```text
B1 → event triplets
B2 → geometric quads
```

Benchmark:

- pitch shift
- speed changes
- time stretch
- noisy transformations
- codecs
- real microphone recordings

Promote the better system to production.

---

# 86. Phase 8: Neural R&D

Only begin after deterministic engine failure modes are quantified.

Research:

- compact learned fingerprints
- sparse peak inputs
- ANN shortlist
- ONNX/Candle/Burn
- neural verification
- hybrid indexes

Exit criteria:

- clear measurable improvement on a defined failure class
- acceptable CPU/memory/storage cost

---

# 87. Phase 9: Chrome Extension

Build:

```text
Manifest V3
tabCapture
offscreen audio document
AudioWorklet
sivana.wasm
existing recognition protocol
editorial popup
```

No fingerprint-engine rewrite.

---

# 88. Phase 10: Scale

Once recognition quality justifies it:

```text
read-only replicated matcher nodes
object-storage index snapshots
regional deployments
CDN segment distribution
catalog prewarming
health-aware routing
horizontal API scaling
```

Matcher node:

```text
binary
+
mmap index
+
metadata cache
```

should remain mostly stateless.

---

# 89. What "Better Than Shazam" Should Mean

Do not claim superiority based on implementation language.

Rust alone does not make an algorithm better.

Sivana can realistically aim to excel in specific measurable areas:

## Modified Audio

Recognize:

```text
sped-up
slowed-down
pitch-shifted
time-stretched
```

with Engine B.

## Privacy

Fingerprint locally in browser.

## Latency

Stream until confidence is sufficient instead of waiting for a fixed capture length.

## Transparency

Expose:

```text
confidence
offset
engine
recognition time
```

## Open Evaluation

Publish benchmark methodology.

## Private Catalogs

Allow user-owned or organization-owned reference libraries.

## Direct Tab Audio

The extension can capture tab audio directly, eliminating room/microphone degradation.

---

# 90. Research Sources / Starting Literature

The implementation research should continue from these families of work.

## Classic Shazam / Landmark Fingerprinting

Avery Wang:

**An Industrial-Strength Audio Search Algorithm**

Key concepts:

- constellation maps
- anchor points
- target zones
- sparse landmark hashes
- time-offset histograms
- large-scale exact lookup

Reference:

https://www.ee.columbia.edu/~dpwe/papers/Wang03-shazam.pdf

---

## Panako

Research area:

- time-scale robustness
- pitch-scale robustness
- sparse event-triplet fingerprints
- transformation invariance

Paper:

https://0110.be/files/publications/2014/ismir_2014_panako_fingerprinter.pdf

Repository:

https://github.com/JorenSix/Panako

Use research concepts.

Do not copy implementation code into Sivana.

---

## Scale-Invariant Quad Fingerprints

Research on geometric event structures robust to time/frequency scaling.

Reference family:

https://www.cp.jku.at/research/papers/Sonnleitner_etal_DAFx_2014.pdf

---

## Neural Audio Fingerprinting

Research:

- contrastive learned fingerprints
- ANN search
- compact learned representations
- robust degraded-query matching

Relevant areas to continue researching:

```text
Neural Audio Fingerprint
PeakNetFP
contrastive audio retrieval
learned audio fingerprinting
```

---

## Chromaprint

Useful baseline for duplicate / near-identical audio rather than primary Sivana short-query recognition.

Repository:

https://github.com/acoustid/chromaprint

---

# 91. Immediate Implementation Order

The next engineering steps should be:

```text
1. Preserve legacy Sivana
2. Create Rust workspace
3. Build benchmark/degradation harness
4. Establish baseline metrics
5. Implement streaming DSP
6. Replace peak detector
7. Implement Landmark V2
8. Compare fingerprint quality against legacy
9. Replace SQLite query path with batched index
10. Add rarity weighting
11. Add geometric verification
12. Calibrate confidence
13. Build production index
14. Compile core to WASM
15. Build editorial website
16. Add real catalog ingestion
17. Build Engine B
18. Build extension
```

---

# 92. Non-Negotiable Engineering Rules

- Benchmark before optimizing.
- Preserve a control implementation.
- No major algorithm parameter should exist without a benchmark justification.
- No raw audio should be uploaded by default if client-side fingerprints are sufficient.
- Never tie the engine directly to one frontend.
- Keep native and WASM fingerprint code shared.
- Version fingerprint formats.
- Version index formats.
- Keep ingestion and serving separated.
- Prefer immutable read-heavy serving structures.
- Avoid allocations in recognition hot loops.
- Avoid lock-heavy global state.
- Treat false positives as seriously as false negatives.
- Test out-of-catalog audio explicitly.
- Test real acoustic recordings, not only synthetic transformations.
- Do not let visual design delay a recognition result.
- Do not let "Rust = fast" substitute for profiling.

---

# 93. Final Architecture

```text
                         SIVANA

                 ┌────────────────────┐
                 │   AUDIO SOURCES    │
                 │                    │
                 │ microphone         │
                 │ browser tab        │
                 │ native files       │
                 └─────────┬──────────┘
                           │
                    streaming PCM
                           │
                 ┌─────────▼──────────┐
                 │    SIVANA DSP      │
                 │                    │
                 │ resample           │
                 │ FFT                │
                 │ normalize          │
                 │ peak detection     │
                 └─────────┬──────────┘
                           │
                     spectral events
                           │
          ┌────────────────┴────────────────┐
          │                                 │
┌─────────▼──────────┐            ┌─────────▼──────────┐
│ LANDMARK ENGINE A  │            │ INVARIANT ENGINE B │
│                    │            │                    │
│ pair fingerprints  │            │ triplets / quads   │
│ fastest path       │            │ scale robust       │
└─────────┬──────────┘            └─────────┬──────────┘
          │                                 │
          └────────────────┬────────────────┘
                           │
                    fingerprint stream
                           │
                 ┌─────────▼──────────┐
                 │  SIVANA MATCHER    │
                 │                    │
                 │ mmap index         │
                 │ rarity weighting   │
                 │ offset voting      │
                 │ geometric verify   │
                 │ confidence         │
                 └─────────┬──────────┘
                           │
               ┌───────────┴────────────┐
               │                        │
         CONFIDENT MATCH          NEED MORE AUDIO
               │                        │
               │                        └── continue stream
               │
        metadata + offset
               │
      ┌────────▼────────┐
      │ EDITORIAL UI    │
      │                 │
      │ web             │
      │ extension       │
      └─────────────────┘
```

---

# 94. Final Product Thesis

Sivana should not become:

> "a Shazam clone with a Rust backend."

It should become:

> **a high-performance, research-driven audio recognition platform built around sparse deterministic fingerprints, a custom memory-mapped search index, streaming evidence accumulation, scale-invariant fallback recognition and a shared Rust/WASM core.**

The website should present that system with a strong editorial identity:

> **premium music magazine outside, ruthless search engine inside.**

The eventual Chrome extension should be only another input surface for the same underlying engine.

That architecture lets Sivana grow from:

```text
old Rust CLI experiment
```

into:

```text
research platform
      ↓
production recognition engine
      ↓
editorial web product
      ↓
Chrome extension
      ↓
large-scale audio search system
```

without throwing away the core implementation each time.

---

# 95. Definition of Success

Sivana is successful when all of the following are true:

- recognition is benchmarked, not anecdotal
- clean songs can often be identified after only a few seconds or less
- noisy microphone capture works reliably
- out-of-catalog false positives are extremely rare
- browser fingerprinting runs comfortably faster than realtime
- raw microphone audio does not need to leave the browser
- one production index can serve a large catalog efficiently
- the same DSP engine powers native, web and extension clients
- the scale-invariant engine materially improves altered-audio recognition
- every performance claim has a reproducible benchmark
- the website feels like a premium editorial music product
- the extension requires no recognition rewrite
- Rust is used where it produces measurable engineering benefit rather than as branding

That is the project.
