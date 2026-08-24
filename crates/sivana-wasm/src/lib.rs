//! Sivana WASM engine (PLAN §33-§35, Phase 4).
//!
//! One fingerprinting core for native and browser (§35): this crate wraps
//! the streaming landmark pipeline behind a chunked push API whose output
//! is a compact binary batch — the wire format the website and extension
//! stream to the matcher. Raw audio never needs to leave the client.
//!
//! Wire format per batch (little-endian):
//! ```text
//! magic "SFP1" u32 | engine_version u32 | sample_rate_hz u32
//! count u32 | count × { hash u32, anchor_time u32 }
//! ```
//!
//! Determinism (§36): all DSP is plain f32/f64 arithmetic without
//! fast-math or reassociation-dependent patterns, so frames are identical
//! across platforms; CI pins a golden-fixture digest natively on every OS
//! matrix entry, and the wasm build is compile-checked in CI. Behavioral
//! parity on-device is asserted by the website's self-check.

use sivana_core::{EngineId, FingerprintVersion};
use sivana_landmark::LandmarkV2Config;

pub const BATCH_MAGIC: &[u8; 4] = b"SFP1";

/// Fingerprint version this engine emits.
pub fn engine_fingerprint_version() -> FingerprintVersion {
    sivana_core::current_fingerprint_version(EngineId::LandmarkV2)
}

/// Serialize finished fingerprints into the SFP1 batch format.
pub fn encode_batch(
    fps: &[sivana_landmark::Fingerprint32],
    sample_rate_hz: u32,
    out: &mut Vec<u8>,
) {
    out.clear();
    out.extend_from_slice(BATCH_MAGIC);
    let ver = engine_fingerprint_version();
    out.extend_from_slice(&u32::from(ver.major).to_le_bytes());
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&(fps.len() as u32).to_le_bytes());
    for fp in fps {
        out.extend_from_slice(&fp.hash.to_le_bytes());
        out.extend_from_slice(&fp.anchor_time.to_le_bytes());
    }
}

/// Incremental streaming fingerprinter shared by every frontend.
///
/// Feed mono PCM at `sample_rate_hz`; pull finished batches after each
/// chunk ([`Self::take_batch`]) and once more at end of stream
/// ([`Self::finish`]). Memory stays constant regardless of duration —
/// suitable for an AudioWorklet driving it with 128-sample render quanta.
pub struct FingerprintEngine {
    inner: sivana_landmark::LandmarkStreamer,
    sample_rate_hz: u32,
    cfg: LandmarkV2Config,
    scratch: Vec<sivana_landmark::Fingerprint32>,
}

impl FingerprintEngine {
    pub fn new(sample_rate_hz: u32, cfg: LandmarkV2Config) -> Self {
        Self {
            inner: sivana_landmark::LandmarkStreamer::new(&cfg),
            sample_rate_hz,
            cfg,
            scratch: Vec::new(),
        }
    }

    pub fn config(&self) -> &LandmarkV2Config {
        &self.cfg
    }

    /// Feed PCM; call [`Self::take_batch`] afterwards to drain anything
    /// finalized by this chunk (or accumulate and drain rarely — results
    /// are identical either way).
    pub fn process(&mut self, samples: &[f32]) {
        self.inner.process(samples);
    }

    /// Drain fingerprints finalized so far into `out` as an SFP1 batch.
    pub fn take_batch(&mut self, out: &mut Vec<u8>) {
        self.inner.drain_into(&mut self.scratch);
        encode_batch(&self.scratch, self.sample_rate_hz, out);
        self.scratch.clear();
    }

    /// Flush end-of-stream state; drain one final batch after calling.
    pub fn finish(&mut self) {
        self.inner.finish();
    }
}

// ---- wasm bindings (browser side) ----

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    pub struct WasmFingerprinter {
        inner: FingerprintEngine,
        batch: Vec<u8>,
    }

    #[wasm_bindgen]
    impl WasmFingerprinter {
        /// Create an engine for mono PCM at `sample_rate_hz` using the
        /// production operating point (E4: 512 log bands — MUST match the
        /// ingest configuration or hashes cannot collide).
        #[wasm_bindgen(constructor)]
        pub fn new(sample_rate_hz: u32) -> WasmFingerprinter {
            Self {
                inner: FingerprintEngine::new(
                    sample_rate_hz,
                    LandmarkV2Config {
                        freq_bands: sivana_core::OPERATING_FREQ_BANDS,
                        ..Default::default()
                    },
                ),
                batch: Vec::new(),
            }
        }

        /// Push mono PCM; returns the SFP1 batch of fingerprints finalized
        /// by this chunk (may be empty).
        pub fn process(&mut self, pcm: &[f32]) -> Vec<u8> {
            self.inner.process(pcm);
            self.inner.take_batch(&mut self.batch);
            std::mem::take(&mut self.batch)
        }

        /// Flush end-of-stream state; returns any trailing batch.
        pub fn finish(&mut self) -> Vec<u8> {
            self.inner.finish();
            self.inner.take_batch(&mut self.batch);
            std::mem::take(&mut self.batch)
        }

        /// Human-readable engine identity for diagnostics panels.
        pub fn version(&self) -> String {
            let v = engine_fingerprint_version();
            format!("landmark-v2/{}-bit fp v{}.{}", 32, v.major, v.minor)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sivana_audio::fixtures;
    use sivana_landmark::fingerprint;

    #[test]
    fn batch_format_roundtrips_and_is_stable() {
        let sig = fixtures::synth_song(7, 3.0, 22_050);
        let mut e = FingerprintEngine::new(22_050, LandmarkV2Config::default());
        // Odd chunk sizes exercise incremental emission.
        for piece in sig.chunks(997) {
            e.process(piece);
        }
        e.finish();
        let mut batch = Vec::new();
        e.take_batch(&mut batch);

        assert_eq!(&batch[..4], BATCH_MAGIC);
        let major = u32::from_le_bytes(batch[4..8].try_into().unwrap());
        let sr = u32::from_le_bytes(batch[8..12].try_into().unwrap());
        let count = u32::from_le_bytes(batch[12..16].try_into().unwrap()) as usize;
        assert_eq!(sr, 22_050);
        assert_eq!(major, 1);
        assert_eq!(batch.len(), 16 + count * 8);

        // Same audio through the batch API must equal the plain function.
        let direct = fingerprint(&sig, 22_050, &LandmarkV2Config::default());
        assert_eq!(count, direct.len());
        for (i, fp) in direct.iter().enumerate() {
            let h = u32::from_le_bytes(batch[16 + i * 8..20 + i * 8].try_into().unwrap());
            let t = u32::from_le_bytes(batch[20 + i * 8..24 + i * 8].try_into().unwrap());
            assert_eq!((h, t), (fp.hash, fp.anchor_time));
        }
    }

    #[test]
    fn golden_fixture_digest_is_pinned() {
        // Cross-platform determinism anchor (§36): the digest must not move
        // unless the fingerprint format deliberately changed. CI runs this
        // on every OS in the matrix; the wasm build shares the same code.
        let sig = fixtures::synth_song(2026, 5.0, 22_050);
        let fps = fingerprint(&sig, 22_050, &LandmarkV2Config::default());
        let mut h = 0x811C_9DC5u32;
        for fp in &fps {
            for b in fp
                .hash
                .to_le_bytes()
                .iter()
                .chain(fp.anchor_time.to_le_bytes().iter())
            {
                h ^= *b as u32;
                h = h.wrapping_mul(0x0100_0193);
            }
        }
        assert!(
            fps.len() > 100,
            "fixture should produce a real fingerprint stream"
        );
        // Digest observed on this workspace build; bump ONLY with a
        // fingerprint-version bump and an EXPERIMENTS.md entry.
        assert_eq!(h, 0xC4A5_91E4, "golden digest drifted");
    }

    #[test]
    fn realtime_factor_comfortable_on_native() {
        // §82 exit criterion proxy: fingerprinting must run far faster than
        // realtime. On the dev box this measures ~500-1000x; assert a
        // conservative floor so slow machines still pass but regressions
        // scream.
        let sig = fixtures::synth_song(11, 10.0, 22_050);
        let t0 = std::time::Instant::now();
        let _ = fingerprint(&sig, 22_050, &LandmarkV2Config::default());
        let elapsed = t0.elapsed();
        let factor = 10.0 / elapsed.as_secs_f64();
        assert!(
            factor > 50.0,
            "realtime factor {factor:.1}x below 50x floor"
        );
    }
}
