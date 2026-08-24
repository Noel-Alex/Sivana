//! WSOLA time-stretching: change duration without changing pitch (§49).
//!
//! Waveform-Similarity Overlap-Add with fixed analysis hops and
//! similarity-searched synthesis offsets. Deterministic: the search scans
//! left-to-right and breaks ties toward the earlier candidate.

/// Time-stretch mono audio by `factor` (>1 = longer output, same pitch).
///
/// `tolerance_samples` bounds the similarity search window; ~15 ms is the
/// classic choice for speech/music at 22 kHz.
pub fn time_stretch(samples: &[f32], sample_rate: u32, factor: f64, tolerance_ms: f64) -> Vec<f32> {
    assert!(factor > 0.0, "stretch factor must be positive");
    let n = samples.len();
    if n < 4 || (factor - 1.0).abs() < 1e-9 {
        return samples.to_vec();
    }

    let frame = (sample_rate as f64 * 0.04) as usize; // 40 ms analysis frame
    let overlap = frame / 2;
    let hop_out = overlap;
    let hop_in = ((overlap as f64 / factor) as usize).max(1);
    let tol = ((sample_rate as f64 * tolerance_ms / 1000.0) as usize).max(1);

    // Hann window for crossfade (periodic, matches the rest of the crate).
    let mut win = vec![0.0f32; frame];
    for (i, w) in win.iter_mut().enumerate() {
        *w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / frame as f32).cos();
    }

    let out_len = ((n as f64) * factor).ceil() as usize + frame;
    let mut out = vec![0.0f32; out_len];
    let mut norm = vec![0.0f32; out_len];

    let mut in_pos = 0usize; // analysis position of the current frame
    let mut out_pos = 0usize; // synthesis position
    let mut first = true;

    while in_pos + frame < n && out_pos + frame < out_len {
        // Similarity search: align this frame's onset against the tail of
        // what was already written (natural continuation), skipping search
        // for the very first frame.
        let delta = if first {
            0i64
        } else {
            best_delta(samples, in_pos, frame, overlap, tol, &out, out_pos)
        };
        // Apply delta: read the frame starting at in_pos + delta.
        let src = (in_pos as i64 + delta).max(0) as usize;
        let src_end = (src + frame).min(n);
        if src_end <= src {
            break;
        }
        let len = src_end - src;

        for i in 0..len {
            out[out_pos + i] += samples[src + i] * win[i];
            norm[out_pos + i] += win[i];
        }
        first = false;
        in_pos += hop_in;
        out_pos += hop_out;
    }

    // Normalize overlapping regions; pass through uncovered head/tail raw.
    let mut result = vec![0.0f32; out_len];
    let covered = out_pos + frame;
    for i in 0..covered.min(out_len) {
        result[i] = if norm[i] > 1e-6 {
            out[i] / norm[i]
        } else {
            out[i]
        };
    }
    // Trim to the expected duration.
    let want = ((n as f64) * factor).round() as usize;
    result.truncate(want.min(out_len));
    result
}

fn best_delta(
    samples: &[f32],
    in_pos: usize,
    frame: usize,
    overlap: usize,
    tol: usize,
    out: &[f32],
    out_pos: usize,
) -> i64 {
    // Correlate the incoming frame's overlap region against the last
    // `overlap` samples already synthesized; pick the offset minimizing
    // squared error (earliest wins ties).
    let seg_start = in_pos.saturating_sub(tol);
    let seg_end = (in_pos + tol).min(frame.max(1) + in_pos);
    let mut best = 0i64;
    let mut best_err = f32::INFINITY;
    for cand in (seg_start..=seg_end.min(in_pos)).rev() {
        let mut err = 0.0f32;
        for k in 0..overlap {
            let a_idx = cand + k;
            let b_idx = out_pos.saturating_sub(overlap) + k;
            if a_idx >= samples.len() || b_idx >= out.len() {
                break;
            }
            let d = samples[a_idx] - out[b_idx];
            err += d * d;
            if err > best_err {
                break;
            }
        }
        if err < best_err {
            best_err = err;
            best = cand as i64 - in_pos as i64;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, sr: u32, seconds: f32) -> Vec<f32> {
        let n = (sr as f32 * seconds) as usize;
        (0..n)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin())
            .collect()
    }

    #[test]
    fn duration_scales_pitch_survives() {
        let sr = 22_050u32;
        let sig = tone(440.0, sr, 2.0);
        let stretched = time_stretch(&sig, sr, 1.25, 15.0);
        // Duration scaled within a small tolerance.
        let expect = (sig.len() as f64 * 1.25) as usize;
        assert!(
            (stretched.len() as i64 - expect as i64).abs() < (sr as f32 * 0.05) as i64,
            "len {} vs expected {}",
            stretched.len(),
            expect
        );
        // Dominant period stays 1/440 s: count zero crossings per second.
        let crossings = |s: &[f32]| -> f64 {
            s.windows(2)
                .filter(|w| w[0].signum() != w[1].signum())
                .count() as f64
                / (s.len() as f64 / sr as f64)
                / 2.0
        };
        let f_est = crossings(&stretched);
        assert!(
            (f_est - 440.0).abs() < 30.0,
            "frequency drifted: {f_est:.1} Hz"
        );
    }

    #[test]
    fn identity_factor_returns_input_length() {
        let sig = tone(300.0, 16_000, 0.5);
        let same = time_stretch(&sig, 16_000, 1.0, 15.0);
        assert_eq!(same.len(), sig.len());
    }

    #[test]
    fn deterministic_output() {
        let sig = tone(220.0, 16_000, 1.0);
        let a = time_stretch(&sig, 16_000, 1.3, 12.0);
        let b = time_stretch(&sig, 16_000, 1.3, 12.0);
        assert_eq!(a, b);
    }
}
