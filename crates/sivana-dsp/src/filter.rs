//! RBJ biquad filters (audio-eq-cookbook) and a one-pole lowpass.
//!
//! Used by the DSP-abuse degradation matrix (§48) and later by the input
//! normalization chain (§7.1). Deterministic, allocation-free per sample.

/// Direct Form I biquad: `y[n] = b0*x + b1*x1 + b2*x2 - a1*y1 - a2*y2`.
#[derive(Debug, Clone)]
pub struct Biquad {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterKind {
    LowPass,
    HighPass,
}

impl Biquad {
    /// RBJ cookbook 2nd-order Butterworth-style section.
    ///
    /// `sample_rate` and `cutoff_hz` in Hz; Q is resonance (Butterworth ≈ 0.7071).
    pub fn new(kind: FilterKind, sample_rate: f32, cutoff_hz: f32, q: f32) -> Self {
        assert!(sample_rate > 0.0);
        let w0 = std::f32::consts::TAU * cutoff_hz / sample_rate;
        // Clamp to the stable region.
        let w0 = w0.min(std::f32::consts::PI * 0.98);
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q);

        let (b0, b1, b2, a0, a1, a2) = match kind {
            FilterKind::LowPass => {
                let b1 = 1.0 - cos_w0;
                let b0 = b1 / 2.0;
                let b2 = b1 / 2.0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterKind::HighPass => {
                let b0 = (1.0 + cos_w0) / 2.0;
                let b1 = -(1.0 + cos_w0);
                let b2 = (1.0 + cos_w0) / 2.0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
        };

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Process one sample, updating internal state.
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    /// In-place filtering of an entire buffer.
    pub fn process(&mut self, samples: &mut [f32]) {
        for s in samples.iter_mut() {
            *s = self.tick(*s);
        }
    }

    /// Reset filter state to silence.
    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// Simple first-order DC-removal high-pass (`y = x - x1 + R*y1`).
///
/// Part of the planned normalization pipeline (§7.1).
pub struct DcBlocker {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    pub fn new(sample_rate: f32) -> Self {
        Self { r: 1.0 - 20.0 / sample_rate.max(21.0), x1: 0.0, y1: 0.0 }
    }

    pub fn tick(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for s in samples.iter_mut() {
            *s = self.tick(*s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, sr: f32, seconds: f32) -> Vec<f32> {
        (0..(seconds * sr) as usize)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / sr).sin())
            .collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    const Q_BUTTERWORTH: f32 = std::f32::consts::FRAC_1_SQRT_2;

    #[test]
    fn lowpass_attenuates_high_frequencies() {
        let sr = 16_000.0;
        let mut high = tone(5000.0, sr, 0.5);
        let mut lp = Biquad::new(FilterKind::LowPass, sr, 1000.0, Q_BUTTERWORTH);
        lp.process(&mut high[2000..]); // skip transient region for measurement
        let ratio = rms(&high[2000..]) / rms(&tone(5000.0, sr, 0.5));
        assert!(ratio < 0.35, "5 kHz should be strongly attenuated by 1 kHz LPF, ratio={ratio}");
    }

    #[test]
    fn highpass_blocks_low_frequencies() {
        let sr = 16_000.0;
        let mut low = tone(60.0, sr, 0.5);
        let mut hp = Biquad::new(FilterKind::HighPass, sr, 500.0, Q_BUTTERWORTH);
        hp.process(&mut low);
        assert!(rms(&low[4000..]) < 0.15 * Q_BUTTERWORTH);
    }

    #[test]
    fn passband_is_nearly_transparent() {
        let sr = 16_000.0;
        let mut mid = tone(440.0, sr, 0.5);
        let reference_rms = rms(&mid);
        let mut lp = Biquad::new(FilterKind::LowPass, sr, 8000.0, Q_BUTTERWORTH);
        lp.process(&mut mid);
        assert!(rms(&mid[4000..]) > 0.9 * reference_rms);
    }

    #[test]
    fn dc_blocker_removes_offset() {
        let mut sig: Vec<f32> = vec![0.5; 8_000];
        let mut dc = DcBlocker::new(16_000.0);
        dc.process(&mut sig);
        assert!(rms(&sig[6000..]) < 1e-3);
    }
}
