//! Degradation transforms for the benchmark matrix (§45–§50 subset).
//!
//! Each degradation is deterministic: given the same input and seed it
//! produces bit-identical output on every platform.

use serde::{Deserialize, Serialize};
use sivana_dsp::filter::{Biquad, FilterKind};
use sivana_dsp::noise::{self, NoiseColor};

/// One degradation applied to a query excerpt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Degradation {
    None,
    Gain { db: f32 },
    Clip { threshold: f32 },
    WhiteNoise { snr_db: f32 },
    PinkNoise { snr_db: f32 },
    LowPass { cutoff_hz: f32 },
    HighPass { cutoff_hz: f32 },
    Echo { delay_s: f32, gain: f32 },
    Speed { factor: f32 },
}

impl Degradation {
    /// Short stable identifier used in reports and CLI grids.
    pub fn id(&self) -> String {
        match self {
            Self::None => "clean".into(),
            Self::Gain { db } => format!("gain{db:+.0}db"),
            Self::Clip { threshold } => format!("clip{threshold:.2}"),
            Self::WhiteNoise { snr_db } => format!("white{snr_db:+.0}db"),
            Self::PinkNoise { snr_db } => format!("pink{snr_db:+.0}db"),
            Self::LowPass { cutoff_hz } => format!("lp{cutoff_hz:.0}"),
            Self::HighPass { cutoff_hz } => format!("hp{cutoff_hz:.0}"),
            Self::Echo { delay_s, gain } => format!("echo{delay_s:.2}s@{gain:.1}"),
            Self::Speed { factor } => format!("speed{factor:.2}"),
        }
    }

    /// Apply to mono samples; returns a fresh buffer (input untouched).
    pub fn apply(&self, samples: &[f32], sample_rate: u32, rng_salt: u64) -> Vec<f32> {
        match self {
            Self::None => samples.to_vec(),
            Self::Gain { db } => {
                let g = 10f32.powf(db / 20.0);
                samples.iter().map(|s| s * g).collect()
            }
            Self::Clip { threshold } => {
                let t = *threshold;
                samples.iter().map(|s| s.clamp(-t, t)).collect()
            }
            Self::WhiteNoise { snr_db } => {
                let n = noise::generate(NoiseColor::White, samples.len(), 0xBADC_0DE0 ^ rng_salt);
                noise::mix_at_snr(samples, &n, *snr_db)
            }
            Self::PinkNoise { snr_db } => {
                let n = noise::generate(NoiseColor::Pink, samples.len(), 0x0516_31D0 ^ rng_salt);
                noise::mix_at_snr(samples, &n, *snr_db)
            }
            Self::LowPass { cutoff_hz } => {
                let mut out = samples.to_vec();
                Biquad::new(
                    FilterKind::LowPass,
                    sample_rate as f32,
                    *cutoff_hz,
                    std::f32::consts::FRAC_1_SQRT_2,
                )
                .process(&mut out);
                out
            }
            Self::HighPass { cutoff_hz } => {
                let mut out = samples.to_vec();
                Biquad::new(
                    FilterKind::HighPass,
                    sample_rate as f32,
                    *cutoff_hz,
                    std::f32::consts::FRAC_1_SQRT_2,
                )
                .process(&mut out);
                out
            }
            Self::Echo { delay_s, gain } => {
                let d = (*delay_s * sample_rate as f32) as usize;
                let mut out = samples.to_vec();
                if d > 0 && d < out.len() {
                    for i in d..out.len() {
                        out[i] += gain * samples[i - d];
                    }
                }
                out
            }
            Self::Speed { factor } => sivana_dsp::resample::change_speed(samples, *factor),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(sr: u32) -> Vec<f32> {
        (0..sr as usize)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn degradations_are_deterministic() {
        let sig = tone(16_000);
        let cases = [
            Degradation::WhiteNoise { snr_db: 10.0 },
            Degradation::PinkNoise { snr_db: 10.0 },
        ];
        for c in &cases {
            assert_eq!(c.apply(&sig, 16_000, 7), c.apply(&sig, 16_000, 7));
        }
    }

    #[test]
    fn gain_and_clip_behave() {
        let sig = vec![0.5f32; 100];
        let loud = Degradation::Gain { db: 6.0 }.apply(&sig, 16_000, 0);
        assert!((loud[0] - 0.5 * 10f32.powf(0.3)).abs() < 1e-4);

        let clipped = Degradation::Clip { threshold: 0.2 }.apply(&loud, 16_000, 0);
        assert!(clipped.iter().all(|s| s.abs() <= 0.2 + 1e-6));
    }

    #[test]
    fn ids_are_stable() {
        assert_eq!(Degradation::None.id(), "clean");
        assert_eq!(Degradation::Speed { factor: 1.05 }.id(), "speed1.05");
    }
}
