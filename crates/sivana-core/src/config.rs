//! Algorithm configuration schema (research/PLAN.md §78).
//!
//! One serializable structure describing every algorithmic knob so that
//! benchmark sweeps (§55), fingerprint headers (§36) and future engines
//! all speak the same language.
//!
//! `AlgorithmConfig::legacy()` reproduces the frozen prototype's exact
//! parameters; it is the baseline every change must beat.

use serde::{Deserialize, Serialize};

/// Candidate target sample rates for benchmarking (§7).
pub const BENCHMARK_SAMPLE_RATES_HZ: [u32; 4] = [8_000, 11_025, 16_000, 22_050];

/// Candidate fanouts per anchor for benchmarking (§12).
pub const BENCHMARK_FANOUTS: [usize; 5] = [5, 8, 10, 12, 15];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlgorithmConfig {
    /// Input sample rate after normalization.
    pub sample_rate_hz: u32,
    pub fft: FftConfig,
    pub peaks: PeakConfig,
    pub landmarks: LandmarkConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowType {
    Hann,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FftConfig {
    pub window_size: usize,
    pub hop_size: usize,
    pub window_type: WindowType,
}

/// Peak extraction parameters.
///
/// `*_legacy` fields reproduce the frozen prototype exactly. The `Option`
/// fields are the V2 knobs (adaptive noise floor §9.2, density control
/// §9.3, band quotas §9.4); `None` means "not active", which keeps the
/// schema forward-compatible without pretending V2 behaviour exists yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeakConfig {
    pub neighborhood_time_radius: usize,
    pub neighborhood_freq_radius: usize,
    pub min_magnitude_threshold: f32,
    /// V2: prominence above the local noise floor in dB.
    pub min_prominence_db: Option<f32>,
    /// V2: enforced peak density budget (peaks per second of audio).
    pub density_peaks_per_second: Option<f32>,
    /// V2: relative quota per log-spaced frequency band (sums to 1.0).
    pub band_quotas: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LandmarkConfig {
    pub dt_min_frames: usize,
    pub dt_max_frames: usize,
    pub df_abs_max_bins: usize,
    /// Targets paired with each anchor ("fanout", §12).
    pub fanout: usize,
}

impl Default for FftConfig {
    fn default() -> Self {
        Self {
            window_size: 2048,
            hop_size: 1024,
            window_type: WindowType::Hann,
        }
    }
}

impl Default for PeakConfig {
    fn default() -> Self {
        Self {
            neighborhood_time_radius: 2,
            neighborhood_freq_radius: 5,
            min_magnitude_threshold: 2.0,
            min_prominence_db: None,
            density_peaks_per_second: None,
            band_quotas: None,
        }
    }
}

impl Default for LandmarkConfig {
    fn default() -> Self {
        Self {
            dt_min_frames: 1,
            dt_max_frames: 50,
            df_abs_max_bins: 200,
            fanout: 5,
        }
    }
}

impl Default for AlgorithmConfig {
    fn default() -> Self {
        Self::legacy()
    }
}

impl AlgorithmConfig {
    /// Exact parameters of the frozen prototype (`legacy/src/main.rs`).
    pub fn legacy() -> Self {
        Self {
            sample_rate_hz: 22_050,
            fft: FftConfig {
                window_size: 2048,
                hop_size: 1024,
                window_type: WindowType::Hann,
            },
            peaks: PeakConfig {
                neighborhood_time_radius: 2,
                neighborhood_freq_radius: 5,
                min_magnitude_threshold: 2.0,
                min_prominence_db: None,
                density_peaks_per_second: None,
                band_quotas: None,
            },
            landmarks: LandmarkConfig {
                dt_min_frames: 1,
                dt_max_frames: 50,
                df_abs_max_bins: 200,
                fanout: 5,
            },
        }
    }

    /// Frames of audio per second at the configured FFT geometry.
    pub fn frames_per_second(&self) -> f64 {
        self.sample_rate_hz as f64 / self.fft.hop_size as f64
    }

    /// Sanity-check a configuration before use.
    pub fn validate(&self) -> Result<(), String> {
        if !BENCHMARK_SAMPLE_RATES_HZ.contains(&self.sample_rate_hz)
            && self.sample_rate_hz % 1000 != 0
        {
            return Err(format!(
                "unusual sample rate {} Hz; use one of {BENCHMARK_SAMPLE_RATES_HZ:?} or a kHz multiple",
                self.sample_rate_hz
            ));
        }
        if self.fft.window_size == 0
            || self.fft.window_size.next_power_of_two() != self.fft.window_size
        {
            return Err("fft.window_size must be a power of two".to_string());
        }
        if self.fft.hop_size == 0 || self.fft.hop_size > self.fft.window_size {
            return Err("fft.hop_size must be in (0, window_size]".to_string());
        }
        if let Some(d) = self.peaks.density_peaks_per_second {
            if !(1.0..=200.0).contains(&d) {
                return Err("density_peaks_per_second outside plausible range 1..=200".to_string());
            }
        }
        if let Some(q) = &self.peaks.band_quotas {
            let sum: f32 = q.iter().sum();
            if (sum - 1.0).abs() > 1e-3 {
                return Err("band_quotas must sum to 1.0".to_string());
            }
        }
        if self.landmarks.dt_min_frames == 0 {
            return Err("landmarks.dt_min_frames must be >= 1".to_string());
        }
        if self.landmarks.dt_max_frames < self.landmarks.dt_min_frames {
            return Err("landmarks.dt_max_frames < dt_min_frames".to_string());
        }
        Ok(())
    }
}

/// The production operating point for log-band quantization (E4): the
/// only band count with a measured zero-false-accept gate. Ingest, the
/// browser engine and the matcher MUST all use this value — a split
/// makes hashes from the two sides mathematically incapable of
/// colliding (found the hard way: browser queries at the 256-band
/// default produced literally zero overlap with a 512-band catalog).
pub const OPERATING_FREQ_BANDS: u16 = 512;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_validates() {
        assert!(AlgorithmConfig::legacy().validate().is_ok());
    }

    #[test]
    fn serde_roundtrip_preserves_legacy_defaults() {
        let cfg = AlgorithmConfig::legacy();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AlgorithmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn validation_rejects_bad_geometry() {
        let mut cfg = AlgorithmConfig::legacy();
        cfg.fft.window_size = 2047;
        assert!(cfg.validate().is_err());

        let mut cfg = AlgorithmConfig::legacy();
        cfg.landmarks.dt_max_frames = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn frames_per_second_matches_legacy_geometry() {
        let cfg = AlgorithmConfig::legacy();
        assert!((cfg.frames_per_second() - 22050.0 / 1024.0).abs() < 1e-9);
    }
}
