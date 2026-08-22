# Research Papers

Starting literature for Sivana's algorithm work (PLAN.md §90).

## Landmark / constellation fingerprinting

- **Avery Wang — "An Industrial-Strength Audio Search Algorithm" (2003)** —
  https://www.ee.columbia.edu/~dpwe/papers/Wang03-shazam.pdf
  Constellation maps, anchor/target zones, sparse `(f1, f2, dt)` hashes,
  time-offset histogram voting. This is the family the legacy prototype and
  Engine A implement.
- Dan Ellis lecture notes on robust audio identification (Columbia E6820)
  for practical parameter ranges (peak density, target zone shapes).

## Scale-invariant fingerprinting (Engine B)

- **Panako (Six & Six, ISMIR 2014)** —
  paper: https://0110.be/files/publications/2014/ismir_2014_panako_fingerprinter.pdf
  repo: https://github.com/JorenSix/TarsosDSP (Panako module).
  Event-triplet invariants `Rt = (t2-t1)/(t3-t1)` survive time/pitch scaling.
  Use the *concepts*; do not copy implementation code (PLAN.md §28).
- **Sonnleitner & Widmer — quad-based fingerprints (DAFx 2014)** —
  https://www.cp.jku.at/research/papers/Sonnleitner_etal_DAFx_2014.pdf
  Geometric quads robust to time- and frequency-scaling; candidate for B2.

## Learned fingerprints (Engine C, later)

- Contrastive neural audio fingerprints (NFNet-lineage work),
  PeakNetFP-style architectures operating on sparse peak sets,
  ANN retrieval (HNSW, IVF-PQ). Only enter after deterministic engine
  failure modes are quantified (§86).

## Baselines

- Chromaprint/AcoustID — https://github.com/acoustid/chromaprint —
  full-file similarity; benchmark-only role for duplicate detection.
- Philips robust hashing family — dense representations; historical
  comparison point.

## Rules

1. Every algorithmic claim in this repo must trace to a paper entry here or
   an experiment in EXPERIMENTS.md.
2. Implementations are written from the described math, not translated from
   reference repos, unless license explicitly permits it.
