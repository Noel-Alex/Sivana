//! Landmark V2 fingerprinting pipeline — streaming form with constant
//! memory (§4.1, §10, §11).
//!
//! [`LandmarkStreamer`] chains the streaming STFT and the streaming peak
//! detector, then pairs anchors with targets chosen from a bounded window
//! of future peaks (`dt_max` frames). Anchors finalize once their whole
//! target zone has arrived, so memory stays O(peaks within `dt_max`
//! frames) regardless of source duration. The batch [`fingerprint`] is a
//! thin wrapper over one streamer pass, keeping both paths identical by
//! construction.
//!
//! Target scoring (§11, E2a follow-up): `score = df * 0.5 + strength * 64`
//! where `strength` is the target's prominence above its own frame's noise
//! floor, clamped to a 60 dB ceiling. Unlike the earlier global-max
//! normalization, prominence is gain-invariant (uniform level shifts move
//! cell and floor equally in dB) and needs no lookahead beyond the frame
//! itself — so it is well-defined in a streaming setting.

use std::collections::VecDeque;

use sivana_core::config::AlgorithmConfig;
use sivana_core::hash::pack_hash32;
use sivana_dsp::peaks_v2::{Peak, PeakStreamer};
use sivana_dsp::stft::StftStreamer;
use sivana_dsp::window::hann_periodic;

/// A 32-bit pair fingerprint with its anchor time in frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint32 {
    pub hash: u32,
    pub anchor_time: u32,
}

#[derive(Debug, Clone)]
pub struct LandmarkV2Config {
    pub fft_window: usize,
    pub hop: usize,
    pub peaks: sivana_dsp::peaks_v2::PeaksV2Config,
    /// Targets per anchor ("fanout", §12).
    pub fanout: usize,
    /// Target zone bounds in frames.
    pub dt_min: usize,
    pub dt_max: usize,
    /// Frequency quantization: number of log-spaced bands across
    /// [0, nyquist] mapped into the 12-bit field (§13).
    pub freq_bands: u16,
}

impl Default for LandmarkV2Config {
    fn default() -> Self {
        Self {
            fft_window: 2048,
            hop: 1024,
            peaks: sivana_dsp::peaks_v2::PeaksV2Config::default(),
            fanout: 8,
            dt_min: 1,
            dt_max: 50,
            // ~26 bands/octave over the 10 usable octaves; every value here
            // awaits its benchmark sweep (PLAN.md §92, E3).
            freq_bands: 256,
        }
    }
}

impl From<&AlgorithmConfig> for LandmarkV2Config {
    fn from(c: &AlgorithmConfig) -> Self {
        Self {
            fft_window: c.fft.window_size,
            hop: c.fft.hop_size,
            ..Default::default()
        }
    }
}

/// Strength-term ceiling in dB: prominence above the frame's noise floor
/// saturates here. 60 dB comfortably exceeds the 8 dB acceptance gate.
const STRENGTH_CEILING_DB: f32 = 60.0;

/// Quantize a frequency bin to a log-spaced band index in `[0, bands)`.
///
/// The mapping covers `[bin 1 .. nyquist]` logarithmically over
/// `log2(total_bins - 1)` octaves (10 for the default 2048-point window),
/// so every band index lands inside the 12-bit field regardless of FFT
/// size. Bin 0 (DC) collapses into band 0.
fn quantize_bin(bin: usize, total_bins: usize, bands: u16) -> u16 {
    if bin == 0 || bands == 0 {
        return 0;
    }
    let f = (bin as f64 + 0.5) / total_bins as f64; // fraction of nyquist
    let octaves = ((total_bins.saturating_sub(1)).max(2) as f64).log2();
    let pos = (f.max(1e-9).log2() + octaves) / octaves; // [0..1] over the octaves
    let band = (pos.clamp(0.0, 1.0 - 1e-9) * bands as f64) as u16;
    band.min(bands - 1)
}

struct PendingAnchor {
    time_idx: u64,
    f1q: u16,
}

struct FramePeaks {
    time: u64,
    peaks: Vec<Peak>,
}

/// Best target found so far within one temporal slot of an anchor's zone.
struct SlotBest {
    score: f32,
    f2q: u16,
    dt: u8,
}

/// Streaming landmark fingerprinter (Engine A, V2).
///
/// Feed PCM chunks ([`Self::process`]); finished fingerprints come out in
/// anchor-time order. Call [`Self::finish`] after the last chunk — it
/// flushes the peak detector's lookahead and finalizes anchors near the
/// end of the stream with whatever target frames exist (matching offline
/// edge semantics; no partial STFT window is analyzed, exactly like the
/// batch frame-count formula).
pub struct LandmarkStreamer {
    cfg: LandmarkV2Config,
    stft: StftStreamer,
    peaks: PeakStreamer,
    total_bins: usize,

    /// Received peak frames still potentially needed: any frame in
    /// `[oldest_anchor.t + dt_min, newest_decided_frame]`.
    frames: VecDeque<FramePeaks>,
    /// Anchors awaiting target-zone completion, in creation (time) order.
    anchors: VecDeque<PendingAnchor>,

    // Scratch — allocated once, cleared per use.
    mags: Vec<f32>,
    peak_out: Vec<Peak>,
    slot_best: Vec<Option<SlotBest>>,
}

impl LandmarkStreamer {
    pub fn new(cfg: &LandmarkV2Config) -> Self {
        let window = hann_periodic(cfg.fft_window);
        let stft = StftStreamer::new(cfg.fft_window, cfg.hop, &window);
        let total_bins = cfg.fft_window / 2 + 1;
        let peaks = PeakStreamer::new(total_bins, cfg.peaks.clone());
        Self {
            cfg: cfg.clone(),
            stft,
            peaks,
            total_bins,
            frames: VecDeque::new(),
            anchors: VecDeque::new(),
            mags: Vec::new(),
            peak_out: Vec::new(),
            slot_best: Vec::new(),
        }
    }

    /// Feed PCM; append newly finalized fingerprints to `out` (cleared
    /// first, like the DSP primitives' `_into` convention).
    pub fn process(&mut self, samples: &[f32], out: &mut Vec<Fingerprint32>) {
        out.clear();
        self.stft.feed(samples);
        while let Some(frame_idx) = self.stft.next_frame(&mut self.mags) {
            self.peaks.process_frame(&self.mags, &mut self.peak_out);
            if !self.peak_out.is_empty() {
                let peaks: Vec<Peak> = self.peak_out.split_off(0);
                self.ingest_frame(frame_idx, peaks, out);
            }
        }
    }

    /// Flush end-of-stream state; append any fingerprints whose target
    /// zones complete against the final frames.
    pub fn finish(&mut self, out: &mut Vec<Fingerprint32>) {
        out.clear();
        self.peaks.finish(&mut self.peak_out);
        if !self.peak_out.is_empty() {
            let last_idx = self
                .frames
                .back()
                .map_or(0, |f| f.time)
                .max(self.next_expected_frame());
            let peaks: Vec<Peak> = self.peak_out.split_off(0);
            self.ingest_frame(last_idx, peaks, out);
        }
        // Finalize everything left with truncated target zones.
        while let Some(anchor) = self.anchors.pop_front() {
            self.emit_for_anchor(&anchor, out);
        }
        self.frames.clear();
    }

    fn next_expected_frame(&self) -> u64 {
        // The streamer's next undecided frame index; used only to label a
        // flushed peak batch that arrives after all prior frames.
        self.stft.frames_emitted()
    }

    /// Register a decided frame's peaks, open new anchors, and finalize
    /// every anchor whose target zone is now fully covered.
    fn ingest_frame(&mut self, frame_idx: u64, peaks: Vec<Peak>, out: &mut Vec<Fingerprint32>) {
        for p in &peaks {
            let f1q = quantize_bin(p.freq_bin_idx, self.total_bins, self.cfg.freq_bands);
            self.anchors.push_back(PendingAnchor {
                time_idx: frame_idx,
                f1q,
            });
        }
        self.frames.push_back(FramePeaks {
            time: frame_idx,
            peaks,
        });

        // Anchors whose entire zone [t+dt_min, t+dt_max] has arrived.
        while let Some(front) = self.anchors.front() {
            if front.time_idx + self.cfg.dt_max as u64 <= frame_idx {
                let anchor = self.anchors.pop_front().expect("front existed");
                self.emit_for_anchor(&anchor, out);
            } else {
                break;
            }
        }

        // Drop frames no pending or future anchor can reference.
        match self.anchors.front() {
            Some(oldest) => {
                let horizon = oldest.time_idx + self.cfg.dt_min as u64;
                while self.frames.front().is_some_and(|f| f.time < horizon) {
                    self.frames.pop_front();
                }
            }
            None => self.frames.clear(),
        }
    }

    /// Choose best-scoring targets per temporal slot and pack fingerprints.
    ///
    /// Targets are limited to frames actually received — an anchor near the
    /// end of a stream simply sees a truncated zone, matching batch
    /// behaviour where the spectrogram ends.
    fn emit_for_anchor(&mut self, anchor: &PendingAnchor, out: &mut Vec<Fingerprint32>) {
        let zone_width = self.cfg.dt_max.saturating_sub(self.cfg.dt_min) + 1;
        let slots = self.cfg.fanout.min(zone_width).max(1);
        let step = (zone_width / slots).max(1);

        self.slot_best.clear();
        self.slot_best.resize_with(slots, || None);

        let lo_time = anchor.time_idx + self.cfg.dt_min as u64;
        let hi_time = anchor.time_idx + self.cfg.dt_max as u64;

        for fr in &self.frames {
            if fr.time < lo_time {
                continue;
            }
            if fr.time > hi_time {
                break; // frames are time-ordered
            }
            let dt = (fr.time - anchor.time_idx) as usize;
            let slot = ((dt - self.cfg.dt_min) / step).min(slots - 1);
            for p in &fr.peaks {
                // Separation is scored in band space — the same quantized
                // domain the packed hash uses, so scoring and matching agree.
                let f2q = quantize_bin(p.freq_bin_idx, self.total_bins, self.cfg.freq_bands);
                let dfq = f2q.abs_diff(anchor.f1q);
                let strength =
                    p.prominence_db.clamp(0.0, STRENGTH_CEILING_DB) / STRENGTH_CEILING_DB;
                let score = dfq as f32 * 0.5 + strength * 64.0;
                let better = match &self.slot_best[slot] {
                    None => true,
                    Some(b) => score > b.score,
                };
                if better {
                    self.slot_best[slot] = Some(SlotBest {
                        score,
                        f2q,
                        dt: dt.min(255) as u8,
                    });
                }
            }
        }

        let f1q = anchor.f1q;
        for (slot, best) in self.slot_best.iter().enumerate() {
            if let Some(b) = best {
                debug_assert!(slot < slots);
                out.push(Fingerprint32 {
                    hash: pack_hash32(f1q, b.f2q, b.dt).0,
                    anchor_time: anchor.time_idx as u32,
                });
            }
        }
    }
}

/// Fingerprint mono PCM with the V2 pipeline (batch convenience form).
///
/// Equivalent to feeding all samples through a [`LandmarkStreamer`] and
/// finishing — one code path, two shapes. `sample_rate` is reserved for
/// rate-aware variants; scoring itself is frame/bin-index based.
pub fn fingerprint(
    samples: &[f32],
    _sample_rate: u32,
    cfg: &LandmarkV2Config,
) -> Vec<Fingerprint32> {
    let mut streamer = LandmarkStreamer::new(cfg);
    let mut out = Vec::new();
    let mut chunk = Vec::new();
    const FEED_LEN: usize = 4096;
    for piece in samples.chunks(FEED_LEN) {
        streamer.process(piece, &mut chunk);
        out.append(&mut chunk);
    }
    streamer.finish(&mut chunk);
    out.append(&mut chunk);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_pair(sr: u32) -> Vec<f32> {
        let n = sr as usize * 4;
        (0..n)
            .map(|i| {
                let t = i as f32 / sr as f32;
                0.5 * (std::f32::consts::TAU * 1000.0 * t).sin()
                    + 0.3 * (std::f32::consts::TAU * 3000.0 * t).sin()
            })
            .collect()
    }

    #[test]
    fn produces_fingerprints_for_tonal_audio() {
        let fps = fingerprint(&tone_pair(22_050), 22_050, &LandmarkV2Config::default());
        assert!(!fps.is_empty());
        assert!(fps.len() > 50);
    }

    #[test]
    fn determinism_same_input_same_hashes() {
        let sig = tone_pair(16_000);
        let a = fingerprint(&sig, 16_000, &LandmarkV2Config::default());
        let b = fingerprint(&sig, 16_000, &LandmarkV2Config::default());
        assert_eq!(a, b);
    }

    #[test]
    fn silence_yields_nothing() {
        let fps = fingerprint(
            &vec![0.0f32; 22_050 * 2],
            22_050,
            &LandmarkV2Config::default(),
        );
        assert!(fps.is_empty());
    }

    #[test]
    fn hashes_fit_32_bits_with_high_low_split() {
        use sivana_core::hash::{Hash32, unpack_hash32};
        let fps = fingerprint(&tone_pair(22_050), 22_050, &LandmarkV2Config::default());
        for fp in fps.iter().take(200) {
            let parts = unpack_hash32(Hash32(fp.hash));
            assert!(parts.dt <= 50, "dt {}/255 out of zone", parts.dt);
            // Round-trip through the fields proves the split is consistent.
            assert_eq!(pack_hash32(parts.f1, parts.f2, parts.dt).0, fp.hash);
            assert_eq!(Hash32(fp.hash).high16(), (fp.hash >> 16) as u16);
        }
    }

    #[test]
    fn streaming_matches_batch_exactly() {
        let sig = tone_pair(22_050);
        let cfg = LandmarkV2Config::default();
        let batch = fingerprint(&sig, 22_050, &cfg);

        let mut s = LandmarkStreamer::new(&cfg);
        let mut got = Vec::new();
        let mut chunk = Vec::new();
        for piece in sig.chunks(3333) {
            s.process(piece, &mut chunk);
            got.append(&mut chunk);
        }
        s.finish(&mut chunk);
        got.append(&mut chunk);
        assert_eq!(got.len(), batch.len(), "same fingerprint count");
        for (a, b) in got.iter().zip(batch.iter()) {
            assert_eq!(a.hash, b.hash, "stream vs batch hash");
            assert_eq!(a.anchor_time, b.anchor_time);
        }
    }

    #[test]
    fn quantize_bin_spans_full_band_range_logarithmically() {
        // Regression: the pre-streaming mapping only ever reached half the
        // band space; the top bins must now land near the top band.
        let total = 1025;
        assert_eq!(quantize_bin(0, total, 256), 0);
        let top = quantize_bin(1024, total, 256);
        let low = quantize_bin(1, total, 256);
        assert!(top >= 250, "top bin band {top} should approach 256");
        assert!(low < 30, "first band {low} should sit near 0");
        assert!(low < top, "monotonic in bin");
    }
}
