//! Deterministic synthetic audio fixtures (research/PLAN.md §55).
//!
//! Real licensed music can't be committed to the repo (§42), so the
//! benchmark platform grows its own corpus: seed-generated "songs" with
//! chord pads, melodies and percussive transients — spectrally rich enough
//! for landmark fingerprinting to behave like it does on real music.
//!
//! Same seed → bit-identical samples on every platform.

use crate::rng::XorShift64Star;

/// A minor pentatonic + major scale fragments; melody picks from these.
const SCALE_SEMITONES: [f32; 12] = [
    0.0, 2.0, 3.0, 5.0, 7.0, 8.0, 10.0, 12.0, 10.0, 7.0, 5.0, 3.0,
];

/// Generate a synthetic song as mono f32 in `[-1, 1]`.
///
/// Distinct seeds produce distinct keys, tempos, progressions and melodic
/// contours; nearby seeds still share the instrument timbres so that
/// cross-song confusion is possible (good for false-positive testing).
pub fn synth_song(seed: u64, duration_s: f32, sample_rate: u32) -> Vec<f32> {
    let mut rng = XorShift64Star::new(seed);
    let total = (duration_s * sample_rate as f32) as usize;
    let sr = sample_rate as f32;

    // Per-song characteristics derived from the seed.
    let bpm = 90.0 + rng.next_f32() * 50.0; // 90–140
    let beat = 60.0 / bpm;
    let root_hz = 110.0 * 2.0_f32.powf(rng.next_bipolar() * 0.5); // ~A2 ±tritone-ish drift

    let chord_degrees = [
        rng.next_u64() % 7,
        rng.next_u64() % 7,
        rng.next_u64() % 7,
        rng.next_u64() % 7,
    ];

    let mut out = vec![0.0f32; total];

    // --- Chord pad: sustained triads changing per bar ---
    let bar_len_s = beat * 4.0;
    let mut t = 0.0;
    let mut bar_i = 0usize;
    while t < duration_s {
        let degree = chord_degrees[bar_i % chord_degrees.len()];
        let chord_root = root_hz * 2.0_f32.powf(degree as f32 / 12.0);
        let intervals = [
            (0.0, 1.0),
            (3.0 / 12.0, 0.6),
            (7.0 / 12.0, 0.7),
            (5.0 / 12.0, 0.35),
        ];
        let seg_start = (t * sr) as usize;
        let seg_end = (((t + bar_len_s).min(duration_s)) * sr) as usize;
        let seg_end = seg_end.min(total).max(seg_start);

        for (k, slot) in out[seg_start..seg_end].iter_mut().enumerate() {
            let ts = k as f32 / sr;
            // Envelope: soft attack/release inside the bar to avoid clicks.
            let env = envelope(ts, bar_len_s);
            let mut s = 0.0;
            for (ratio, amp) in intervals {
                let f = chord_root * 2.0_f32.powf(ratio);
                // Additive saw-ish: fundamental + a few harmonics.
                s += amp
                    * 0.22
                    * ((std::f32::consts::TAU * f * ts).sin()
                        + 0.45 * (std::f32::consts::TAU * f * 2.0 * ts).sin()
                        + 0.22 * (std::f32::consts::TAU * f * 3.0 * ts).sin());
            }
            *slot += s * env;
        }
        t += bar_len_s;
        bar_i += 1;
    }

    // --- Melody: eighth notes from the scale, sine with decay envelope ---
    let note_len = beat / 2.0;
    let mut nt = 0.0;
    while nt < duration_s {
        if rng.next_f32() < 0.85 {
            let semi = SCALE_SEMITONES[(rng.next_u64() % SCALE_SEMIN_LEN) as usize];
            let f = root_hz * 4.0 * 2.0_f32.powf(semi / 12.0); // two octaves up
            let start = (nt * sr) as usize;
            let end = (((nt + note_len).min(duration_s)) * sr) as usize;
            let end = end.min(total);
            for (k, slot) in out[start..end].iter_mut().enumerate() {
                let ts = k as f32 / sr;
                let env = (-ts * 6.0).exp().min(1.0);
                *slot += 0.30 * env * (std::f32::consts::TAU * f * ts).sin();
            }
        }
        nt += note_len;
    }

    // --- Percussion: noise bursts on beats 1 and 3 ---
    let mut bt = 0.0;
    let mut beat_i = 0usize;
    while bt < duration_s {
        if beat_i % 2 == 0 && rng.next_f32() < 0.9 {
            let start = (bt * sr) as usize;
            let burst_len = (0.06 * sr) as usize;
            for (k, slot) in out[start.min(total)..]
                .iter_mut()
                .take(burst_len)
                .enumerate()
            {
                let env = (-(k as f32 / burst_len as f32) * 7.0).exp();
                *slot += 0.25 * env * rng.next_bipolar();
            }
        }
        bt += beat;
        beat_i += 1;
        // Advance the shared RNG so percussion placement varies even when
        // earlier branches were skipped.
        let _ = rng.next_f32();
    }

    normalize_peak(&mut out, 0.89);
    fade_edges(&mut out, sample_rate, 0.01);
    out
}

const SCALE_SEMIN_LEN: u64 = SCALE_SEMITONES.len() as u64;

fn envelope(pos_s: f32, len_s: f32) -> f32 {
    let attack = 0.05_f32.min(len_s * 0.2);
    let release = 0.15_f32.min(len_s * 0.3);
    let a = if pos_s < attack { pos_s / attack } else { 1.0 };
    let rel_pos = len_s - pos_s;
    let r = if rel_pos < release {
        rel_pos.max(0.0) / release
    } else {
        1.0
    };
    a * r
}

/// Scale so peak absolute amplitude equals `target`.
pub fn normalize_peak(samples: &mut [f32], target: f32) {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 1e-9 {
        let g = target / peak;
        for s in samples.iter_mut() {
            *s *= g;
        }
    }
}

/// Linear fades at both ends to keep transients clean.
pub fn fade_edges(samples: &mut [f32], sample_rate: u32, seconds: f32) {
    let n = (seconds * sample_rate as f32) as usize;
    let n = n.min(samples.len() / 2);
    for i in 0..n {
        let g = i as f32 / n as f32;
        samples[i] *= g;
        samples[samples.len() - 1 - i] *= g;
    }
}

/// Extract `[start_s, start_s + dur_s)` as a copy; clamps to signal bounds.
pub fn excerpt(samples: &[f32], sample_rate: u32, start_s: f32, dur_s: f32) -> Vec<f32> {
    let start = ((start_s * sample_rate as f32) as usize).min(samples.len());
    let end = (start + (dur_s * sample_rate as f32) as usize).min(samples.len());
    samples[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_bit_identical() {
        let a = synth_song(1234, 2.0, 16_000);
        let b = synth_song(1234, 2.0, 16_000);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seeds_differ() {
        let a = synth_song(1, 2.0, 16_000);
        let b = synth_song(2, 2.0, 16_000);
        assert_ne!(a, b);
    }

    #[test]
    fn output_is_clean_audio() {
        let s = synth_song(99, 3.0, 22_050);
        assert_eq!(s.len(), (3.0f32 * 22_050.0) as usize);
        for v in s.iter() {
            assert!(v.is_finite(), "non-finite sample");
            assert!(*v <= 1.0 && *v >= -1.0, "sample out of range: {v}");
        }
    }

    #[test]
    fn excerpt_clamps_to_bounds() {
        let s = synth_song(5, 1.0, 8_000);
        let e = excerpt(&s, 8_000, 0.5, 10.0);
        assert_eq!(e.len(), s.len() / 2);
        let past = excerpt(&s, 8_000, 50.0, 1.0);
        assert!(past.is_empty());
    }
}
