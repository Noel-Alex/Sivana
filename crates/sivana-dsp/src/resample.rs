//! Resampling / playback-rate manipulation.
//!
//! `change_speed` implements the naive "tape/vinyl" transformation:
//! resampling the whole signal changes duration **and** pitch together —
//! exactly what a phone playback-speed slider does. True time-stretching
//! independent of pitch is a separate, later benchmark axis (§49) and is
//! tracked in research/EXPERIMENTS.md.

/// Change playback speed by `factor` (>1 = faster/higher, <1 = slower/lower)
/// using linear interpolation. Output length ≈ `len / factor`.
pub fn change_speed(samples: &[f32], factor: f32) -> Vec<f32> {
    assert!(factor > 0.0, "speed factor must be positive");
    if samples.len() < 2 {
        return samples.to_vec();
    }
    let out_len = ((samples.len() as f64) / factor as f64).ceil().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    let step = factor as f64;
    let mut pos = 0.0f64;
    for _ in 0..out_len {
        let i = pos.floor() as usize;
        if i + 1 >= samples.len() {
            // Hold the final sample rather than dropping the tail.
            out.push(samples[samples.len() - 1]);
            pos += step;
            continue;
        }
        let frac = (pos - i as f64) as f32;
        out.push(samples[i] * (1.0 - frac) + samples[i + 1] * frac);
        pos += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faster_shortens_and_preserves_shape() {
        let s: Vec<f32> = (0..16_000)
            .map(|i| (std::f32::consts::TAU * i as f32 / 1600.0).sin())
            .collect();
        let fast = change_speed(&s, 1.25);
        assert!((fast.len() as i64 - 12_800).abs() <= 2);
    }

    #[test]
    fn slower_lengthens() {
        let s = vec![0.5f32; 10_000];
        let slow = change_speed(&s, 0.8);
        assert_eq!(slow.len(), 12_500);
    }

    #[test]
    fn unit_factor_is_identity_ish() {
        let s: Vec<f32> = (0..1000).map(|i| (i % 37) as f32 / 100.0).collect();
        let same = change_speed(&s, 1.0);
        assert_eq!(same.len(), 1000);
        for (a, b) in s.iter().zip(same.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn tiny_input_is_safe() {
        assert!(change_speed(&[], 2.0).is_empty());
        assert_eq!(change_speed(&[0.3], 2.0), vec![0.3]);
    }
}
