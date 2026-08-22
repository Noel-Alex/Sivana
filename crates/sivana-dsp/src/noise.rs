//! Noise generators for the degradation matrix (§45).

use sivana_audio::rng::XorShift64Star;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseColor {
    White,
    Pink,
}

/// Generate `len` noise samples with RMS ≈ 1.0 (scale to target SNR later).
// Kellet's published filter constants are kept verbatim.
#[allow(clippy::excessive_precision)]
pub fn generate(color: NoiseColor, len: usize, seed: u64) -> Vec<f32> {
    let mut rng = XorShift64Star::new(seed | 1);
    match color {
        NoiseColor::White => {
            // Uniform [-1,1) has RMS = 1/sqrt(3); rescale to unit RMS.
            const G: f32 = 1.732_050_8;
            (0..len).map(|_| rng.next_bipolar() * G).collect()
        }
        NoiseColor::Pink => {
            // Paul Kellet's refined pink filter.
            let mut b0 = 0.0f32;
            let mut b1 = 0.0f32;
            let mut b2 = 0.0f32;
            let mut b3 = 0.0f32;
            let mut b4 = 0.0f32;
            let mut b5 = 0.0f32;
            let b6 = 0.0f32;
            let raw: Vec<f32> = (0..len)
                .map(|_| {
                    let w = rng.next_bipolar();
                    b0 = 0.99886 * b0 + w * 0.0555179;
                    b1 = 0.99332 * b1 + w * 0.0750759;
                    b2 = 0.96900 * b2 + w * 0.1538520;
                    b3 = 0.86650 * b3 + w * 0.3104856;
                    b4 = 0.55000 * b4 + w * 0.5329522;
                    b5 = -0.7616 * b5 - w * 0.0168980;
                    b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362
                })
                .collect();
            // Normalize to unit RMS so SNR math matches white noise.
            let r = crate::level::rms(&raw);
            let g = if r > 1e-9 { 1.0 / r } else { 1.0 };
            raw.into_iter().map(|s| s * g).collect()
        }
    }
}

/// Mix `noise` into `signal` at the requested SNR (dB), returning a new buffer.
///
/// The signal level is preserved; the noise is scaled to hit the target.
pub fn mix_at_snr(signal: &[f32], noise: &[f32], snr_db: f32) -> Vec<f32> {
    let n = signal.len().min(noise.len());
    let sig_rms = crate::level::rms(&signal[..n]);
    let noise_rms = crate::level::rms(&noise[..n]);
    let target_noise_rms = sig_rms / 10f32.powf(snr_db / 20.0);
    let gain = if noise_rms > 1e-9 { target_noise_rms / noise_rms } else { 0.0 };
    signal[..n]
        .iter()
        .zip(noise[..n].iter())
        .map(|(s, nz)| s + gain * nz)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::{rms, snr_db};

    #[test]
    fn white_noise_is_unit_rms() {
        let n = generate(NoiseColor::White, 100_000, 7);
        assert!((rms(&n) - 1.0).abs() < 0.05);
    }

    #[test]
    fn pink_noise_is_unit_rms_and_deterministic() {
        let a = generate(NoiseColor::Pink, 10_000, 7);
        let b = generate(NoiseColor::Pink, 10_000, 7);
        assert_eq!(a, b);
        assert!((rms(&a) - 1.0).abs() < 0.05);
    }

    #[test]
    fn mixing_hits_target_snr() {
        let sr = 16_000u32;
        let sig: Vec<f32> = (0..sr as usize)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.5)
            .collect();
        let noise = generate(NoiseColor::White, sig.len(), 99);
        for target in [20.0, 10.0, 0.0, -5.0] {
            let mixed = mix_at_snr(&sig, &noise, target);
            assert!(
                (snr_db(&sig, &mixed) - target).abs() < 0.25,
                "target {target} got {}",
                snr_db(&sig, &mixed)
            );
        }
    }
}
