//! Level math: RMS, dBFS, SNR measurement.

/// Root mean square of a buffer.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// RMS expressed as dBFS (`20 log10(rms)`); `-inf`-safe floor.
pub fn rms_dbfs(samples: &[f32]) -> f32 {
    20.0 * rms(samples).max(1e-10).log10()
}

/// Measure the signal-to-noise ratio in dB between a clean reference and
/// the residual/noisy version of it (same length).
pub fn snr_db(clean: &[f32], noisy: &[f32]) -> f32 {
    let n = clean.len().min(noisy.len());
    let mut sig_e = 0.0f32;
    let mut err_e = 0.0f32;
    for i in 0..n {
        let d = clean[i] - noisy[i];
        sig_e += clean[i] * clean[i];
        err_e += d * d;
    }
    10.0 * (sig_e.max(1e-12) / err_e.max(1e-12)).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_sine_is_peak_over_sqrt_two() {
        let sine: Vec<f32> = (0..16_000)
            .map(|i| (std::f32::consts::TAU * i as f32 / 100.0).sin())
            .collect();
        assert!((rms(&sine) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);
    }

    #[test]
    fn silence_measures_floor_not_nan() {
        assert!(rms_dbfs(&[0.0; 100]).is_finite());
        assert!(snr_db(&[0.0; 100], &[0.0; 100]).is_finite());
    }

    #[test]
    fn snr_of_identical_buffers_is_huge() {
        let s: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        // Error energy is exactly zero; the epsilon floor caps the value.
        assert!(snr_db(&s, &s) > 100.0);
    }
}
