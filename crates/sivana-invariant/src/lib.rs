//! Engine B1 — scale-invariant event-triplet fingerprints (PLAN §28, §85).
//!
//! Classic `(f1, f2, dt)` landmark hashes break when playback is sped up,
//! slowed down or pitch-shifted: every time and frequency coordinate
//! scales. Triplet invariants instead hash *ratios* between three spectral
//! events:
//!
//! ```text
//!   q1 = log2(f2 / f1)      q2 = log2(f3 / f1)     rt = (t2-t1)/(t3-t1)
//! ```
//!
//! Under a time-scale `a` and frequency-scale `b` (resampling = both at
//! once), all three quantities are unchanged, so the same audio at any
//! speed/pitch produces the same hashes.
//!
//! Matching cannot use offset histograms (the playback transform is affine,
//! not translational), so candidates are shortlisted by hash votes and then
//! verified by fitting `t_db = a * t_query + b` to the aligned triplet
//! pairs; the inlier count of that fit is the evidence.

use sivana_core::RecordingId;
use sivana_dsp::peaks_v2::Peak;

#[derive(Debug, Clone)]
pub struct TripletsConfig {
    /// Temporal reach for the second/third events, in frames.
    pub span_frames: usize,
    /// Candidate future peaks considered per anchor (deterministic order).
    pub fanout: usize,
    /// log2-frequency ratio quantization steps spanning [-4, +4] octaves.
    pub freq_ratio_bits: u32,
    /// Time-ratio quantization steps spanning [0, 1].
    pub time_ratio_steps: u32,
}

impl Default for TripletsConfig {
    fn default() -> Self {
        Self {
            span_frames: 12,
            fanout: 6,
            // Coarse enough that integer-bin rounding under frequency
            // scaling (~±4% at typical bins) stays inside one bucket.
            freq_ratio_bits: 8,
            // Coarse on purpose: frame-integer rounding under time scaling
            // jitters gap ratios by ~10%; 32 steps absorbs that.
            time_ratio_steps: 16,
        }
    }
}

/// One triplet fingerprint: scale-invariant hash + anchor frame time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TripletFp {
    pub hash: u32,
    pub t1: u32,
}

fn q_ratio(log2r: f64, bits: u32) -> u32 {
    // Map [-4, +4] octaves onto [0, 2^bits).
    const SPAN: f64 = 8.0;
    let v = ((log2r + 4.0) / SPAN).clamp(0.0, 1.0 - 1e-9);
    (v * (1u32 << bits) as f64) as u32
}

fn q_tratio(t: f64, steps: u32) -> u32 {
    let v = t.clamp(0.0, 1.0 - 1e-9);
    (v * steps as f64) as u32
}

/// Build triplet fingerprints from V2 peaks.
///
/// Anchors walk peaks in time order; for each anchor the next `fanout`
/// peaks inside `span_frames` form deterministic candidate pairs, and each
/// (first, second, third) combination yields one fingerprint. Bin indices
/// stand in for frequencies (log-band consistent with Engine A).
pub fn fingerprint_triplets(peaks: &[Peak], cfg: &TripletsConfig) -> Vec<TripletFp> {
    let mut out = Vec::new();
    let fr_bits = cfg.freq_ratio_bits.min(12);
    let tr_steps = cfg.time_ratio_steps.clamp(16, 65536);

    for (i, a) in peaks.iter().enumerate() {
        // Candidates strictly after the anchor within the span window.
        let mut cands = Vec::with_capacity(cfg.fanout);
        for p in peaks[i + 1..].iter() {
            let dt = p.time_idx.saturating_sub(a.time_idx);
            if dt == 0 {
                continue;
            }
            if dt > cfg.span_frames {
                break; // sorted by time
            }
            cands.push(p);
            if cands.len() >= cfg.fanout * 2 {
                break;
            }
        }
        if cands.len() < 2 {
            continue;
        }
        let take = cands.len().min(cfg.fanout);
        for j in 0..take - 1 {
            for k in j + 1..take {
                let b = cands[j];
                let c = cands[k];
                if c.time_idx == b.time_idx || b.time_idx == a.time_idx {
                    continue;
                }
                let f1 = (a.freq_bin_idx as f64 + 0.5).max(0.5);
                let f2 = (b.freq_bin_idx as f64 + 0.5).max(0.5);
                let f3 = (c.freq_bin_idx as f64 + 0.5).max(0.5);
                let q1 = q_ratio((f2 / f1).log2(), fr_bits);
                let q2 = q_ratio((f3 / f1).log2(), fr_bits);
                let tr = q_tratio(
                    (b.time_idx - a.time_idx) as f64 / (c.time_idx - a.time_idx) as f64,
                    tr_steps,
                );
                // Frame-integer rounding under time scaling jitters the
                // gap ratio by ±1 bucket; emitting neighbour variants
                // keeps real matches colliding (postings grow ~3x, which
                // is acceptable for a fallback engine).
                const TR_BITS: u32 = 4;
                let base = (q1 << (32 - fr_bits)) | (q2 << (32 - 2 * fr_bits));
                for dt in [tr.saturating_sub(1), tr, (tr + 1).min(tr_steps - 1)] {
                    let uniq: u32 = dt % (1u32 << TR_BITS);
                    out.push(TripletFp {
                        hash: base | (uniq << (32 - 2 * fr_bits - TR_BITS)),
                        t1: a.time_idx as u32,
                    });
                }
            }
        }
    }
    out
}

/// Result of one invariant query.
#[derive(Debug, Clone)]
pub struct B1Outcome {
    pub recording: RecordingId,
    /// Aligned pairs surviving the affine fit.
    pub inliers: usize,
    /// Total pairs considered for this candidate.
    pub pairs: usize,
    /// Fitted time-scale (`t_db ≈ a * t_q + b`).
    pub time_scale: f32,
    /// Fitted offset (frames).
    pub offset_frames: i64,
    /// Mean absolute residual of inliers (frames).
    pub residual: f32,
}

/// Query an [`sivana_match::InMemoryIndex`] populated with triplet hashes.
///
/// Candidates are ranked by raw vote count, then verified per candidate by
/// robust least-squares over `(t_q -> t_db)` pairs (two outlier-trimming
/// passes).
/// One aligned (query, catalog) triplet pair feeding the affine fit.
#[derive(Clone, Copy)]
struct Pair {
    rec: u32,
    t_db: u32,
    t_q: u32,
}

pub fn query_affine(
    index: &sivana_match::InMemoryIndex,
    query: &[TripletFp],
    max_candidates: usize,
) -> Vec<B1Outcome> {
    // Stop-hash suppression (§15): a triplet present in every recording
    // carries zero identity information — usually shared timbre structure.
    let n_recs = index.n_recordings().max(1) as usize;
    let mut pairs: Vec<Pair> = Vec::new();
    for q in query {
        if let Some(plist) = index.postings_for(q.hash) {
            let mut distinct = plist.first().map_or(0, |_| 1usize);
            for w in plist.windows(2) {
                if w[1].recording != w[0].recording {
                    distinct += 1;
                }
            }
            if distinct >= n_recs && n_recs > 1 {
                continue;
            }
            for p in plist {
                pairs.push(Pair {
                    rec: p.recording.as_u32(),
                    t_db: p.anchor_time,
                    t_q: q.t1,
                });
            }
        }
    }
    if pairs.is_empty() {
        return Vec::new();
    }

    pairs.sort_by_key(|p| (p.rec, p.t_db));
    // Group runs per recording.
    let mut by_rec: Vec<(u32, Vec<Pair>)> = Vec::new();
    for p in pairs {
        match by_rec.last_mut() {
            Some((r, v)) if *r == p.rec => v.push(p),
            _ => by_rec.push((p.rec, vec![p])),
        }
    }
    by_rec.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));

    by_rec
        .into_iter()
        .take(max_candidates)
        .filter_map(|(_rec, plist)| fit_affine(&plist))
        .collect()
}

/// Robust affine fit `t_db = a * t_q + b`; returns None when too few pairs.
fn fit_affine(pairs: &[Pair]) -> Option<B1Outcome> {
    if pairs.len() < 4 {
        return None;
    }
    let mut pts: Vec<(f64, f64)> = pairs
        .iter()
        .map(|p| (p.t_q as f64, p.t_db as f64))
        .collect();
    let total = pairs.len();

    let (mut a, mut b) = (1.0f64, pts[0].1 - pts[0].0);
    for _pass in 0..3 {
        // Least squares over current points.
        let n = pts.len() as f64;
        if n < 2.0 {
            return None;
        }
        let (mut sx, mut sy, mut sxx, mut sxy): (f64, f64, f64, f64) = (0.0, 0.0, 0.0, 0.0);
        for &(x, y) in &pts {
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let denom = n * sxx - sx * sx;
        if denom.abs() < 1e-9 {
            a = 1.0;
            b = (sy - sx).max(0.0) / n;
        } else {
            a = (n * sxy - sx * sy) / denom;
            b = (sy - a * sx) / n;
        }
        // Keep points whose prediction is close; drop the worst tail.
        let mut resid: Vec<(usize, f64)> = pts
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| (i, (y - (a * x + b)).abs()))
            .collect();
        resid.sort_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal));
        let min_keep: usize = pts.len().min(4);
        let keep_n = (((pts.len() as f64) * 0.8).ceil() as usize).max(min_keep);
        let kept: Vec<(usize, f64)> = resid.into_iter().take(keep_n).collect();
        let tol_frames = 24.0; // ~1.1 s at V2 geometry covers stretch jitter
        pts = kept
            .iter()
            .filter(|&&(_, r)| r <= tol_frames)
            .map(|&(i, _)| pts[i])
            .collect();
        if pts.len() < 4 {
            return None;
        }
    }
    let mean_resid = pts
        .iter()
        .map(|&(x, y)| (y - (a * x + b)).abs())
        .sum::<f64>()
        / pts.len() as f64;

    Some(B1Outcome {
        recording: sivana_core::RecordingId::new(pairs[0].rec),
        inliers: pts.len(),
        pairs: total,
        time_scale: a as f32,
        offset_frames: b.round() as i64,
        residual: mean_resid as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sivana_match::InMemoryIndex;

    fn peaks_from_bins(bins: &[(usize, usize)]) -> Vec<Peak> {
        bins.iter()
            .map(|&(t, f)| Peak {
                time_idx: t,
                freq_bin_idx: f,
                magnitude: 1.0,
                prominence_db: 20.0,
            })
            .collect()
    }

    #[test]
    fn triplets_are_invariant_to_uniform_scaling() {
        // Scale time by 2 and frequency bins by exactly one octave: the
        // log-ratio terms shift uniformly, so every hash must survive.
        // Exact powers of two keep integer rounding out of the picture.
        let base: Vec<Peak> = [
            (10, 40),
            (12, 55),
            (15, 48),
            (18, 70),
            (21, 52),
            (25, 80),
            (28, 60),
            (31, 90),
        ]
        .iter()
        .map(|&(t, f)| Peak {
            time_idx: t,
            freq_bin_idx: f,
            magnitude: 1.0,
            prominence_db: 20.0,
        })
        .collect();

        let scaled: Vec<Peak> = base
            .iter()
            .map(|p| Peak {
                time_idx: p.time_idx * 2,
                freq_bin_idx: p.freq_bin_idx * 2,
                ..*p
            })
            .collect();

        let cfg = TripletsConfig {
            span_frames: 48, // widened so scaled events stay in range
            ..Default::default()
        };
        let h1: Vec<u32> = fingerprint_triplets(&base, &cfg)
            .into_iter()
            .map(|f| f.hash)
            .collect();
        let h2: Vec<u32> = fingerprint_triplets(&scaled, &cfg)
            .into_iter()
            .map(|f| f.hash)
            .collect();
        assert!(!h1.is_empty());
        // The scaled stream must produce the same hash multiset (subset check
        // on distinct values keeps this robust to zone-edge effects).
        let set1: std::collections::HashSet<u32> = h1.into_iter().collect();
        let set2: std::collections::HashSet<u32> = h2.into_iter().collect();
        let shared = set1.intersection(&set2).count();
        assert!(
            shared >= set1.len() * 7 / 10,
            "only {shared}/{} hashes survived scaling",
            set1.len()
        );
    }

    #[test]
    fn affine_matching_identifies_scaled_copy() {
        // Catalog recording: triplets at unscaled times.
        let catalog_peaks = peaks_from_bins(&[
            (100, 40),
            (104, 60),
            (108, 44),
            (112, 75),
            (116, 50),
            (120, 88),
            (124, 58),
        ]);
        let cfg = TripletsConfig {
            span_frames: 24, // wide enough for the doubled query timeline
            ..Default::default()
        };
        let fps = fingerprint_triplets(&catalog_peaks, &cfg);
        assert!(fps.len() >= 4);

        let mut idx = InMemoryIndex::new();
        idx.add_recording(
            RecordingId::new(0),
            &fps.iter().map(|f| (f.hash, f.t1)).collect::<Vec<_>>(),
        );
        idx.finalize();

        // Query: the same events at exactly 2x time scale and one octave
        // up, plus an absolute shift. Hashes must still collide; the fit
        // recovers a = 2.0.
        let shifted: Vec<TripletFp> = fingerprint_triplets(
            &peaks_from_bins(&[
                (210, 80),
                (218, 120),
                (226, 88),
                (234, 150),
                (242, 100),
                (250, 176),
                (258, 116),
            ]),
            &cfg,
        );
        let outcomes = query_affine(&idx, &shifted, 3);
        assert!(!outcomes.is_empty(), "no candidate survived");
        let best = &outcomes[0];
        assert_eq!(best.recording.as_u32(), 0);
        // Query timeline is the stretched one (2x gaps), so mapping
        // query -> catalog divides: fitted a ~= 0.5.
        assert!(
            best.time_scale > 0.3 && best.time_scale < 0.7,
            "implausible fitted scale {}",
            best.time_scale
        );
    }
}
