//! Peak detection V2: linear-time local maxima with adaptive acceptance
//! (§9).
//!
//! Replaces the frozen prototype's brute-force 2D scan:
//!
//! * local-max test via separable centered sliding max — `O(T*F)`
//! * absolute magnitude floor replaced by per-frame prominence over a
//!   robust noise-floor estimate (`median - k*sigma` style, in dB)
//! * density controlled by keeping the strongest `max_peaks_per_frame`
//!   survivors
//!
//! Legacy equivalence mode: set `min_prominence_db = f32::NEG_INFINITY`
//! and an absolute floor to reproduce the old filter (modulo the old
//! strict tie-break), which the property tests exploit as an oracle.

/// A spectral event: one (frame, bin) cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peak {
    pub time_idx: usize,
    pub freq_bin_idx: usize,
}

#[derive(Debug, Clone)]
pub struct PeaksV2Config {
    /// Neighborhood radius along time (frames).
    pub time_radius: usize,
    /// Neighborhood radius along frequency (bins).
    pub freq_radius: usize,
    /// Required margin above the estimated local noise floor (dB).
    pub min_prominence_db: f32,
    /// Absolute dBFS-ish floor below which cells are never peaks
    /// (`f32::NEG_INFINITY` disables).
    pub absolute_floor: f32,
    /// Keep at most this many peaks per frame (density control §9.3).
    pub max_peaks_per_frame: usize,
}

impl Default for PeaksV2Config {
    fn default() -> Self {
        // First-cut V2 defaults; every value here must eventually be
        // justified by a benchmark sweep (PLAN.md §92).
        Self {
            time_radius: 2,
            freq_radius: 5,
            min_prominence_db: 8.0,
            absolute_floor: f32::NEG_INFINITY,
            max_peaks_per_frame: 8,
        }
    }
}

fn to_db(mags: &[f32], out: &mut Vec<f32>) {
    out.clear();
    out.reserve(mags.len());
    for m in mags {
        out.push(20.0 * m.max(1e-10).log10());
    }
}

/// Detect peaks over a whole spectrogram (batch form).
///
/// `spectrogram[t]` is the magnitude spectrum of frame t. Streaming form
/// will arrive with the landmark streamer (needs `time_radius` lookahead).
pub fn find_peaks_v2(spectrogram: &[Vec<f32>], cfg: &PeaksV2Config) -> Vec<Peak> {
    let mut peaks = Vec::new();
    if spectrogram.is_empty() || spectrogram[0].is_empty() {
        return peaks;
    }
    let t_len = spectrogram.len();
    let f_len = spectrogram[0].len();

    // Precompute per-frame dB spectra once.
    let mut db_frames: Vec<Vec<f32>> = Vec::with_capacity(t_len);
    let mut scratch = Vec::new();
    for frame in spectrogram {
        to_db(frame, &mut scratch);
        db_frames.push(scratch.clone());
    }

    // Per-frame noise floor: median dB of the frame (robust baseline §9.2).
    let mut sorted = Vec::with_capacity(f_len);
    let mut floors = vec![0.0f32; t_len];
    for (t, frame_db) in db_frames.iter().enumerate() {
        sorted.clear();
        sorted.extend_from_slice(frame_db);
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        floors[t] = sorted[sorted.len() / 2];
    }

    // Sliding max along frequency per frame (centered, truncated edges).
    let mut freq_max: Vec<Vec<f32>> = Vec::with_capacity(t_len);
    for frame in spectrogram {
        freq_max.push(crate::sliding_max::sliding_max_centered(
            frame,
            cfg.freq_radius,
        ));
    }
    // Then along time per bin (centered).
    let mut time_max_col: Vec<f32> = Vec::with_capacity(t_len);
    let mut combined_max: Vec<Vec<f32>> = vec![vec![0.0f32; f_len]; t_len];
    for b in 0..f_len {
        time_max_col.clear();
        for frame_freq_max in &freq_max {
            time_max_col.push(frame_freq_max[b]);
        }
        let tm = crate::sliding_max::sliding_max_centered(&time_max_col, cfg.time_radius);
        for t in 0..t_len {
            combined_max[t][b] = tm[t];
        }
    }

    // Candidates: equal to the 2D window max (separable == true window max)
    // plus adaptive prominence and absolute floor.
    for t in 0..t_len {
        let mut frame_peaks: Vec<(usize, f32)> = Vec::new(); // (bin, mag)
        for b in 0..f_len {
            let m = spectrogram[t][b];
            if m != combined_max[t][b] {
                continue; // not the window maximum
            }
            let db = db_frames[t][b];
            if db < cfg.absolute_floor {
                continue;
            }
            if db - floors[t] < cfg.min_prominence_db {
                continue;
            }
            frame_peaks.push((b, m));
        }
        // Density control: strongest K only (stable order by bin for ties).
        if frame_peaks.len() > cfg.max_peaks_per_frame {
            frame_peaks.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            frame_peaks.truncate(cfg.max_peaks_per_frame);
            frame_peaks.sort_by_key(|p| p.0);
        }
        for (b, _) in frame_peaks {
            peaks.push(Peak {
                time_idx: t,
                freq_bin_idx: b,
            });
        }
    }

    peaks
}

#[cfg(test)]
mod tests {
    use super::*;
    use sivana_audio::rng::XorShift64Star;

    fn random_spec(rng: &mut XorShift64Star, t: usize, f: usize) -> Vec<Vec<f32>> {
        (0..t)
            .map(|_| (0..f).map(|_| rng.next_f32() * 10.0).collect())
            .collect()
    }

    /// Brute-force oracle: legacy-style local maxima with the same
    /// truncating neighborhood (ties broken like legacy: earlier cell wins).
    // Deliberately mirrors the frozen implementation's loop structure so it
    // stays a trustworthy oracle.
    #[allow(clippy::needless_range_loop)]
    fn brute_local_max(spec: &[Vec<f32>], tr: usize, fr: usize) -> Vec<Peak> {
        let mut peaks = Vec::new();
        let (tl, fl) = (spec.len(), spec.first().map_or(0, |f| f.len()));
        for t in 0..tl {
            for b in 0..fl {
                let cur = spec[t][b];
                let mut is_max = true;
                for nt in t.saturating_sub(tr)..((t + tr + 1).min(tl)) {
                    for nb in b.saturating_sub(fr)..((b + fr + 1).min(fl)) {
                        if nt == t && nb == b {
                            continue;
                        }
                        if spec[nt][nb] > cur
                            || (spec[nt][nb] == cur && (nt < t || (nt == t && nb < b)))
                        {
                            is_max = false;
                        }
                    }
                }
                if is_max {
                    peaks.push(Peak {
                        time_idx: t,
                        freq_bin_idx: b,
                    });
                }
            }
        }
        peaks
    }

    #[test]
    fn without_gates_candidates_equal_oracle() {
        // No prominence/floor/cap filters -> same set as the brute oracle.
        let mut rng = XorShift64Star::new(777);
        let spec = random_spec(&mut rng, 12, 40);
        let cfg = PeaksV2Config {
            min_prominence_db: f32::NEG_INFINITY,
            max_peaks_per_frame: usize::MAX,
            ..Default::default()
        };
        let got = find_peaks_v2(&spec, &cfg);
        let want = brute_local_max(&spec, cfg.time_radius, cfg.freq_radius);
        assert_eq!(got, want);
    }

    #[test]
    fn strong_tone_is_found_over_noise() {
        // One loud sinusoid bin rising well above a noisy floor.
        let mut spec = vec![vec![0.05f32; 256]; 20];
        let mut rng = XorShift64Star::new(4);
        for frame in spec.iter_mut() {
            for v in frame.iter_mut() {
                *v *= 0.5 + rng.next_f32();
            }
        }
        spec[10][100] = 50.0; // the event
        let peaks = find_peaks_v2(&spec, &PeaksV2Config::default());
        assert!(peaks.contains(&Peak {
            time_idx: 10,
            freq_bin_idx: 100
        }));
    }

    #[test]
    fn density_cap_limits_peaks_per_frame() {
        let mut rng = XorShift64Star::new(11);
        let spec = random_spec(&mut rng, 6, 300); // many local maxima
        let cfg = PeaksV2Config {
            max_peaks_per_frame: 3,
            ..Default::default()
        };
        let peaks = find_peaks_v2(&spec, &cfg);
        for t in 0..6 {
            let n = peaks.iter().filter(|p| p.time_idx == t).count();
            assert!(n <= 3, "frame {t} had {n} peaks");
        }
    }

    #[test]
    fn empty_input_is_safe() {
        assert!(find_peaks_v2(&[], &PeaksV2Config::default()).is_empty());
        assert!(find_peaks_v2(&[vec![]], &PeaksV2Config::default()).is_empty());
    }
}
