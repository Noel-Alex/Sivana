//! Peak detection V2: linear-time local maxima with adaptive acceptance
//! (§9), in streaming form with constant memory (§4.1).
//!
//! Replaces the frozen prototype's brute-force 2D scan:
//!
//! * local-max test via separable centered sliding max — `O(T*F)` along
//!   frequency, `O(T*F*r_t)` along time (`r_t` = time radius, a small
//!   constant; the online form scans the ≤2r+1 ring rows directly)
//! * absolute magnitude floor replaced by per-frame prominence over a
//!   robust noise-floor estimate (median dB of the frame, §9.2)
//! * density controlled by keeping the strongest `max_peaks_per_frame`
//!   survivors (§9.3)
//!
//! [`PeakStreamer`] decides frame `t` once frame `t + time_radius` has
//! arrived, holding only ~2r+1 rows regardless of source duration. The
//! batch [`find_peaks_v2`] is a thin wrapper over the streamer, so both
//! paths produce identical output by construction. End-of-stream frames
//! use truncated future windows, matching the batch edge semantics.
//!
//! Legacy equivalence mode: set `min_prominence_db = f32::NEG_INFINITY`
//! and an absolute floor to reproduce the old filter (modulo the old
//! strict tie-break on plateaus), which the property tests exploit as an
//! oracle.

/// A spectral event: one (frame, bin) cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub time_idx: usize,
    pub freq_bin_idx: usize,
    /// Linear magnitude at the cell.
    pub magnitude: f32,
    /// Cell level above its frame's median-noise floor, in dB. Uniform
    /// gain shifts cell and floor equally, so this is gain-invariant —
    /// usable as a scale-free strength feature (§11, E2a follow-up).
    pub prominence_db: f32,
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
    /// Temporal background suppression (E12, minimum-statistics form): a
    /// candidate must exceed its OWN BIN's estimated noise floor — the
    /// MINIMUM level that bin touched over a trailing window — by
    /// [`Self::background_margin_db`] to be a peak. Minimum statistics is
    /// the right tracker here because music VARIES (every vibrato, decay,
    /// or note change dips far below its own running mean, constantly
    /// refreshing the floor), so sustained tones survive; only truly
    /// stationary content (fan hum, mains buzz, mic hiss) never dips, so
    /// only it stays pinned at the floor and is rejected. This is what an
    /// EMA-based background got wrong: rise-tau fast enough to track
    /// capture start also absorbed any note longer than the tau, emitting
    /// ZERO fingerprints from real music windows (measured). Window length
    /// in FRAMES; 0 disables.
    pub background_min_window_frames: usize,
    /// Required margin over the per-bin minimum-statistics floor (dB).
    pub background_margin_db: f32,
}

impl Default for PeaksV2Config {
    fn default() -> Self {
        // First-cut V2 defaults; every value here must eventually be
        // justified by a benchmark sweep (PLAN.md §92). Whitening is OFF at
        // this layer: the pure-detector semantics (oracle property tests,
        // benchmarks) stay untouched. Production enables it via
        // LandmarkV2Config::default().
        Self {
            time_radius: 2,
            freq_radius: 5,
            min_prominence_db: 8.0,
            absolute_floor: f32::NEG_INFINITY,
            max_peaks_per_frame: 8,
            background_min_window_frames: 0,
            background_margin_db: 0.0,
        }
    }
}

/// dB level of one linear magnitude (clamped at -200 dB to stay finite).
fn to_db(m: f32) -> f32 {
    20.0 * m.max(1e-10).log10()
}

/// Median of a buffer via selection (O(n) average). Copies into scratch so
/// the caller's ordering is untouched; NaNs sort as equal (magnitudes are
/// non-negative in practice).
fn median_of(buf: &[f32], scratch: &mut Vec<f32>) -> f32 {
    scratch.clear();
    scratch.extend_from_slice(buf);
    let mid = scratch.len() / 2;
    let (_, val, _) = scratch.select_nth_unstable_by(mid, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
    *val
}

/// Streaming peak detector over STFT magnitude frames (§9).
///
/// Feed one frame at a time; peaks for frame `t` become available once
/// `t + cfg.time_radius` has been fed (the centered time-window lookahead),
/// or at [`Self::finish`] with truncated windows. Memory is O(F · r)
/// independent of stream length.
pub struct PeakStreamer {
    cfg: PeaksV2Config,
    f_len: usize,
    /// Absolute index of the front ring row.
    oldest: u64,
    /// Absolute index of the next frame to decide.
    next_emit: u64,
    /// Total frames fed.
    fed: u64,
    fmap_ring: std::collections::VecDeque<Vec<f32>>,
    mag_ring: std::collections::VecDeque<Vec<f32>>,
    floor_ring: std::collections::VecDeque<f32>,
    /// Raw per-bin levels in dB over the trailing whitening window (E12),
    /// from which the minimum-statistics floor is computed. Length is
    /// bounded by `background_min_window_frames`; empty when disabled.
    db_history: Option<std::collections::VecDeque<Vec<f32>>>,
    /// Per-frame snapshots of the computed floor, index-aligned with
    /// fmap/mag/floor_ring: a frame decided `time_radius` frames later
    /// reads exactly the floor its own instant produced — streaming ==
    /// batch by construction.
    bg_snapshots: Option<std::collections::VecDeque<Vec<f32>>>,
    // Scratch (reused, never retained between calls).
    fmap_scratch: Vec<f32>,
    sel_scratch: Vec<f32>,
    bg_scratch: Vec<f32>,
    cand_scratch: Vec<(usize, f32, f32)>, // (bin, magnitude, prominence_db)
    max_scratch: crate::sliding_max::SlidingMaxScratch,
}

/// Whitening enabled = window > 0. One place for the invariant.
fn cfg_whitened(cfg: &PeaksV2Config) -> bool {
    cfg.background_min_window_frames > 0
}

impl PeakStreamer {
    /// Create a streamer for spectra of `f_len` bins (window/2+1). Pass 0
    /// to size lazily from the first frame.
    pub fn new(f_len: usize, cfg: PeaksV2Config) -> Self {
        let whitened = cfg_whitened(&cfg);
        Self {
            cfg,
            f_len,
            oldest: 0,
            next_emit: 0,
            fed: 0,
            fmap_ring: std::collections::VecDeque::new(),
            mag_ring: std::collections::VecDeque::new(),
            floor_ring: std::collections::VecDeque::new(),
            db_history: if whitened {
                Some(std::collections::VecDeque::new())
            } else {
                None
            },
            bg_snapshots: if whitened {
                Some(std::collections::VecDeque::new())
            } else {
                None
            },
            fmap_scratch: Vec::new(),
            sel_scratch: Vec::new(),
            bg_scratch: Vec::new(),
            cand_scratch: Vec::new(),
            max_scratch: crate::sliding_max::SlidingMaxScratch::new(),
        }
    }

    /// Number of frames fed but not yet decided.
    pub fn pending(&self) -> u64 {
        self.fed - self.next_emit
    }

    /// Feed one magnitude spectrum; append peaks for every frame that
    /// became decidable (usually zero or one). `out` is cleared first.
    ///
    /// Panics if frame lengths disagree with earlier calls.
    pub fn process_frame(&mut self, mags: &[f32], out: &mut Vec<Peak>) {
        out.clear();
        if self.f_len == 0 {
            self.f_len = mags.len();
        }
        assert_eq!(mags.len(), self.f_len, "frame length changed mid-stream");
        if mags.is_empty() {
            return;
        }

        // Noise floor: the median of the frame in dB. log is monotone, so
        // taking the median of linear magnitudes and logging that single
        // value is bit-identical to converting the whole frame first — at
        // one log call instead of F.
        let floor = to_db(median_of(mags, &mut self.sel_scratch));
        self.max_scratch
            .centered_into(mags, self.cfg.freq_radius, &mut self.fmap_scratch);

        self.fmap_ring.push_back(self.fmap_scratch.clone());
        self.mag_ring.push_back(mags.to_vec());
        self.floor_ring.push_back(floor);

        // Minimum-statistics floor (E12): per-bin min over the trailing
        // window, recomputed from raw history each frame. Cost is
        // O(window * bins) per frame — a few million cheap comparisons
        // per second at production geometry, negligible even in WASM.
        // Correctness beats cleverness here: every incremental-min scheme
        // either never lets the floor rise again or needs per-bin deques.
        if let (Some(hist), Some(snaps)) = (&mut self.db_history, &mut self.bg_snapshots) {
            hist.push_back(mags.iter().map(|&m| to_db(m)).collect::<Vec<f32>>());
            let w = self.cfg.background_min_window_frames;
            while hist.len() > w {
                hist.pop_front();
            }
            let mut min_row = std::mem::take(&mut self.bg_scratch);
            min_row.clear();
            min_row.extend_from_slice(hist.front().expect("just pushed"));
            for row in hist.iter().skip(1) {
                for (a, &v) in min_row.iter_mut().zip(row.iter()) {
                    if v < *a {
                        *a = v;
                    }
                }
            }
            snaps.push_back(std::mem::replace(
                &mut min_row,
                std::mem::take(&mut self.bg_scratch),
            ));
        }

        self.fed += 1;

        // Decide everything whose time-radius lookahead has arrived.
        while (self.next_emit + self.cfg.time_radius as u64) < self.fed {
            self.decide(out);
        }
    }

    /// Decide all remaining buffered frames with truncated future windows
    /// (end-of-stream). Call once after the last [`Self::process_frame`].
    pub fn finish(&mut self, out: &mut Vec<Peak>) {
        out.clear();
        while self.next_emit < self.fed {
            self.decide(out);
        }
    }

    /// Decide frame `next_emit` and advance. The vertical (time-axis) max
    /// spans rows `[t-r, t+r]` clipped to what exists — exactly the batch
    /// detector's truncated centered window.
    #[allow(clippy::needless_range_loop)]
    fn decide(&mut self, out: &mut Vec<Peak>) {
        debug_assert!(self.next_emit < self.fed);
        let t = self.next_emit;
        let base = (t - self.oldest) as usize;
        let filled = self.fmap_ring.len();
        let r = self.cfg.time_radius;
        let lo = base.saturating_sub(r);
        let hi = (base + r).min(filled - 1);
        let floor = self.floor_ring[base];

        self.cand_scratch.clear();
        for b in 0..self.f_len {
            let m = self.mag_ring[base][b];
            // Cheap pre-filter: a 2D window maximum must equal its own
            // frequency-row max first. This skips the 2r+1-row vertical
            // scan for ~90% of cells at default radii.
            if m != self.fmap_ring[base][b] {
                continue;
            }
            let mut vmax = f32::NEG_INFINITY;
            for slot in lo..=hi {
                let v = self.fmap_ring[slot][b];
                if v > vmax {
                    vmax = v;
                }
            }
            if m != vmax {
                continue; // not the window maximum
            }
            // dB is only computed for cells that passed the local-max test.
            let db = to_db(m);
            if db < self.cfg.absolute_floor {
                continue;
            }
            // Minimum-statistics background test (E12): the cell must rise
            // above its OWN BIN's floor-of-the-trailing-window. Stationary
            // content never dips, so it sits AT the floor and is rejected;
            // music constantly varies below any of its peaks, refreshing
            // the floor so sustained tones survive.
            if let Some(snaps) = &self.bg_snapshots {
                let bg = snaps[base][b];
                if db - bg < self.cfg.background_margin_db {
                    continue;
                }
            }
            let prominence = db - floor;
            if prominence < self.cfg.min_prominence_db {
                continue;
            }
            self.cand_scratch.push((b, m, prominence));
        }

        // Density control: strongest K only (stable order by bin for ties).
        if self.cand_scratch.len() > self.cfg.max_peaks_per_frame {
            self.cand_scratch.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            self.cand_scratch.truncate(self.cfg.max_peaks_per_frame);
            self.cand_scratch.sort_by_key(|c| c.0);
        }
        for &(b, m, prom) in &self.cand_scratch {
            out.push(Peak {
                time_idx: t as usize,
                freq_bin_idx: b,
                magnitude: m,
                prominence_db: prom,
            });
        }

        self.next_emit += 1;
        // Rows below next_emit - r can no longer affect any decision.
        let keep_from = self.next_emit.saturating_sub(r as u64);
        while self.oldest < keep_from {
            self.fmap_ring.pop_front();
            self.mag_ring.pop_front();
            self.floor_ring.pop_front();
            if let Some(snaps) = &mut self.bg_snapshots {
                snaps.pop_front();
            }
            self.oldest += 1;
        }
    }
}

/// Detect peaks over a whole spectrogram (batch convenience wrapper).
///
/// `spectrogram[t]` is the magnitude spectrum of frame t. Identical results
/// come from feeding the same frames through a [`PeakStreamer`].
pub fn find_peaks_v2(spectrogram: &[Vec<f32>], cfg: &PeaksV2Config) -> Vec<Peak> {
    let f_len = spectrogram.first().map_or(0, |f| f.len());
    let mut streamer = PeakStreamer::new(f_len, cfg.clone());
    let mut peaks = Vec::new();
    let mut chunk = Vec::new();
    for frame in spectrogram {
        streamer.process_frame(frame, &mut chunk);
        peaks.append(&mut chunk);
    }
    streamer.finish(&mut chunk);
    peaks.append(&mut chunk);
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
    // stays a trustworthy oracle. Prominence is recomputed independently
    // from the raw spectrogram (per-frame median dB).
    #[allow(clippy::needless_range_loop)]
    fn brute_local_max(spec: &[Vec<f32>], tr: usize, fr: usize) -> Vec<Peak> {
        let mut peaks = Vec::new();
        let (tl, fl) = (spec.len(), spec.first().map_or(0, |f| f.len()));
        for t in 0..tl {
            let mut dbs: Vec<f32> = spec[t]
                .iter()
                .map(|&m| 20.0 * m.max(1e-10).log10())
                .collect();
            let mid = dbs.len() / 2;
            dbs.select_nth_unstable_by(mid, |a, b| {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            });
            let floor = dbs[dbs.len() / 2];
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
                        magnitude: cur,
                        prominence_db: 20.0 * cur.max(1e-10).log10() - floor,
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
        let event = peaks
            .iter()
            .find(|p| p.time_idx == 10 && p.freq_bin_idx == 100)
            .expect("event peak missing");
        assert_eq!(event.magnitude, 50.0);
        // Prominence: well above the gate margin, and gain-invariant.
        assert!(event.prominence_db > 8.0);
        let scaled = spec
            .iter()
            .map(|f| f.iter().map(|v| v * 1000.0).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let peaks_scaled = find_peaks_v2(&scaled, &PeaksV2Config::default());
        let event_scaled = peaks_scaled
            .iter()
            .find(|p| p.time_idx == 10 && p.freq_bin_idx == 100)
            .expect("scaled event peak missing");
        assert!((event.prominence_db - event_scaled.prominence_db).abs() < 0.01);
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

    #[test]
    fn streaming_matches_batch_exactly() {
        let mut rng = XorShift64Star::new(4242);
        let spec = random_spec(&mut rng, 40, 97);
        let cfg = PeaksV2Config::default();
        let batch = find_peaks_v2(&spec, &cfg);

        // One frame at a time through the streamer.
        let mut ps = PeakStreamer::new(97, cfg.clone());
        let mut got = Vec::new();
        let mut chunk = Vec::new();
        for frame in &spec {
            ps.process_frame(frame, &mut chunk);
            got.append(&mut chunk);
        }
        ps.finish(&mut chunk);
        got.append(&mut chunk);
        assert_eq!(got, batch, "streamer must equal batch");
    }

    #[test]
    fn emission_waits_for_lookahead_then_flows_in_order() {
        // With r=2, frames are held until the lookahead arrives: nothing is
        // decided before frame r+1 exists, then frames flow out in strict
        // time order as each new frame lands.
        let mut rng = XorShift64Star::new(909090);
        let spec = random_spec(&mut rng, 10, 65);
        let cfg = PeaksV2Config::default();
        assert_eq!(cfg.time_radius, 2);

        let mut ps = PeakStreamer::new(65, cfg.clone());
        let mut chunk = Vec::new();
        let mut last_time: i64 = -1;

        for (i, frame) in spec.iter().enumerate() {
            ps.process_frame(frame, &mut chunk);
            assert_eq!(
                ps.pending(),
                ((i + 1) as u64).min(cfg.time_radius as u64),
                "pending count after feeding frame {i}"
            );
            for p in chunk.drain(..) {
                assert!((p.time_idx as i64) > last_time, "out-of-order emission");
                last_time = p.time_idx as i64;
            }
        }
        ps.finish(&mut chunk);
        assert_eq!(ps.pending(), 0, "finish must drain every buffered frame");
        for p in chunk.drain(..) {
            assert!((p.time_idx as i64) > last_time, "out-of-order flush");
            last_time = p.time_idx as i64;
        }
    }

    #[test]
    fn short_stream_flushes_with_truncated_windows() {
        // A single loud cell in a tiny spectrogram must survive entirely
        // via finish()'s end-of-stream flush.
        let spec = vec![vec![0.1f32; 16], vec![0.1f32; 16]];
        let mut spec = spec;
        spec[1][9] = 20.0;
        let mut ps = PeakStreamer::new(16, PeaksV2Config::default());
        let mut got = Vec::new();
        let mut chunk = Vec::new();
        for frame in &spec {
            ps.process_frame(frame, &mut chunk);
            got.append(&mut chunk);
        }
        ps.finish(&mut chunk);
        got.append(&mut chunk);
        assert_eq!(got, find_peaks_v2(&spec, &PeaksV2Config::default()));
        assert!(got.iter().any(|p| p.time_idx == 1 && p.freq_bin_idx == 9));
    }

    #[test]
    fn memory_stays_bounded_on_long_streams() {
        // The ring must hold ~2r+1 rows no matter how long the stream runs.
        let mut rng = XorShift64Star::new(31337);
        let cfg = PeaksV2Config::default();
        let mut ps = PeakStreamer::new(33, cfg.clone());
        let mut chunk = Vec::new();
        for _ in 0..2000 {
            let frame: Vec<f32> = (0..33).map(|_| rng.next_f32()).collect();
            ps.process_frame(&frame, &mut chunk);
        }
        ps.finish(&mut chunk);
        let bound = 2 * cfg.time_radius + 2;
        assert!(
            ps.fmap_ring.len() <= bound,
            "ring grew to {} rows",
            ps.fmap_ring.len()
        );
    }
}
