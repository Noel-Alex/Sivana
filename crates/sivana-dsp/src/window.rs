//! Window functions.

/// Periodic Hann window of length `n` (DFT-even; correct choice for STFT).
pub fn hann_periodic(n: usize) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|i| 0.5 * (1.0 - (std::f32::consts::TAU * i as f32 / n as f32).cos()))
        .collect()
}

/// Symmetric Hann of length `n` — what the frozen prototype uses.
pub fn hann_symmetric(n: usize) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![1.0];
    }
    (0..n)
        .map(|i| 0.5 * (1.0 - (std::f32::consts::PI * i as f32 / (n - 1) as f32).cos()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodic_hann_properties() {
        let w = hann_periodic(8);
        assert_eq!(w.len(), 8);
        assert!((w[0]).abs() < 1e-6);
        assert!((w[4] - 1.0).abs() < 1e-6); // peak at center for even n
    }

    #[test]
    fn symmetric_matches_legacy_definition() {
        // legacy/spectrogram.rs lines 16-18.
        let n = 9;
        let w = hann_symmetric(n);
        for (i, actual) in w.iter().enumerate() {
            let expected = 0.5 * (1.0 - (std::f32::consts::PI * i as f32 / 8.0).cos());
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn edge_cases() {
        assert!(hann_periodic(0).is_empty());
        assert_eq!(hann_symmetric(1), vec![1.0]);
    }
}
