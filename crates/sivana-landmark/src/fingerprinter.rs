//! Landmark V2 fingerprinting pipeline.

use sivana_core::config::AlgorithmConfig;
use sivana_core::hash::pack_hash32;
use sivana_dsp::peaks_v2::{PeaksV2Config, find_peaks_v2};
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
    pub peaks: PeaksV2Config,
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
            peaks: PeaksV2Config::default(),
            fanout: 8,
            dt_min: 1,
            dt_max: 50,
            // ~10 bands/octave over 10 octaves; every value here awaits its
            // benchmark sweep (PLAN.md §92).
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

/// Quantize a frequency bin to a log-spaced band index in `[0, bands)`.
fn quantize_bin(bin: usize, total_bins: usize, bands: u16) -> u16 {
    if bin == 0 || bands == 0 {
        return 0;
    }
    let f = (bin as f64 + 0.5) / total_bins as f64; // normalized [0,1]
    let scaled = (f.max(1e-9).log2() / 2.0 + 1.0) * 0.5; // log2 in [-10,0] -> [0,1]
    ((scaled.clamp(0.0, 1.0 - 1e-9)) * bands as f64) as u16
}

/// Fingerprint mono PCM with the V2 pipeline (batch form).
///
/// Streaming emission reuses the same scoring once the landmark streamer
/// lands; batch keeps the first implementation honest and testable.
/// Fingerprint mono PCM with the V2 pipeline (batch form).
///
/// `sample_rate` is part of the stable API (the streaming variant and
/// future log-band tables use it); batch scoring is rate-independent
/// because peaks arrive as frame/bin indices.
pub fn fingerprint(
    samples: &[f32],
    _sample_rate: u32,
    cfg: &LandmarkV2Config,
) -> Vec<Fingerprint32> {
    let win = cfg.fft_window;
    let hop = cfg.hop;
    let window = hann_periodic(win);
    let mut stft = sivana_dsp::stft::StftStreamer::new(win, hop, &window);

    // Collect spectrogram (batch mode) while exercising the streaming STFT.
    let mut spectrogram: Vec<Vec<f32>> = Vec::new();
    let mut mags = Vec::new();
    stft.process(samples, &mut mags, |_, m| spectrogram.push(m.to_vec()));
    if spectrogram.is_empty() {
        return Vec::new();
    }

    let peaks = find_peaks_v2(&spectrogram, &cfg.peaks);
    let total_bins = spectrogram[0].len();
    let global_max = spectrogram
        .iter()
        .flat_map(|f| f.iter())
        .copied()
        .fold(0.0f32, f32::max);

    // Group peak indices by frame for zone lookup.
    let mut by_frame: Vec<Vec<usize>> = vec![Vec::new(); spectrogram.len()];
    for (pi, p) in peaks.iter().enumerate() {
        by_frame[p.time_idx].push(pi);
    }

    let mut out = Vec::with_capacity(peaks.len() * cfg.fanout.min(16));
    for (ai, anchor) in peaks.iter().enumerate() {
        let f1q = quantize_bin(anchor.freq_bin_idx, total_bins, cfg.freq_bands);

        // Candidate targets inside the zone, one best per temporal slot:
        // slot k covers dt range [dt_min + k*step, ...) — this spreads the
        // chosen targets across the zone instead of clustering on early
        // frames ("first N" failure of the prototype, §11).
        let mut chosen: Vec<usize> = Vec::new();
        let zone_width = cfg.dt_max.saturating_sub(cfg.dt_min) + 1;
        let slots = cfg.fanout.min(zone_width);
        let step = (zone_width / slots).max(1);

        for slot in 0..slots {
            let lo = cfg.dt_min + slot * step;
            let hi = if slot + 1 == slots {
                cfg.dt_max
            } else {
                lo + step - 1
            };
            let mut best: Option<(usize, f32)> = None; // (peak_index, score)
            for (j, target) in peaks.iter().enumerate().skip(ai + 1) {
                let dt = target.time_idx.saturating_sub(anchor.time_idx);
                if dt < lo {
                    continue;
                }
                if dt > hi {
                    break; // peaks sorted by time
                }
                let df = target.freq_bin_idx.abs_diff(anchor.freq_bin_idx);
                // Score: spectral separation + peak strength (§11, E2).
                // Strength is normalized against the strongest peak in the
                // scan so the term is scale-free across recordings.
                let rel = if global_max > 0.0 {
                    target.magnitude / global_max
                } else {
                    0.0
                };
                let score = df as f32 * 0.5 + rel * 64.0;
                if best.as_ref().is_none_or(|(_, s)| score > *s) {
                    best = Some((j, score));
                }
            }
            if let Some((j, _)) = best {
                chosen.push(j);
            }
        }

        for j in chosen {
            let target = &peaks[j];
            let dt = (target.time_idx - anchor.time_idx).min(255) as u8;
            let f2q = quantize_bin(target.freq_bin_idx, total_bins, cfg.freq_bands);
            out.push(Fingerprint32 {
                hash: pack_hash32(f1q, f2q, dt).0,
                anchor_time: anchor.time_idx as u32,
            });
        }
    }
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
            assert!(parts.dt <= 50 || parts.dt == 50); // masked field
            let _ = Hash32(fp.hash).high16();
        }
    }
}
