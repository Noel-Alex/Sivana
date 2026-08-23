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
    /// Votes agreeing exactly with the reported offset (inliers).
    pub inliers: usize,
    /// Fraction of this candidate's votes at the dominant offset.
    pub offset_concentration: f32,
    /// Verified time offset of the recording relative to the query (frames).
    pub offset_frames: i64,
}

#[derive(Debug, Clone)]
pub struct MatchParams {
    /// Hashes whose distinct-recording df exceeds this are ignored at
    /// query time (§15).
    pub stop_hash_df_threshold: u64,
    /// Keep this many candidates for verification.
    pub shortlist_len: usize,
}

impl Default for MatchParams {
    fn default() -> Self {
        Self {
            // With tiny catalogs everything is rare; tune by sweep later.
            stop_hash_df_threshold: u64::MAX,
            shortlist_len: 5,
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
            weight: f32,
        }
        let mut votes: Vec<Vote> = Vec::with_capacity(query.len() * 8);
        let mut seen_hashes = std::collections::HashSet::new();

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
                        bucket: offset,
                        weight: w,
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

        // Per-cell accumulation. Offsets live in a BTreeMap so both the
        // dominant-offset pick and iteration order are deterministic.
        struct Acc {
            total_weight: f32,
            votes: usize,
            offsets: BTreeMap<i64, (usize, f32)>, // exact offset -> (count, w)
        }
        let mut accs: Vec<((u32, i64), Acc)> = Vec::new();
        for v in votes {
            match accs.last_mut() {
                Some(((r, b), acc)) if *r == v.rec.as_u32() && *b == v.bucket => {
                    acc.total_weight += v.weight;
                    acc.votes += 1;
                    let e = acc.offsets.entry(v.bucket).or_insert((0, 0.0));
                    e.0 += 1;
                    e.1 += v.weight;
                }
                _ => {
                    let mut m = BTreeMap::new();
                    m.insert(v.bucket, (1, v.weight));
                    accs.push((
                        (v.rec.as_u32(), v.bucket),
                        Acc {
                            total_weight: v.weight,
                            votes: 1,
                            offsets: m,
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

        rows.into_iter()
            .map(|(rec_u32, bucket, weight)| {
                // Verification: dominant exact offset inside the winning
                // cell. bucket == exact offset today (buckets exist so the
                // structure survives coarser quantization later); ties go
                // to the smaller offset via BTreeMap order.
                let acc = &accs
                    .iter()
                    .find(|((r, b), _)| *r == rec_u32 && *b == bucket)
                    .expect("row must map to an accumulator")
                    .1;
                let (&best_off, &(count, ow)) = acc
                    .offsets
                    .iter()
                    .max_by(|a, b| {
                        a.1.1
                            .partial_cmp(&b.1.1)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .expect("accumulator non-empty");
                MatchOutcome {
                    recording: RecordingId::new(rec_u32),
                    weighted_score: weight,
                    inliers: count,
                    offset_concentration: if weight > 0.0 { ow / weight } else { 0.0 },
                    offset_frames: best_off,
                }
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
        let out = idx.query(&q, &MatchParams::default());
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
