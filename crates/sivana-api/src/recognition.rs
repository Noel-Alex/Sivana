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
/// Solo-catalog acceptance (n_recordings == 1), E13: no runner-up exists,
/// so margin cannot separate truth from collisions; the job belongs to
/// the fingerprinter's temporal background suppression (E12), which
/// removed stationary phantom evidence at the SOURCE. Post-E12 measured
/// bands (live WS, realtime pacing, re-ingested catalog): negatives and
/// pure room tone peak <=10 aligned inliers; every true case >=143 even
/// under heavy degradation. The floor sits between the bands with ~6x
/// headroom over junk and ~2x under the weakest true case. The E11c
/// arm/confirm machinery is retired: growth was a ceiling-trap for
/// slow-ramp evidence and density punished capture length (E11b).
pub const GATE_SOLO_MIN_INLIERS_E13: usize = 64;
/// E13 tight-spike prong: a spike only counts once it carries real mass —
/// concentration is measured inside the winning ±2-frame bucket, so any
/// lone stray hash trivially reads conc 1.0 with span 0. True playback
/// shows >=44 aligned inliers within span 9 of first appearance; junk
/// cells hold single digits.
pub const GATE_SPIKE_MIN_INLIERS: usize = 20;
/// E13 tight-spike prong: while the inlier window is still this young,
/// concentration at or above the threshold identifies true alignment.
pub const GATE_SPIKE_MAX_SPAN_FRAMES: u32 = 16;
/// E13 tight-spike prong: fraction of the candidate's vote mass inside
/// the winning ±2-frame bucket. Measured bands: true playback reads
/// 0.98-1.00 (the ±2-frame tolerance absorbs jitter); every false source
/// of alignment — lost-girl quotation 0.88 max, spider-dance shared-patch
/// collision 0.904 max — smears across buckets. 0.95 centers the floor in
/// the measured (0.904, 0.98) gap.
pub const GATE_SPIKE_MIN_CONCENTRATION: f32 = 0.95;
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
            #[cfg(test)]
            evaluations: 0,
        }
    }

    pub fn capture_seconds(&self) -> f32 {
        self.started_at.elapsed().as_secs_f32()
    }

    /// Fraction of the session's fingerprints whose hash exists anywhere
    /// in the catalog (E12 channel-health metric). File queries run
    /// 20-50%; healthy microphone captures 5-15%; broken chains (stale
    /// wasm vs re-ingested catalog, wrong sample-rate handling) <2%.
    /// Surfaced on every candidate/matched/no_match event.
    pub fn catalog_hit_rate(&self, index: &InMemoryIndex) -> f32 {
        if self.fps.is_empty() {
            return 0.0;
        }
        let hits = self
            .fps
            .iter()
            .filter(|fp| index.postings_for(fp.hash).is_some())
            .count();
        hits as f32 / self.fps.len() as f32
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
            let accepted = if solo {
                // E13 gate, two independent prongs measured on the E12
                // (minimum-statistics whitening) engine:
                //
                // * ALIGNMENT TIGHTNESS is the solo-catalog discriminator.
                //   True playback reproduces the catalog's exact sample
                //   timing, so nearly ALL votes land inside one ±2-frame
                //   offset bucket: megalovania reads conc 0.98-1.00 from
                //   the first evaluation. Every measured false source of
                //   aligned evidence — lost-girl reprising the motif,
                //   spider-dance sharing synth patches — smears across
                //   buckets (tempo/performance drift) and NEVER exceeds
                //   0.88. Both prongs therefore demand conc >= 0.9.
                // * SPIKE: small-but-immediate evidence (>=20 inliers while
                //   the window is still young) confirms instantly — this is
                //   the prong that makes playback match within ~0.5 s.
                // * MASS: larger evidence (>=64 distinct pair-hashes)
                //   confirms once enough of the song has streamed; junk
                //   peaks ~10-27.
                let tight = top.offset_concentration >= GATE_SPIKE_MIN_CONCENTRATION;
                top.inliers >= GATE_SOLO_MIN_INLIERS_E13 && tight
                    || (top.inliers >= GATE_SPIKE_MIN_INLIERS
                        && top.query_span_frames <= GATE_SPIKE_MAX_SPAN_FRAMES
                        && tight)
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
        // runner-up margin, acceptance leans on the absolute mass floor
        // and this query carries too few inliers to clear it.
        assert_eq!(state, RecognitionState::Candidate);
    }

    #[test]
    fn dense_solo_catalog_evidence_confidently_matches() {
        // E13: a single absolute mass floor. Enough aligned evidence
        // crossing the floor confirms immediately — no arming, no clock
        // games, no growth requirement (which was a ceiling-trap for
        // slow-ramp real-mic evidence).
        let mut idx = InMemoryIndex::new();
        let catalog: Vec<(u32, u32)> = (0..200).map(|i| (500 + i, 100 + i)).collect();
        idx.add_recording(RecordingId::new(0), &catalog);
        idx.finalize();
        let mut session = RecognitionSession::new(22_050, 1024);
        // Aligned at offset 90 with enough distinct hashes to exceed the
        // E13 floor several times over.
        let wave: Vec<QueryFp> = (0..150)
            .map(|i| QueryFp {
                hash: 500 + i,
                anchor_time: 10 + i,
            })
            .collect();
        let state = session.ingest(wave, &idx, &MatchParams::default());
        assert_eq!(state, RecognitionState::ConfidentMatch);
        let o = session.outcome.as_ref().unwrap();
        assert_eq!(o.recording.as_u32(), 0);
    }

    #[test]
    fn sub_floor_solo_evidence_never_confirms() {
        // The post-E12 junk band: stationary/room-tone evidence and weak
        // collisions peak around ~10 aligned inliers — far below the floor.
        // Mass alone must not confirm; the session stays a candidate until
        // the capture timeout.
        let mut idx = InMemoryIndex::new();
        let catalog: Vec<(u32, u32)> = (0..80).map(|i| (500 + i, 100 + i)).collect();
        idx.add_recording(RecordingId::new(0), &catalog);
        idx.finalize();
        let mut session = RecognitionSession::new(22_050, 1024);
        let burst: Vec<QueryFp> = (0..10)
            .map(|i| QueryFp {
                hash: 500 + i,
                anchor_time: 10 + i,
            })
            .collect();
        let state1 = session.ingest(burst, &idx, &MatchParams::default());
        assert_eq!(state1, RecognitionState::Candidate);
        // More of the same scattered evidence later never crosses 64.
        let more: Vec<QueryFp> = (20..30)
            .map(|i| QueryFp {
                hash: 500 + i,
                anchor_time: 400 + i,
            })
            .collect();
        let state2 = session.ingest(more, &idx, &MatchParams::default());
        assert_ne!(state2, RecognitionState::ConfidentMatch);
    }

    #[test]
    fn sparse_solo_catalog_evidence_stays_a_candidate() {
        // Same hash identity but spread thin across the window: scattered
        // coincidence rather than aligned audio.
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
