//! Benchmark runner: drives the frozen legacy engine over a degraded
//! query matrix and records per-case results with timings.
//!
//! This is the Phase 0 control harness — the same grid will later run
//! against Landmark V2 and the invariant engines for A/B comparison
//! (research/PLAN.md §78 exit criteria).

use crate::corpus::{self, Corpus};
use crate::degradations::Degradation;
use serde::Serialize;
use sivana_audio::fixtures;
use sivana_audio::rng::XorShift64Star;
use sivana_core::config::AlgorithmConfig;
use std::path::Path;
use std::time::Instant;

pub struct GridConfig {
    pub excerpt_seconds: f32,
    pub positions_per_track: usize,
    pub degradations: Vec<Degradation>,
    /// The legacy implementation's hard gate; recorded separately from
    /// raw rank-1 so we can measure both.
    pub legacy_min_score: usize,
    pub verbose: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseResult {
    pub case_id: usize,
    pub degradation: String,
    pub expected_track: String,
    pub matched_track: Option<String>,
    pub score: Option<usize>,
    /// Rank-1 song identity correct (raw argmax, no gate).
    pub track_hit: bool,
    /// Song correct **and** offset within tolerance.
    pub offset_hit: bool,
    /// Song correct and score >= legacy gate.
    pub gated_hit: bool,
    pub offset_frames_expected: i64,
    pub offset_frames_matched: Option<i64>,
    pub fingerprint_us: u128,
    pub match_us: u128,
    // Calibration features (§26); populated by engines that expose raw
    // matcher evidence, None for the legacy score-only engine.
    pub score_weight: Option<f32>,
    pub offset_concentration: Option<f32>,
    pub margin_over_next: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectionCase {
    pub degradation: String,
    pub accepted_by_gate: bool,
    pub best_score: Option<usize>,
    pub best_inliers: Option<usize>,
    pub best_concentration: Option<f32>,
    pub best_margin: Option<f32>,
}

#[derive(Serialize)]
pub struct RunSummary {
    pub engine: String,
    pub fingerprint_version: String,
    pub seed: u64,
    pub sample_rate_hz: u32,
    pub track_seconds: f32,
    pub n_tracks: usize,
    pub excerpt_seconds: f32,
    pub config: AlgorithmConfig,
    /// Engine-specific configuration actually used (V2 landmark config).
    /// The legacy `config` field predates per-engine configs and stays for
    /// compatibility; E3 adds this so JSON headers stop misreporting V2
    /// runs with legacy parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landmark_config: Option<serde_json::Value>,
    pub cases: Vec<CaseResult>,
    pub rejection_cases: Vec<RejectionCase>,
}

impl RunSummary {
    pub fn aggregate(&self) -> Aggregates {
        Aggregates {
            total_cases: self.cases.len(),
            recall_track: ratio(&self.cases, |c| c.track_hit),
            recall_offset: ratio(&self.cases, |c| c.offset_hit),
            recall_gated: ratio(&self.cases, |c| c.gated_hit),
            mean_fingerprint_ms: mean(self.cases.iter().map(|c| c.fingerprint_us)) / 1000.0,
            mean_match_ms: mean(self.cases.iter().map(|c| c.match_us)) / 1000.0,
            p95_total_ms: p95(self
                .cases
                .iter()
                .map(|c| (c.fingerprint_us + c.match_us) as f64))
                / 1000.0,
            false_accepts: self
                .rejection_cases
                .iter()
                .filter(|r| r.accepted_by_gate)
                .count(),
            rejection_cases: self.rejection_cases.len(),
        }
    }
}

#[derive(Serialize)]
pub struct Aggregates {
    pub total_cases: usize,
    pub recall_track: f64,
    pub recall_offset: f64,
    pub recall_gated: f64,
    pub mean_fingerprint_ms: f64,
    pub mean_match_ms: f64,
    pub p95_total_ms: f64,
    pub false_accepts: usize,
    pub rejection_cases: usize,
}

fn ratio(cases: &[CaseResult], f: impl Fn(&CaseResult) -> bool) -> f64 {
    if cases.is_empty() {
        0.0
    } else {
        cases.iter().filter(|c| f(c)).count() as f64 / cases.len() as f64
    }
}

fn mean(us: impl Iterator<Item = u128>) -> f64 {
    let (sum, n) = us.fold((0u128, 0u128), |(s, n), v| (s + v, n + 1));
    if n == 0 { 0.0 } else { sum as f64 / n as f64 }
}

fn p95(values: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[((v.len() as f64 * 0.95).ceil() as usize).min(v.len()) - 1]
}

/// Fingerprint mono samples with the frozen legacy pipeline.
fn legacy_fingerprints(
    samples: &[f32],
    sample_rate: u32,
    cfg: &AlgorithmConfig,
) -> Vec<sivana_legacy::hashing::Fingerprint> {
    let spec = sivana_legacy::spectrogram::create_spectrogram(
        samples,
        sample_rate,
        cfg.fft.window_size,
        cfg.fft.hop_size,
    );
    let peaks = sivana_legacy::peaks::find_peaks(
        &spec,
        cfg.peaks.neighborhood_time_radius,
        cfg.peaks.neighborhood_freq_radius,
        cfg.peaks.min_magnitude_threshold,
    );
    sivana_legacy::hashing::create_hashes(
        &peaks,
        cfg.landmarks.dt_min_frames,
        cfg.landmarks.dt_max_frames,
        cfg.landmarks.df_abs_max_bins,
        cfg.landmarks.fanout,
    )
}

/// Run the full baseline benchmark against the legacy engine.
pub fn run_baseline(
    corpus: &Corpus,
    grid: &GridConfig,
    db_path: &Path,
) -> Result<RunSummary, String> {
    sivana_legacy::set_verbose(grid.verbose);
    let cfg = AlgorithmConfig::legacy();

    // Fresh database per run.
    for suffix in ["", "-wal", "-shm"] {
        let mut p = db_path.as_os_str().to_owned();
        p.push(suffix);
        std::fs::remove_file(&p).ok();
    }
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let mut conn =
        sivana_legacy::database::open_db_connection_at(db_path).map_err(|e| e.to_string())?;
    sivana_legacy::database::init_db(&conn).map_err(|e| e.to_string())?;

    // --- Enroll every catalog track ---
    let mut db_to_recording: Vec<(u32, sivana_core::RecordingId)> = Vec::new();
    for t in &corpus.tracks {
        let db_id = sivana_legacy::database::enroll_song(
            &mut conn,
            &t.name,
            None,
            &t.samples,
            corpus.sample_rate,
            cfg.fft.window_size,
            cfg.fft.hop_size,
            (
                cfg.peaks.neighborhood_time_radius,
                cfg.peaks.neighborhood_freq_radius,
                cfg.peaks.min_magnitude_threshold,
            ),
            (
                cfg.landmarks.dt_min_frames,
                cfg.landmarks.dt_max_frames,
                cfg.landmarks.df_abs_max_bins,
                cfg.landmarks.fanout,
            ),
        )
        .map_err(|e| format!("enroll {}: {e}", t.name))?;
        db_to_recording.push((db_id, t.recording));
    }

    // --- Query matrix ---
    let frames_per_sec_f32 = cfg.frames_per_second() as f32;
    let mut cases = Vec::new();
    let mut case_id = 0usize;

    for (ti, track) in corpus.tracks.iter().enumerate() {
        for pos_i in 0..grid.positions_per_track {
            // Deterministic excerpt position inside the track.
            let mut rng =
                XorShift64Star::new(corpus.seed ^ 0x51E5).derive((ti * 977 + pos_i) as u64);
            let max_start = (corpus.duration_s - grid.excerpt_seconds - 0.1).max(0.0);
            let start_s = rng.next_f32() * max_start;
            let clean = fixtures::excerpt(
                &track.samples,
                corpus.sample_rate,
                start_s,
                grid.excerpt_seconds,
            );
            let expected_offset = (start_s * frames_per_sec_f32) as i64;

            for deg in &grid.degradations {
                let query = deg.apply(&clean, corpus.sample_rate, case_id as u64);

                let t0 = Instant::now();
                let qfps = legacy_fingerprints(&query, corpus.sample_rate, &cfg);
                let fingerprint_us = t0.elapsed().as_micros();

                let t1 = Instant::now();
                let m = sivana_legacy::database::query_db_and_match_with_threshold(&conn, &qfps, 1);
                let match_us = t1.elapsed().as_micros();

                let matched_name = m.as_ref().and_then(|m| {
                    db_to_recording
                        .iter()
                        .find(|(db, _)| *db == m.song_id)
                        .and_then(|(_, rec)| {
                            corpus
                                .tracks
                                .iter()
                                .find(|t| t.recording == *rec)
                                .map(|t| t.name.clone())
                        })
                });
                let track_hit = matches!(&matched_name, Some(n) if *n == track.name);
                let offset_matched = m.as_ref().map(|m| m.time_offset_in_song_frames as i64);
                let offset_ok = track_hit
                    && offset_matched
                        .map(|o| (o - expected_offset).abs() <= 2)
                        .unwrap_or(false);
                let gated = track_hit
                    && m.as_ref()
                        .map(|m| m.score >= grid.legacy_min_score)
                        .unwrap_or(false);

                cases.push(CaseResult {
                    case_id,
                    degradation: deg.id(),
                    expected_track: track.name.clone(),
                    matched_track: matched_name,
                    score: m.as_ref().map(|m| m.score),
                    track_hit,
                    offset_hit: offset_ok,
                    gated_hit: gated,
                    offset_frames_expected: expected_offset,
                    offset_frames_matched: offset_matched,
                    fingerprint_us,
                    match_us,
                    score_weight: None,
                    offset_concentration: None,
                    margin_over_next: None,
                });
                case_id += 1;
            }
        }
    }

    // --- Out-of-catalog rejection ---
    let held_seed = corpus.seed ^ 0xDEAD_BEEF;
    let held_samples =
        fixtures::synth_song(held_seed, grid.excerpt_seconds + 2.0, corpus.sample_rate);
    let mut rejection_cases = Vec::new();
    for deg in &grid.degradations {
        let query = deg.apply(&held_samples, corpus.sample_rate, 0xC0FFEE);
        let qfps = legacy_fingerprints(&query, corpus.sample_rate, &cfg);
        let m = sivana_legacy::database::query_db_and_match_with_threshold(
            &conn,
            &qfps,
            grid.legacy_min_score,
        );
        rejection_cases.push(RejectionCase {
            degradation: deg.id(),
            accepted_by_gate: m.is_some(),
            best_score: m.as_ref().map(|m| m.score),
            best_inliers: None,
            best_concentration: None,
            best_margin: None,
        });
    }

    Ok(RunSummary {
        engine: "legacy".into(),
        fingerprint_version: format!(
            "{:?}",
            sivana_core::current_fingerprint_version(sivana_core::EngineId::Legacy)
        ),
        seed: corpus.seed,
        sample_rate_hz: corpus.sample_rate,
        track_seconds: corpus.duration_s,
        n_tracks: corpus.tracks.len(),
        excerpt_seconds: grid.excerpt_seconds,
        config: cfg,
        landmark_config: None,
        cases,
        rejection_cases,
    })
}

/// Default degradation grid used when no CLI overrides are given.
pub fn default_grid() -> GridConfig {
    GridConfig {
        excerpt_seconds: 8.0,
        positions_per_track: 2,
        degradations: vec![
            Degradation::None,
            Degradation::WhiteNoise { snr_db: 20.0 },
            Degradation::WhiteNoise { snr_db: 10.0 },
            Degradation::PinkNoise { snr_db: 10.0 },
            Degradation::Speed { factor: 0.90 },
            Degradation::Speed { factor: 1.05 },
            Degradation::LowPass { cutoff_hz: 3000.0 },
            Degradation::HighPass { cutoff_hz: 150.0 },
            Degradation::Clip { threshold: 0.30 },
            Degradation::Echo {
                delay_s: 0.15,
                gain: 0.40,
            },
        ],
        legacy_min_score: 100,
        verbose: false,
    }
}

pub use corpus::generate as generate_corpus;

/// Landmark-V2 engine: streaming DSP + PeaksV2 fingerprints against the
/// flat rarity-weighted matcher. Same grid, same case semantics as the
/// legacy runner so results are directly comparable. `freq_bands` selects
/// the log-band quantization (E3 sweep axis); `params` carries matcher
/// configuration (offset tolerance etc.) so calibration sweeps can vary it.
pub fn run_landmark_v2(
    corpus: &Corpus,
    grid: &GridConfig,
    freq_bands: u16,
    params: sivana_match::MatchParams,
) -> Result<RunSummary, String> {
    let cfg = AlgorithmConfig::legacy();
    let lm_cfg = sivana_landmark::LandmarkV2Config {
        fft_window: cfg.fft.window_size,
        hop: cfg.fft.hop_size,
        freq_bands,
        ..Default::default()
    };
    // Honest engine metadata: record the parameters actually in effect.
    let landmark_config = serde_json::json!({
        "fft_window": lm_cfg.fft_window,
        "hop": lm_cfg.hop,
        "fanout": lm_cfg.fanout,
        "dt_min_frames": lm_cfg.dt_min,
        "dt_max_frames": lm_cfg.dt_max,
        "freq_bands": lm_cfg.freq_bands,
        "peaks": {
            "time_radius": lm_cfg.peaks.time_radius,
            "freq_radius": lm_cfg.peaks.freq_radius,
            "min_prominence_db": lm_cfg.peaks.min_prominence_db,
            "absolute_floor": lm_cfg.peaks.absolute_floor,
            "max_peaks_per_frame": lm_cfg.peaks.max_peaks_per_frame,
        },
    });

    // --- Build reference index ---
    let mut index = sivana_match::InMemoryIndex::new();
    for t in &corpus.tracks {
        let fps = sivana_landmark::fingerprint(&t.samples, corpus.sample_rate, &lm_cfg);
        index.add_recording(
            t.recording,
            &fps.iter()
                .map(|f| (f.hash, f.anchor_time))
                .collect::<Vec<_>>(),
        );
    }
    index.finalize();

    let frames_per_sec_f32 = cfg.frames_per_second() as f32;
    let mut cases = Vec::new();
    let mut case_id = 0usize;

    for (ti, track) in corpus.tracks.iter().enumerate() {
        for pos_i in 0..grid.positions_per_track {
            let mut rng =
                XorShift64Star::new(corpus.seed ^ 0x51E5).derive((ti * 977 + pos_i) as u64);
            let max_start = (corpus.duration_s - grid.excerpt_seconds - 0.1).max(0.0);
            let start_s = rng.next_f32() * max_start;
            let clean = fixtures::excerpt(
                &track.samples,
                corpus.sample_rate,
                start_s,
                grid.excerpt_seconds,
            );
            let expected_offset = (start_s * frames_per_sec_f32) as i64;

            for deg in &grid.degradations {
                let query = deg.apply(&clean, corpus.sample_rate, case_id as u64);

                let t0 = Instant::now();
                let qfps_raw = sivana_landmark::fingerprint(&query, corpus.sample_rate, &lm_cfg);
                let qfps: Vec<sivana_match::QueryFp> = qfps_raw
                    .iter()
                    .map(|f| sivana_match::QueryFp {
                        hash: f.hash,
                        anchor_time: f.anchor_time,
                    })
                    .collect();
                let fingerprint_us = t0.elapsed().as_micros();

                let t1 = Instant::now();
                let outcomes = index.query(&qfps, &params);
                let match_us = t1.elapsed().as_micros();

                let best = outcomes.first();
                let matched_name = best.and_then(|o| {
                    corpus
                        .tracks
                        .iter()
                        .find(|t| t.recording == o.recording)
                        .map(|t| t.name.clone())
                });
                let track_hit = matches!(&matched_name, Some(n) if *n == track.name);
                let offset_matched = best.map(|o| o.offset_frames);
                let offset_ok = track_hit
                    && offset_matched
                        .map(|o| (o - expected_offset).abs() <= 2)
                        .unwrap_or(false);
                // "Gate" analogue for V2, calibrated in E4: accept iff
                // rank-1 evidence clears the zero-false-accept operating
                // point (a=7 inliers, b=0.5 concentration at bands=512).
                const CALIB_MIN_INLIERS: usize = 7;
                const CALIB_MIN_CONCENTRATION: f32 = 0.5;
                let gated = track_hit
                    && best
                        .map(|o| {
                            o.inliers >= CALIB_MIN_INLIERS
                                && o.offset_concentration >= CALIB_MIN_CONCENTRATION
                        })
                        .unwrap_or(false);

                cases.push(CaseResult {
                    case_id,
                    degradation: deg.id(),
                    expected_track: track.name.clone(),
                    matched_track: matched_name,
                    score: best.map(|o| o.inliers),
                    track_hit,
                    offset_hit: offset_ok,
                    gated_hit: gated,
                    offset_frames_expected: expected_offset,
                    offset_frames_matched: offset_matched,
                    fingerprint_us,
                    match_us,
                    score_weight: best.map(|o| o.weighted_score),
                    offset_concentration: best.map(|o| o.offset_concentration),
                    margin_over_next: best.map(|o| o.margin_over_next),
                });
                case_id += 1;
            }
        }
    }

    // --- Out-of-catalog rejection ---
    let held_seed = corpus.seed ^ 0xDEAD_BEEF;
    let held_samples =
        fixtures::synth_song(held_seed, grid.excerpt_seconds + 2.0, corpus.sample_rate);
    let mut rejection_cases = Vec::new();
    for deg in &grid.degradations {
        let query = deg.apply(&held_samples, corpus.sample_rate, 0xC0FFEE);
        let qfps_raw = sivana_landmark::fingerprint(&query, corpus.sample_rate, &lm_cfg);
        let qfps: Vec<sivana_match::QueryFp> = qfps_raw
            .iter()
            .map(|f| sivana_match::QueryFp {
                hash: f.hash,
                anchor_time: f.anchor_time,
            })
            .collect();
        let outcomes = index.query(&qfps, &params);
        rejection_cases.push(RejectionCase {
            degradation: deg.id(),
            accepted_by_gate: outcomes
                .first()
                .map(|o| o.inliers >= 7 && o.offset_concentration >= 0.5)
                .unwrap_or(false),
            best_score: outcomes.first().map(|o| o.inliers),
            best_inliers: outcomes.first().map(|o| o.inliers),
            best_concentration: outcomes.first().map(|o| o.offset_concentration),
            best_margin: outcomes.first().map(|o| o.margin_over_next),
        });
    }

    Ok(RunSummary {
        engine: if freq_bands == 256 {
            "landmark-v2".into()
        } else {
            format!("landmark-v2-b{freq_bands}")
        },
        fingerprint_version: format!(
            "v2-32bit ({:?})",
            sivana_core::FingerprintVersion::LANDMARK_V2_32BIT
        ),
        seed: corpus.seed,
        sample_rate_hz: corpus.sample_rate,
        track_seconds: corpus.duration_s,
        n_tracks: corpus.tracks.len(),
        excerpt_seconds: grid.excerpt_seconds,
        config: cfg,
        landmark_config: Some(landmark_config),
        cases,
        rejection_cases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_helpers_are_sane() {
        assert_eq!(mean([10u128, 20].into_iter()), 15.0);
        assert_eq!(p95([1.0, 2.0, 3.0, 4.0].into_iter()), 4.0);
        assert_eq!(ratio(&[], |c| c.track_hit), 0.0);
    }
}
