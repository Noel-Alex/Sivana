//! Server-side recognition session (PLAN §25, §26).
//!
//! A session accumulates SFP1 fingerprint batches streamed from the
//! browser and re-queries the catalog after every batch, emitting state
//! transitions:
//!
//! ```text
//! LISTENING -> CANDIDATE -> CONFIDENT_MATCH
//!        \-> NO_MATCH (timeout with no candidate)
//! ```
//!
//! Acceptance uses the E4-calibrated gate; intermediate states use
//! deliberately looser evidence so the UI can show progress without
//! claiming a result.

use std::time::Instant;

use sivana_match::{InMemoryIndex, MatchOutcome, MatchParams, QueryFp};

/// Calibrated zero-false-accept gate (E4: bands=512, tol=2).
pub const GATE_MIN_INLIERS: usize = 7;
/// E8/E10: same-franchise catalogs produce cross-track collisions whose
/// inlier counts, concentration, uniqueness and even span all overlap
/// true matches; only the winner's margin over the runner-up separates
/// them. Measured on REAL audio (E10 live-capture sweep, 9 Toby Fox
/// tracks x 15 positions x 3 durations): every observed false accept
/// landed at margin 2.52-2.80, while true matches scored >= 3.79 except
/// a single outlier at 2.75. The previous 2.5 floor sat INSIDE the
/// false-accept band and let wrong songs win in production. 3.0 clears
/// the entire measured false band; the price is the one 2.75 true case
/// (a rare miss beats a confident wrong answer).
pub const GATE_MIN_MARGIN: f32 = 3.0;
/// Solo-catalog acceptance (n_recordings == 1): no runner-up exists, so
/// margin cannot separate truth from lucky collisions. Three features
/// carry the gate, each calibrated from live measurement
/// (E10/E11/E11b/E11c):
///
/// * DENSITY (inliers / (query_span_frames + 1)) separates scattered
///   coincidence (<=1.36) from concentrated alignment; 2.0 sits below
///   every true case's sustained window and above every scatter case.
///   Phone-speaker EQ loss halves the arrival rate but keeps density
///   windows at ~1.95-2.5 during capture.
/// * CONFIRMATION closes the one remaining hole: same-franchise
///   melodic QUOTATIONS (lost-girl quoting MEGALOVANIA) deliver a
///   genuine burst of consistent evidence (33 distinct pair-hashes,
///   conc 1.0, density 2.06) and then STOP when the quoted phrase ends.
///   Density alone cannot separate burst-then-stall from continuous
///   arrival, but growth CAN: a solo match is only confirmed when the
///   inlier count has grown by GATE_SOLO_CONFIRM_GROWTH after
///   GATE_SOLO_CONFIRM_SECONDS beyond the moment the candidate first
///   cleared the floors. Real playback keeps feeding new alignment for
///   as long as the mic hears it; a quotation runs dry.
pub const GATE_SOLO_MIN_INLIERS: usize = 30;
pub const GATE_SOLO_MIN_CONCENTRATION: f32 = 0.8;
pub const GATE_SOLO_MIN_DENSITY: f32 = 2.0;
pub const GATE_SOLO_CONFIRM_SECONDS: f32 = 2.0;
pub const GATE_SOLO_CONFIRM_GROWTH: usize = 8;
/// Absolute mass required AT CONFIRMATION TIME. Same-franchise
/// quotations keep re-triggering their quoted phrase (lost-girl
/// reprises MEGALOVANIA), so growth-based confirmation alone cannot
/// separate them from real playback: a sliding trailing window
/// eventually sits entirely inside a reprise and reads conc 1.0 with
/// growing inliers. Measured ceilings/floors (E11c): quotation junk
/// never exceeds ~110 sustained aligned hashes (the phrase's total
/// fingerprint inventory), while EVERY true case — including
/// phone-speaker-degraded ones — banks >=264 within the capture
/// window. 150 sits between the bands.
pub const GATE_SOLO_CONFIRM_MIN_INLIERS: usize = 150;
pub const GATE_MIN_CONCENTRATION: f32 = 0.5;
/// Robustness-contract verifier floor: at least this many DISTINCT query
/// hashes must align with the winning candidate. A single hash repeating
/// at many query times can stack up inliers in repetitive audio without
/// adding identity; uniqueness cannot be inflated that way. The E4/E8
/// probes all carried >=5 unique aligned hashes, so the floor sits well
/// below real-match evidence while killing degenerate one-hash votes.
pub const GATE_MIN_UNIQUE_ALIGNED: usize = 4;
/// Looser bar for surfacing an interim CANDIDATE.
const CANDIDATE_MIN_INLIERS: usize = 4;
const CANDIDATE_MIN_CONCENTRATION: f32 = 0.3;
/// Give up after this much captured audio without a confident match.
pub const MAX_CAPTURE_SECONDS: f32 = 12.0;
/// Re-running a full catalog query for every AudioWorklet message creates
/// quadratic work as the evidence window grows. A 250 ms audio-time cadence
/// is responsive while keeping matching comfortably ahead of real time.
const QUERY_CADENCE_SECONDS: f32 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecognitionState {
    Listening,
    Candidate,
    ConfidentMatch,
    NoMatch,
}

/// One streaming recognition session.
pub struct RecognitionSession {
    pub started_at: Instant,
    /// Fingerprints received so far, bounded to the trailing window.
    fps: Vec<QueryFp>,
    pub state: RecognitionState,
    pub outcome: Option<MatchOutcome>,
    pub batches: usize,
    /// Width of the trailing query window in STFT frame coordinates.
    /// Fingerprint density is content-dependent, so this must not be used
    /// as a cap on the number of fingerprints.
    max_window_frames: u32,
    /// Latest audio anchor included in a full matcher evaluation.
    last_evaluated_anchor: Option<u32>,
    min_query_stride_frames: u32,
    /// Solo-catalog confirmation state (E11c): the winning recording when
    /// armed, the capture-time instant it first cleared the solo floors,
    /// and its inlier count at that moment. A confident match requires
    /// evidence to still be growing after `GATE_SOLO_CONFIRM_SECONDS`,
    /// which burst-then-stall quotation collisions never achieve.
    solo_armed_at: Option<(sivana_core::RecordingId, f32, usize)>,
    #[cfg(test)]
    evaluations: usize,
}

impl RecognitionSession {
    pub fn new(sample_rate_hz: u32, hop: usize) -> Self {
        let fps_rate = sample_rate_hz as f32 / hop.max(1) as f32;
        Self {
            started_at: Instant::now(),
            fps: Vec::new(),
            state: RecognitionState::Listening,
            outcome: None,
            batches: 0,
            // Keep fingerprints whose anchors fall in the trailing ~10 s
            // (§25 window). A dense song can emit several fingerprints per
            // frame, so a Vec-length cap would discard most of the window.
            max_window_frames: (fps_rate * 10.0) as u32,
            last_evaluated_anchor: None,
            min_query_stride_frames: (fps_rate * QUERY_CADENCE_SECONDS).ceil() as u32,
            solo_armed_at: None,
            #[cfg(test)]
            evaluations: 0,
        }
    }

    pub fn capture_seconds(&self) -> f32 {
        self.started_at.elapsed().as_secs_f32()
    }

    /// Eagerly enforce the capture timeout: a client that stops (or
    /// finishes) streaming must still get a terminal event instead of
    /// hanging until its next batch. Called on a timer by the server;
    /// returns the state after any transition. A Candidate that never
    /// strengthens within the window is also a NoMatch — otherwise weak
    /// queries hang forever.
    pub fn poll_timeout(&mut self) -> RecognitionState {
        if (self.state == RecognitionState::Listening || self.state == RecognitionState::Candidate)
            && self.capture_seconds() > MAX_CAPTURE_SECONDS
        {
            self.state = RecognitionState::NoMatch;
        }
        self.state
    }

    /// Ingest one batch and advance the state machine.
    ///
    /// `index` is the active catalog; `params` carries matcher config
    /// (offset tolerance). Returns the event to stream to the client.
    pub fn ingest(
        &mut self,
        batch: Vec<QueryFp>,
        index: &InMemoryIndex,
        params: &MatchParams,
    ) -> RecognitionState {
        if self.state == RecognitionState::ConfidentMatch || self.state == RecognitionState::NoMatch
        {
            return self.state; // terminal
        }
        self.batches += 1;
        self.fps.extend(batch);
        let latest_anchor = self.fps.iter().map(|fp| fp.anchor_time).max();
        if let Some(latest_anchor) = latest_anchor {
            let cutoff = latest_anchor.saturating_sub(self.max_window_frames);
            self.fps.retain(|fp| fp.anchor_time >= cutoff);
        }

        let should_evaluate = latest_anchor.is_some_and(|latest| {
            self.last_evaluated_anchor.is_none_or(|previous| {
                latest.saturating_sub(previous) >= self.min_query_stride_frames
            })
        });
        if !should_evaluate {
            if self.capture_seconds() > MAX_CAPTURE_SECONDS {
                self.state = RecognitionState::NoMatch;
            }
            return self.state;
        }
        self.last_evaluated_anchor = latest_anchor;
        #[cfg(test)]
        {
            self.evaluations += 1;
        }

        let outcomes = index.query(&self.fps, params);
        self.outcome = outcomes.first().cloned();
        if let Some(top) = outcomes.first() {
            // Multi-recording catalogs: margin is the feature that separates
            // truth from lucky collisions (measured: pink noise against a
            // single-track catalog reaches 155 inliers at conc 1.0 — no
            // absolute floor survives; E10: cross-track false accepts sit at
            // margin 2.52-2.80, true matches above 3.0).
            //
            // Solo catalogs have no runner-up, so the same job falls to
            // evidence density: true audio alignment concentrates inliers in
            // adjacent frames, junk spreads them across the window (E11).
            let solo = index.n_recordings() < 2;
            let density = (top.inliers as f32) / (top.query_span_frames as f32 + 1.0);
            let accepted = if solo {
                // The floors decide when to ARM, not whether every single
                // evaluation passes: cumulative density decays as the query
                // window widens even for perfect playback (E11b), so
                // re-checking them each round would disarm real matches.
                // Once armed, confirmation alone gates acceptance — and it
                // is keyed to the winning recording, resetting if the
                // leader changes.
                if self
                    .solo_armed_at
                    .is_some_and(|(rec, _, _)| rec != top.recording)
                {
                    self.solo_armed_at = None;
                }
                let floors_clear = top.inliers >= GATE_SOLO_MIN_INLIERS
                    && top.offset_concentration >= GATE_SOLO_MIN_CONCENTRATION
                    && density >= GATE_SOLO_MIN_DENSITY
                    && top.unique_aligned >= GATE_MIN_UNIQUE_ALIGNED;
                if floors_clear && self.solo_armed_at.is_none() {
                    self.solo_armed_at = Some((top.recording, self.capture_seconds(), top.inliers));
                }
                match self.solo_armed_at {
                    Some((_, armed_at, armed_inliers)) => {
                        self.capture_seconds() - armed_at >= GATE_SOLO_CONFIRM_SECONDS
                            && top.inliers >= armed_inliers + GATE_SOLO_CONFIRM_GROWTH
                            && top.inliers >= GATE_SOLO_CONFIRM_MIN_INLIERS
                    }
                    None => false,
                }
            } else {
                top.inliers >= GATE_MIN_INLIERS
                    && top.offset_concentration >= GATE_MIN_CONCENTRATION
                    && top.unique_aligned >= GATE_MIN_UNIQUE_ALIGNED
                    && top.margin_over_next >= GATE_MIN_MARGIN
            };
            if accepted {
                self.state = RecognitionState::ConfidentMatch;
                return self.state;
            }
            if top.inliers >= CANDIDATE_MIN_INLIERS
                && top.offset_concentration >= CANDIDATE_MIN_CONCENTRATION
            {
                self.state = RecognitionState::Candidate;
                return self.state;
            }
        }

        if self.capture_seconds() > MAX_CAPTURE_SECONDS {
            self.state = RecognitionState::NoMatch;
        } else {
            self.state = RecognitionState::Listening;
        }
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sivana_core::RecordingId;
    use sivana_match::MatchParams;

    fn index_with_shared_hashes() -> InMemoryIndex {
        // Recording 0 shares many distinct hashes with the query stream;
        // recording 1 shares one.
        let mut idx = InMemoryIndex::new();
        idx.add_recording(
            RecordingId::new(0),
            &[
                (100, 10),
                (101, 12),
                (102, 14),
                (103, 16),
                (104, 18),
                (105, 20),
                (106, 22),
                (107, 24),
                (108, 26),
            ],
        );
        idx.add_recording(RecordingId::new(1), &[(100, 900)]);
        idx.finalize();
        idx
    }

    fn query_batch(hashes: &[(u32, u32)]) -> Vec<QueryFp> {
        hashes
            .iter()
            .map(|&(h, t)| QueryFp {
                hash: h,
                anchor_time: t,
            })
            .collect()
    }

    #[test]
    fn session_reaches_confident_match() {
        let idx = index_with_shared_hashes();
        let mut s = RecognitionSession::new(22_050, 1024);
        let batch = query_batch(&[
            (100, 0),
            (101, 2),
            (102, 4),
            (103, 6),
            (104, 8),
            (105, 10),
            (106, 12),
            (107, 14),
            (108, 16),
        ]);
        let state = s.ingest(batch, &idx, &MatchParams::default());
        assert_eq!(state, RecognitionState::ConfidentMatch);
        let o = s.outcome.as_ref().unwrap();
        assert_eq!(o.recording.as_u32(), 0);
        assert_eq!(o.offset_frames, 10);
    }

    #[test]
    fn session_times_out_to_no_match() {
        // Empty catalog: no evidence ever arrives.
        let mut idx = InMemoryIndex::new();
        idx.finalize();
        let mut s = RecognitionSession::new(22_050, 1024);
        // Force the clock: pretend we've been listening for a while.
        s.started_at -= std::time::Duration::from_secs(30);
        let state = s.ingest(query_batch(&[(7, 1)]), &idx, &MatchParams::default());
        assert_eq!(state, RecognitionState::NoMatch);
        // Terminal: further batches change nothing.
        let again = s.ingest(query_batch(&[(8, 1)]), &idx, &MatchParams::default());
        assert_eq!(again, RecognitionState::NoMatch);
    }

    #[test]
    fn single_recording_catalog_does_not_require_a_runner_up_margin() {
        let mut idx = InMemoryIndex::new();
        idx.add_recording(
            RecordingId::new(0),
            &[
                (100, 10),
                (101, 12),
                (102, 14),
                (103, 16),
                (104, 18),
                (105, 20),
                (106, 22),
            ],
        );
        idx.finalize();
        let mut session = RecognitionSession::new(22_050, 1024);
        let state = session.ingest(
            query_batch(&[
                (100, 0),
                (101, 2),
                (102, 4),
                (103, 6),
                (104, 8),
                (105, 10),
                (106, 12),
            ]),
            &idx,
            &MatchParams::default(),
        );
        assert_eq!(session.outcome.as_ref().unwrap().margin_over_next, 1.0);
        // Weak evidence on a solo catalog stays a candidate: without a
        // runner-up margin, acceptance leans on the density floor and this
        // query carries too few inliers to clear it.
        assert_eq!(state, RecognitionState::Candidate);
    }

    #[test]
    fn dense_solo_catalog_evidence_confidently_matches() {
        // One recording whose hashes align with a dense query stream at a
        // single constant offset. Real capture arrives in waves, and the
        // E11c confirmation stage requires evidence to still be growing
        // after GATE_SOLO_CONFIRM_SECONDS — so the test streams a first
        // wave (40 hashes over 8 frames, density ~4.4), waits past the
        // confirm window, then streams a second wave on the same offset.
        let mut idx = InMemoryIndex::new();
        let mut catalog = Vec::new();
        // Enough hash inventory to exceed GATE_SOLO_CONFIRM_MIN_INLIERS:
        // 200 hashes over two timeline regions.
        for i in 0..200u32 {
            catalog.push((500 + i, 100 + (i / 5) * 2));
        }
        idx.add_recording(RecordingId::new(0), &catalog);
        idx.finalize();
        let mut session = RecognitionSession::new(22_050, 1024);
        // Wave 1 at fixed offset 90; crosses the solo floors and arms the
        // confirmation timer.
        let wave1: Vec<QueryFp> = (0..60)
            .map(|i| QueryFp {
                hash: 500 + i,
                anchor_time: 10 + (i / 5) * 2,
            })
            .collect();
        let state1 = session.ingest(wave1, &idx, &MatchParams::default());
        assert_ne!(state1, RecognitionState::ConfidentMatch);
        // Advance the clock beyond the confirm window.
        session.started_at -= std::time::Duration::from_secs_f32(GATE_SOLO_CONFIRM_SECONDS + 0.5);
        // Wave 2 on the same alignment: mass grows past the confirm floor.
        let wave2: Vec<QueryFp> = (60..200)
            .map(|i| QueryFp {
                hash: 500 + i,
                anchor_time: 10 + (i / 5) * 2,
            })
            .collect();
        let state2 = session.ingest(wave2, &idx, &MatchParams::default());
        assert_eq!(state2, RecognitionState::ConfidentMatch);
        let o = session.outcome.as_ref().unwrap();
        assert_eq!(o.recording.as_u32(), 0);
    }

    #[test]
    fn stalled_solo_evidence_does_not_confirm() {
        // The quotation signature (E11c): a strong burst that clears every
        // floor but never accumulates GATE_SOLO_CONFIRM_MIN_INLIERS of
        // mass — the quoted phrase's hash inventory runs out.
        let mut idx = InMemoryIndex::new();
        let catalog: Vec<(u32, u32)> = (0..40).map(|i| (500 + i, 100 + i)).collect();
        idx.add_recording(RecordingId::new(0), &catalog);
        idx.finalize();
        let mut session = RecognitionSession::new(22_050, 1024);
        let burst: Vec<QueryFp> = (0..40)
            .map(|i| QueryFp {
                hash: 500 + i,
                anchor_time: 10 + i / 3, // dense burst, density > 2
            })
            .collect();
        let state1 = session.ingest(burst, &idx, &MatchParams::default());
        assert_ne!(state1, RecognitionState::ConfidentMatch);
        // Clock passes the confirm window; a reprise re-triggers growth on
        // the same small inventory (inliers fluctuate around 40-100) but
        // mass stays below the confirm floor.
        session.started_at -= std::time::Duration::from_secs_f32(GATE_SOLO_CONFIRM_SECONDS + 1.0);
        let reprise: Vec<QueryFp> = (0..30)
            .map(|i| QueryFp {
                hash: 500 + i,
                anchor_time: 400 + i / 3,
            })
            .collect();
        let state2 = session.ingest(reprise, &idx, &MatchParams::default());
        assert_ne!(state2, RecognitionState::ConfidentMatch);
    }

    #[test]
    fn sparse_solo_catalog_evidence_stays_a_candidate() {
        // Same hash identity but spread thin across the window: density ~0.2,
        // the signature of scattered coincidence rather than aligned audio.
        let mut idx = InMemoryIndex::new();
        let catalog: Vec<(u32, u32)> = (0..40).map(|i| (500 + i as u32, 100 + i as u32)).collect();
        idx.add_recording(RecordingId::new(0), &catalog);
        idx.finalize();
        let mut session = RecognitionSession::new(22_050, 1024);
        let query: Vec<QueryFp> = (0..40)
            .map(|i| QueryFp {
                hash: 500 + i as u32,
                anchor_time: i as u32 * 20, // 40 hashes over 780 frames
            })
            .collect();
        let state = session.ingest(query, &idx, &MatchParams::default());
        assert_ne!(state, RecognitionState::ConfidentMatch);
    }

    #[test]
    fn trailing_window_is_bounded_by_audio_time_not_fingerprint_count() {
        let mut idx = InMemoryIndex::new();
        idx.finalize();
        let mut session = RecognitionSession::new(22_050, 1024);
        let dense: Vec<QueryFp> = (0..=1_000)
            .flat_map(|frame| {
                (0..10).map(move |salt| QueryFp {
                    hash: frame * 10 + salt,
                    anchor_time: frame,
                })
            })
            .collect();
        session.ingest(dense, &idx, &MatchParams::default());

        let first = session.fps.first().unwrap().anchor_time;
        let last = session.fps.last().unwrap().anchor_time;
        assert_eq!(last, 1_000);
        assert!(first >= 1_000 - session.max_window_frames);
        assert!(session.fps.len() > 2_000, "dense evidence was count-capped");
    }

    #[test]
    fn matcher_evaluations_are_throttled_by_audio_time() {
        let mut idx = InMemoryIndex::new();
        idx.finalize();
        let mut session = RecognitionSession::new(22_050, 1024);

        for anchor_time in 0..100 {
            session.ingest(
                vec![QueryFp {
                    hash: anchor_time + 1,
                    anchor_time,
                }],
                &idx,
                &MatchParams::default(),
            );
        }

        assert!(
            session.evaluations <= 18,
            "100 worklet-sized batches caused {} full matcher runs",
            session.evaluations
        );
        assert!(session.evaluations >= 16);
    }
}
