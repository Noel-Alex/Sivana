//! Sivana Matcher V2 (research/PLAN.md §22-§26).
//!
//! Replaces the prototype's nested `HashMap<SongId, HashMap<offset, count>>`
//! voting and hard-coded gate with:
//!
//! * document-frequency (rarity) weights `w(h) = ln((N+1)/(df+1))` (§14),
//!   where df counts *distinct recordings* containing the hash
//! * stop-hash suppression above a df threshold (§15)
//! * flat vote tuples `(recording, offset_bucket)` in contiguous vectors,
//!   sorted + grouped — no nested maps in the hot path (§23)
//! * per-offset breakdown inside each candidate cell for verification (§24)
//! * score features returned raw for calibration work (§26)
//!
//! All aggregation structures are ordered (`BTreeMap`) and every ranking
//! has an explicit tie-break, so results are fully deterministic across
//! runs and platforms.

use std::collections::{BTreeMap, HashMap};

use sivana_core::RecordingId;

/// One query fingerprint: 32-bit hash + anchor time in frames.
#[derive(Debug, Clone, Copy)]
pub struct QueryFp {
    pub hash: u32,
    pub anchor_time: u32,
}

/// Posting entry stored per hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posting {
    pub recording: RecordingId,
    pub anchor_time: u32,
}

/// In-memory reference index. Phase 3 swaps this for mmap segments; the
/// matcher API is designed to survive that swap.
///
/// Build with [`Self::add_recording`], call [`Self::finalize`] once, then
/// query any number of times.
#[derive(Default)]
pub struct InMemoryIndex {
    postings: HashMap<u32, Vec<Posting>>,
    /// Distinct-recording document frequency per hash (§14). Populated by
    /// [`Self::finalize`].
    df: HashMap<u32, u64>,
    n_recordings: u64,
    finalized: bool,
}

impl InMemoryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add all fingerprints of one recording. Cheap; may leave each hash's
    /// postings unsorted until [`Self::finalize`].
    pub fn add_recording(&mut self, recording: RecordingId, fps: &[(u32, u32)]) {
        assert!(!self.finalized, "add_recording after finalize");
        self.n_recordings += 1;
        for (hash, anchor) in fps {
            self.postings.entry(*hash).or_default().push(Posting {
                recording,
                anchor_time: *anchor,
            });
        }
    }

    /// Sort postings per hash and compute distinct-recording document
    /// frequencies. Must be called once between building and querying;
    /// idempotent.
    pub fn finalize(&mut self) {
        self.df.clear();
        self.df.reserve(self.postings.len());
        for (hash, plist) in self.postings.iter_mut() {
            plist.sort_by_key(|p| (p.recording.as_u32(), p.anchor_time));
            plist.dedup();
            let mut seen = usize::from(!plist.is_empty());
            for w in plist.windows(2) {
                if w[1].recording != w[0].recording {
                    seen += 1;
                }
            }
            self.df.insert(*hash, seen as u64);
        }
        self.finalized = true;
    }

    /// Distinct hashes in the index (not recordings).
    pub fn len(&self) -> usize {
        self.postings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.postings.is_empty()
    }

    /// Number of recordings added.
    pub fn n_recordings(&self) -> u64 {
        self.n_recordings
    }

    /// Raw posting list for one hash (sorted, deduped after finalize).
    /// Exposed so alternative scoring schemes (e.g. Engine B's affine
    /// verification) can reuse this index as pure storage.
    pub fn postings_for(&self, hash: u32) -> Option<&[Posting]> {
        self.postings.get(&hash).map(|v| v.as_slice())
    }

    /// Rarity weight (§14). The raw formula `ln((N+1)/(df+1))` hits
    /// exactly zero when df == N (hash present in every recording), which
    /// would silence entire tiny catalogs; we floor it at a small epsilon
    /// so ubiquitous hashes are near-zero-weight instead of dead.
    fn idf(&self, df: u64) -> f32 {
        const EPS: f32 = 1e-3;
        (((self.n_recordings as f64 + 1.0) / (df as f64 + 1.0)).ln() as f32).max(EPS)
    }
}

/// Result of matching one query.
#[derive(Debug, Clone)]
pub struct MatchOutcome {
    pub recording: RecordingId,
    /// Rarity-weighted vote mass after stop-hash filtering.
    pub weighted_score: f32,
    /// Votes agreeing with the reported offset within the configured
    /// tolerance (inliers, §24).
    pub inliers: usize,
    /// Fraction of this candidate's vote mass inside the tolerance window
    /// around the dominant offset.
    pub offset_concentration: f32,
    /// Verified time offset of the recording relative to the query (frames):
    /// the dominant exact offset inside the winning bucket.
    pub offset_frames: i64,
    /// `weighted_score` divided by the next candidate's score (rank-1 only
    /// carries a competitor's ratio; 1.0 when there is no next row). A
    /// calibration feature (§26).
    pub margin_over_next: f32,
    /// Span of query anchor times contributing inlier votes to this
    /// candidate, in frames (robustness contract: "matched query-time span").
    /// Repetitive music concentrates evidence at one query moment; a real
    /// capture spreads it across the whole window.
    pub query_span_frames: u32,
    /// Distinct query hashes among the inliers (robustness contract:
    /// "unique aligned occurrences"). One hash repeating at many times
    /// inflates `inliers` without adding identity; this counts what is
    /// actually unique.
    pub unique_aligned: usize,
    /// Mean rarity weight over the inlier votes (the idf mean behind
    /// `weighted_score`). Ubiquitous timbre hashes drag this toward the
    /// epsilon floor even when raw counts look healthy; a calibrated model
    /// can use that directly instead of inferring it from mass.
    pub mean_rarity: f32,
}

#[derive(Debug, Clone)]
pub struct MatchParams {
    /// Hashes whose distinct-recording df exceeds this are ignored at
    /// query time (§15).
    pub stop_hash_df_threshold: u64,
    /// Keep this many candidates for verification.
    pub shortlist_len: usize,
    /// Offset tolerance in frames (§24): votes whose exact offsets fall in
    /// a common ±`tolerance` window reinforce each other instead of
    /// requiring bit-exact alignment. 0 restores exact voting.
    pub offset_tolerance_frames: i64,
}

impl Default for MatchParams {
    fn default() -> Self {
        Self {
            // With tiny catalogs everything is rare; tune by sweep later.
            stop_hash_df_threshold: u64::MAX,
            shortlist_len: 5,
            // E4: tolerance 2 frames maximizes zero-FA gated recall on the
            // standard grid (75->76.7% at bands=512).
            offset_tolerance_frames: 2,
        }
    }
}

impl InMemoryIndex {
    /// Match a query fingerprint batch; returns up to `shortlist_len`
    /// candidates sorted by verified score (best first, ties broken by
    /// ascending recording id). Empty when the catalog has never heard
    /// any of the query's hashes.
    ///
    /// Panics if [`Self::finalize`] was not called first.
    pub fn query(&self, query: &[QueryFp], params: &MatchParams) -> Vec<MatchOutcome> {
        assert!(self.finalized, "call finalize() before query()");

        // Flat vote tuples; key = (recording, offset_bucket).
        #[derive(Clone, Copy)]
        struct Vote {
            rec: RecordingId,
            bucket: i64,
            exact: i64,
            weight: f32,
            /// Query-side provenance for the calibration features: which
            /// hash voted, and where in the query timeline it sits.
            hash: u32,
            q_anchor: u32,
        }
        let mut votes: Vec<Vote> = Vec::with_capacity(query.len() * 8);
        let mut seen_hashes = std::collections::HashSet::new();
        // Bucket width: with tolerance T, offsets within ±T of each other
        // share a cell via euclidean division (symmetric around zero).
        let tol = params.offset_tolerance_frames.max(0);
        let width = 2 * tol + 1;

        for q in query {
            if !seen_hashes.insert(q.hash) {
                continue; // dedup query hashes (§22)
            }
            if let Some(postings) = self.postings.get(&q.hash) {
                let df = self.df[&q.hash];
                if df > params.stop_hash_df_threshold {
                    continue;
                }
                let w = self.idf(df);
                for p in postings {
                    let offset = p.anchor_time as i64 - q.anchor_time as i64;
                    votes.push(Vote {
                        rec: p.recording,
                        bucket: offset.div_euclid(width),
                        exact: offset,
                        weight: w,
                        hash: q.hash,
                        q_anchor: q.anchor_time,
                    });
                }
            }
        }
        if votes.is_empty() {
            return Vec::new();
        }

        // Group by (recording, bucket): sort then walk runs — cache-friendly
        // replacement for nested hashmaps (§23 Option A).
        votes.sort_by_key(|v| (v.rec.as_u32(), v.bucket));

        // Per-cell accumulation. Exact offsets live in a BTreeMap so both
        // the dominant-offset pick and range queries are deterministic.
        struct Acc {
            total_weight: f32,
            votes: usize,
            offsets: BTreeMap<i64, (usize, f32)>, // exact offset -> (count, w)
            /// Distinct contributing query hashes (calibration feature).
            hashes: std::collections::HashSet<u32>,
            /// Min/max query anchors among contributing votes (span).
            q_min: u32,
            q_max: u32,
        }
        let mut accs: Vec<((u32, i64), Acc)> = Vec::new();
        for v in &votes {
            match accs.last_mut() {
                Some(((r, b), acc)) if *r == v.rec.as_u32() && *b == v.bucket => {
                    acc.total_weight += v.weight;
                    acc.votes += 1;
                    let e = acc.offsets.entry(v.exact).or_insert((0, 0.0));
                    e.0 += 1;
                    e.1 += v.weight;
                    acc.hashes.insert(v.hash);
                    acc.q_min = acc.q_min.min(v.q_anchor);
                    acc.q_max = acc.q_max.max(v.q_anchor);
                }
                _ => {
                    let mut m = BTreeMap::new();
                    m.insert(v.exact, (1, v.weight));
                    accs.push((
                        (v.rec.as_u32(), v.bucket),
                        Acc {
                            total_weight: v.weight,
                            votes: 1,
                            offsets: m,
                            hashes: std::iter::once(v.hash).collect(),
                            q_min: v.q_anchor,
                            q_max: v.q_anchor,
                        },
                    ));
                }
            }
        }

        // Best cell per recording. Walked in accs' deterministic order with
        // strict `>` so the earliest bucket wins ties.
        let mut best_per_rec: BTreeMap<u32, (i64, f32, usize)> = BTreeMap::new();
        for ((r, b), acc) in &accs {
            match best_per_rec.entry(*r) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert((*b, acc.total_weight, acc.votes));
                }
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    if acc.total_weight > e.get().1 {
                        e.insert((*b, acc.total_weight, acc.votes));
                    }
                }
            }
        }

        let mut rows: Vec<(u32, i64, f32)> = best_per_rec
            .into_iter()
            .map(|(r, (b, w, _))| (r, b, w))
            .collect();
        // Total order: weight desc, then ascending recording id.
        rows.sort_by(|a, b| {
            b.2.partial_cmp(&a.2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        rows.truncate(params.shortlist_len);

        // Verification per shortlisted cell (§24): pick the dominant exact
        // offset inside the bucket (max weight; ties to the smaller offset),
        // then aggregate mass within ±tolerance of it.
        struct Verified {
            rec: u32,
            score: f32,
            offset: i64,
            inliers: usize,
            concentration: f32,
            query_span_frames: u32,
            unique_aligned: usize,
            mean_rarity: f32,
        }
        let mut verified: Vec<Verified> = rows
            .into_iter()
            .map(|(rec_u32, bucket, weight)| {
                let acc = &accs
                    .iter()
                    .find(|((r, b), _)| *r == rec_u32 && *b == bucket)
                    .expect("row must map to an accumulator")
                    .1;
                // Dominant exact offset: highest weight; ties resolve to
                // the smaller offset (deterministic, earlier time).
                let (&best_off, _) = acc
                    .offsets
                    .iter()
                    .max_by(|a, b| {
                        a.1.1
                            .partial_cmp(&b.1.1)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| b.0.cmp(a.0))
                    })
                    .expect("accumulator non-empty");
                // Second walk over the cell's votes collects calibration
                // features from inliers only (votes were sorted by
                // (rec, bucket), so this run is contiguous).
                let mut inliers = 0usize;
                let mut inlier_w = 0.0f32;
                for (_off, (count, w)) in acc.offsets.range(best_off - tol..=best_off + tol) {
                    inliers += count;
                    inlier_w += w;
                }
                let mut inlier_hashes: std::collections::HashSet<u32> =
                    std::collections::HashSet::new();
                let mut q_min = u32::MAX;
                let mut q_max = 0u32;
                let mut rarity_sum = 0.0f32;
                let vote_run = votes
                    .as_slice()
                    .binary_search_by(|v| (v.rec.as_u32(), v.bucket).cmp(&(rec_u32, bucket)))
                    .map_or(&[][..], |i| {
                        let mut lo = i;
                        while lo > 0
                            && votes[lo - 1].rec.as_u32() == rec_u32
                            && votes[lo - 1].bucket == bucket
                        {
                            lo -= 1;
                        }
                        let mut hi = i;
                        while hi < votes.len()
                            && votes[hi].rec.as_u32() == rec_u32
                            && votes[hi].bucket == bucket
                        {
                            hi += 1;
                        }
                        &votes[lo..hi]
                    });
                for v in vote_run {
                    if (v.exact - best_off).abs() <= tol {
                        inlier_hashes.insert(v.hash);
                        q_min = q_min.min(v.q_anchor);
                        q_max = q_max.max(v.q_anchor);
                        rarity_sum += v.weight;
                    }
                }
                let n_inlier_votes = rarity_sum.max(1e-9);
                Verified {
                    rec: rec_u32,
                    score: weight,
                    offset: best_off,
                    inliers,
                    concentration: if weight > 0.0 { inlier_w / weight } else { 0.0 },
                    query_span_frames: if inlier_hashes.is_empty() {
                        0
                    } else {
                        q_max.saturating_sub(q_min)
                    },
                    unique_aligned: inlier_hashes.len(),
                    mean_rarity: rarity_sum / n_inlier_votes,
                }
            })
            .collect();

        verified.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.rec.cmp(&b.rec))
        });
        let next_scores: Vec<f32> = verified.iter().skip(1).map(|v| v.score).collect();
        verified
            .into_iter()
            .zip(next_scores.iter().chain(std::iter::once(&0.0)))
            .map(|(v, next)| MatchOutcome {
                margin_over_next: if next > &0.0 { v.score / next } else { 1.0 },
                recording: RecordingId::new(v.rec),
                weighted_score: v.score,
                inliers: v.inliers,
                offset_concentration: v.concentration,
                offset_frames: v.offset,
                query_span_frames: v.query_span_frames,
                unique_aligned: v.unique_aligned,
                mean_rarity: v.mean_rarity,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u32 = 100;

    #[test]
    fn clean_query_finds_right_recording_and_offset() {
        let mut idx = InMemoryIndex::new();
        idx.add_recording(RecordingId::new(0), &[(H, 10), (200, 12)]);
        idx.add_recording(RecordingId::new(1), &[(H, 40), (201, 42)]);
        idx.finalize();

        let out = idx.query(
            &[QueryFp {
                hash: H,
                anchor_time: 5,
            }],
            &MatchParams::default(),
        );
        assert_eq!(out.len(), 2); // both recordings share the hash
        assert_eq!(out[0].recording, RecordingId::new(0)); // tie broken by id
        assert_eq!(out[1].recording, RecordingId::new(1));
    }

    #[test]
    fn repeated_votes_beat_single_votes_via_idf_and_mass() {
        let mut idx = InMemoryIndex::new();
        // Recording A matches many distinct query hashes at one offset.
        idx.add_recording(
            RecordingId::new(0),
            &[(1, 100), (2, 102), (3, 104), (4, 106)],
        );
        // Recording B shares exactly one hash with the query.
        idx.add_recording(RecordingId::new(1), &[(3, 999)]);
        idx.finalize();

        let q = [
            QueryFp {
                hash: 1,
                anchor_time: 0,
            },
            QueryFp {
                hash: 2,
                anchor_time: 0,
            },
            QueryFp {
                hash: 3,
                anchor_time: 0,
            },
            QueryFp {
                hash: 4,
                anchor_time: 0,
            },
        ];
        let out = idx.query(
            &q,
            &MatchParams {
                offset_tolerance_frames: 0, // this test reasons about exact cells
                ..MatchParams::default()
            },
        );
        assert!(!out.is_empty());
        assert_eq!(out[0].recording, RecordingId::new(0));
        assert_eq!(out[0].offset_frames, 100);
        assert!(out[0].weighted_score > out.get(1).map(|o| o.weighted_score).unwrap_or(0.0));
        assert!((out[0].offset_concentration - 1.0).abs() < 1e-6);
    }

    #[test]
    fn unknown_query_returns_empty() {
        let mut idx = InMemoryIndex::new();
        idx.add_recording(RecordingId::new(0), &[(7, 1)]);
        idx.finalize();
        let out = idx.query(
            &[QueryFp {
                hash: 123456,
                anchor_time: 0,
            }],
            &MatchParams::default(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn duplicate_query_hashes_counted_once() {
        let mut idx = InMemoryIndex::new();
        idx.add_recording(RecordingId::new(0), &[(9, 50)]);
        idx.finalize();
        let q = [
            QueryFp {
                hash: 9,
                anchor_time: 10,
            },
            QueryFp {
                hash: 9,
                anchor_time: 10,
            },
            QueryFp {
                hash: 9,
                anchor_time: 10,
            },
        ];
        let out = idx.query(&q, &MatchParams::default());
        assert_eq!(out[0].inliers, 1); // one vote, not three
    }

    #[test]
    fn df_counts_recordings_not_postings() {
        // §14: a hash repeated 10x inside ONE recording must stay "rare"
        // (df=1), while the same hash spread across three recordings is
        // common (df=3).
        let mut idx = InMemoryIndex::new();
        let repeats: Vec<(u32, u32)> = (0..10).map(|i| (77, i * 5)).collect();
        idx.add_recording(RecordingId::new(0), &repeats);
        idx.add_recording(RecordingId::new(1), &[(77, 999)]);
        idx.add_recording(RecordingId::new(2), &[(77, 1000)]);
        idx.finalize();

        assert_eq!(idx.df[&77], 3);

        // Weight reflects the small df: ln((N+1)/(df+1)) with N=3, df=3
        // floors to EPS — a catalog-wide hash is nearly weightless.
        let w = idx.idf(idx.df[&77]);
        assert!((w - 1e-3).abs() < 1e-6);
        // And rarity still orders weights monotonically.
        assert!(w < idx.idf(1), "common hash must weigh less than rare");
    }

    #[test]
    fn rare_hash_outranks_common_hash_at_equal_votes() {
        let mut idx = InMemoryIndex::new();
        // Hash 500 appears in all three recordings; hash 600 only in #0.
        idx.add_recording(RecordingId::new(0), &[(500, 10), (600, 11)]);
        idx.add_recording(RecordingId::new(1), &[(500, 20)]);
        idx.add_recording(RecordingId::new(2), &[(500, 30)]);
        idx.finalize();

        let q = [
            QueryFp {
                hash: 500,
                anchor_time: 0,
            },
            QueryFp {
                hash: 600,
                anchor_time: 0,
            },
        ];
        let out = idx.query(&q, &MatchParams::default());
        // Both candidates get two votes, but #0's include the rare hash.
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[0].recording,
            RecordingId::new(0),
            "rare-hash mass should dominate"
        );
    }

    #[test]
    fn offset_tolerance_merges_jittered_votes() {
        // Same hash observed at offsets 100/101/102 in the reference; a
        // query whose anchor makes those offsets land on 100/101/102 must
        // aggregate into one candidate with 3 inliers under T=2, while
        // exact voting (T=0) would split into three single-vote cells.
        let mut idx = InMemoryIndex::new();
        idx.add_recording(RecordingId::new(0), &[(1, 110), (2, 111), (3, 112)]);
        idx.finalize();

        let q = [
            QueryFp {
                hash: 1,
                anchor_time: 10,
            },
            QueryFp {
                hash: 2,
                anchor_time: 10,
            },
            QueryFp {
                hash: 3,
                anchor_time: 10,
            },
        ];
        let exact = idx.query(
            &q,
            &MatchParams {
                offset_tolerance_frames: 0,
                ..MatchParams::default()
            },
        );
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].inliers, 1);
        assert_eq!(exact[0].offset_frames, 100);

        let params = MatchParams {
            offset_tolerance_frames: 2,
            ..MatchParams::default()
        };
        let tol = idx.query(&q, &params);
        assert_eq!(tol.len(), 1);
        assert_eq!(tol[0].inliers, 3, "jittered votes merge under T=2");
        assert!((tol[0].offset_concentration - 1.0).abs() < 1e-6);
        assert_eq!(tol[0].offset_frames, 100); // dominant exact offset
    }

    #[test]
    fn margin_reflects_competitor_gap() {
        let mut idx = InMemoryIndex::new();
        // rec 0 matches two distinct hashes; rec 1 matches one of them.
        idx.add_recording(RecordingId::new(0), &[(1, 50), (2, 52)]);
        idx.add_recording(RecordingId::new(1), &[(1, 90)]);
        idx.finalize();
        let q = [
            QueryFp {
                hash: 1,
                anchor_time: 0,
            },
            QueryFp {
                hash: 2,
                anchor_time: 0,
            },
        ];
        let out = idx.query(&q, &MatchParams::default());
        assert!(out[0].margin_over_next > 1.5, "two votes vs one");
        assert_eq!(out[1].margin_over_next, 1.0, "no competitor below rank-2");

        let solo = idx.query(
            &[QueryFp {
                hash: 2,
                anchor_time: 0,
            }],
            &MatchParams::default(),
        );
        assert_eq!(
            solo[0].margin_over_next, 1.0,
            "single candidate has no rival"
        );
    }

    /// Calibration features (robustness contract item 2): the outcome must
    /// distinguish "one hash repeated" from "many unique hashes spread over
    /// the query timeline" — raw inlier counts cannot.
    #[test]
    fn calibration_features_measure_uniqueness_and_span() {
        let mut idx = InMemoryIndex::new();
        // Recording 0: five distinct hashes aligned at offset +100.
        idx.add_recording(
            RecordingId::new(0),
            &[(1, 100), (2, 102), (3, 104), (4, 106), (5, 108)],
        );
        idx.finalize();

        // Query: those five hashes at consecutive anchors -> all five are
        // inliers spanning 8 frames of query time.
        let q: Vec<QueryFp> = (0..5u32)
            .map(|i| QueryFp {
                hash: i + 1,
                anchor_time: i * 2,
            })
            .collect();
        let out = idx.query(&q, &MatchParams::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].unique_aligned, 5);
        assert_eq!(out[0].query_span_frames, 8);
        assert!(out[0].inliers >= 5);
        assert!(
            out[0].mean_rarity > 0.5,
            "single-recording catalog keeps rarity above the epsilon floor: {}",
            out[0].mean_rarity
        );

        // Now a degenerate case in a fresh index: ONE hash repeated at many
        // query times. Inliers rise with every repetition; uniqueness must
        // not.
        let mut rep = InMemoryIndex::new();
        rep.add_recording(RecordingId::new(1), &[(9, 500), (9, 600), (9, 700)]);
        rep.add_recording(RecordingId::new(2), &[(9, 900)]);
        rep.finalize();
        let q_rep: Vec<QueryFp> = (0..4u32)
            .map(|i| QueryFp {
                hash: 9,
                anchor_time: i,
            })
            .collect();
        let out_rep = rep.query(&q_rep, &MatchParams::default());
        let rec2 = out_rep.iter().find(|o| o.recording.as_u32() == 2).unwrap();
        assert_eq!(
            rec2.unique_aligned, 1,
            "repeated hash must not inflate uniqueness"
        );
    }

    #[test]
    fn matching_is_deterministic_across_builds() {
        let build = || {
            let mut idx = InMemoryIndex::new();
            idx.add_recording(
                RecordingId::new(0),
                &[(1, 10), (1, 20), (2, 15), (3, 16), (4, 40)],
            );
            idx.add_recording(RecordingId::new(1), &[(1, 10), (2, 25), (3, 26)]);
            idx.add_recording(RecordingId::new(2), &[(1, 90), (4, 95)]);
            idx.finalize();
            idx
        };
        let q = [
            QueryFp {
                hash: 1,
                anchor_time: 0,
            },
            QueryFp {
                hash: 2,
                anchor_time: 0,
            },
            QueryFp {
                hash: 3,
                anchor_time: 0,
            },
            QueryFp {
                hash: 4,
                anchor_time: 0,
            },
        ];
        let a = build().query(&q, &MatchParams::default());
        let b = build().query(&q, &MatchParams::default());
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.recording, y.recording);
            assert_eq!(x.offset_frames, y.offset_frames);
            assert_eq!(x.weighted_score.to_bits(), y.weighted_score.to_bits());
        }
    }
}
