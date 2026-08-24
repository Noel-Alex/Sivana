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
/// E8: same-franchise catalogs produce cross-track collisions whose
/// inlier counts overlap true matches; the winner's margin over the
/// runner-up separates cleanly (false accepts <= 1.8, true >= 3.0 on
/// measured DELTARUNE probes).
pub const GATE_MIN_MARGIN: f32 = 2.5;
pub const GATE_MIN_CONCENTRATION: f32 = 0.5;
/// Looser bar for surfacing an interim CANDIDATE.
const CANDIDATE_MIN_INLIERS: usize = 4;
const CANDIDATE_MIN_CONCENTRATION: f32 = 0.3;
/// Give up after this much captured audio without a confident match.
pub const MAX_CAPTURE_SECONDS: f32 = 12.0;

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
    max_window_fps: usize,
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
            // Keep at most ~10 s of trailing fingerprints (§25 window).
            max_window_fps: (fps_rate * 10.0) as usize,
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
        if self.fps.len() > self.max_window_fps {
            let drop = self.fps.len() - self.max_window_fps;
            self.fps.drain(0..drop);
        }

        let outcomes = index.query(&self.fps, params);
        if let Some(top) = outcomes.first() {
            if top.inliers >= GATE_MIN_INLIERS
                && top.offset_concentration >= GATE_MIN_CONCENTRATION
                && top.margin_over_next >= GATE_MIN_MARGIN
            {
                self.state = RecognitionState::ConfidentMatch;
                self.outcome = Some(top.clone());
                return self.state;
            }
            if top.inliers >= CANDIDATE_MIN_INLIERS
                && top.offset_concentration >= CANDIDATE_MIN_CONCENTRATION
            {
                self.state = RecognitionState::Candidate;
                self.outcome = Some(top.clone());
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
}
