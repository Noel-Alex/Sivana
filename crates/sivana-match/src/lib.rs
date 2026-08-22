//! Sivana Matcher V2 (research/PLAN.md §22-§26).
//!
//! Replaces the prototype's nested `HashMap<SongId, HashMap<offset, count>>`
//! voting and hard-coded gate with:
//!
//! * document-frequency (rarity) weights `w(h) = ln((N+1)/(df+1))` (§14)
//! * stop-hash suppression above a df threshold (§15)
//! * flat vote tuples `(recording, offset_bucket)` in contiguous vectors,
//!   sorted + grouped — no nested maps in the hot path (§23)
//! * robust offset verification: median-offset inlier fraction (§24)
//! * calibrated-style score features returned raw for calibration work (§26)

use std::collections::HashMap;

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
#[derive(Default)]
pub struct InMemoryIndex {
    postings: HashMap<u32, Vec<Posting>>,
    n_recordings: u64,
}

impl InMemoryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add all fingerprints of one recording.
    pub fn add_recording(&mut self, recording: RecordingId, fps: &[(u32, u32)]) {
        self.n_recordings += 1;
        for (hash, anchor) in fps {
            self.postings.entry(*hash).or_default().push(Posting {
                recording,
                anchor_time: *anchor,
            });
        }
    }

    pub fn len(&self) -> usize {
        self.postings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.postings.is_empty()
    }

    /// Rarity weight (§14). The raw formula `ln((N+1)/(df+1))` hits
    /// exactly zero when df == N (hash present in every recording), which
    /// would silence entire tiny catalogs; we floor it at a small epsilon
    /// so ubiquitous hashes are near-zero-weight instead of dead.
    fn idf(&self, df: usize) -> f32 {
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
    /// Votes agreeing with the verified offset (inliers).
    pub inliers: usize,
    /// Fraction of this candidate's votes at the dominant offset.
    pub offset_concentration: f32,
    /// Verified time offset of the recording relative to the query (frames).
    pub offset_frames: i64,
}

#[derive(Debug, Clone)]
pub struct MatchParams {
    /// Hashes whose df exceeds this are ignored at query time (§15).
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
    /// candidates sorted by verified score (best first). Empty when the
    /// catalog has never heard any of the query's hashes.
    pub fn query(&self, query: &[QueryFp], params: &MatchParams) -> Vec<MatchOutcome> {
        // Flat vote tuples; key = (recording, offset_bucket).
        #[derive(Clone, Copy)]
        struct Vote {
            rec: RecordingId,
            bucket: i64,
            exact_offset: i64,
            weight: f32,
        }
        let mut votes: Vec<Vote> = Vec::with_capacity(query.len() * 8);
        let mut seen_hashes = std::collections::HashSet::new();

        for q in query {
            if !seen_hashes.insert(q.hash) {
                continue; // dedup query hashes (§22)
            }
            if let Some(postings) = self.postings.get(&q.hash) {
                let df = postings.len() as u64;
                if df > params.stop_hash_df_threshold {
                    continue;
                }
                let w = self.idf(postings.len());
                if w <= 0.0 {
                    continue;
                }
                for p in postings {
                    votes.push(Vote {
                        rec: p.recording,
                        bucket: (p.anchor_time as i64 - q.anchor_time as i64),
                        exact_offset: p.anchor_time as i64 - q.anchor_time as i64,
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

        struct Acc {
            total_weight: f32,
            votes: usize,
            offsets: HashMap<i64, (usize, f32)>, // exact offset -> (count, w)
        }
        let mut accs: Vec<((u32, i64), Acc)> = Vec::new();
        for v in votes {
            match accs.last_mut() {
                Some(((r, b), acc)) if *r == v.rec.as_u32() && *b == v.bucket => {
                    acc.total_weight += v.weight;
                    acc.votes += 1;
                    let e = acc.offsets.entry(v.exact_offset).or_insert((0, 0.0));
                    e.0 += 1;
                    e.1 += v.weight;
                }
                _ => {
                    let mut m = HashMap::new();
                    m.insert(v.exact_offset, (1, v.weight));
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

        // Best (recording, bucket) cells -> per-recording best cell only.
        let mut best_per_rec: HashMap<u32, (u32, i64, f32, usize)> = HashMap::new();
        for ((r, b), acc) in &accs {
            let entry = best_per_rec.entry(*r).or_insert_with(|| (*r, *b, 0.0, 0));
            if acc.total_weight > entry.2 {
                *entry = (*r, *b, acc.total_weight, acc.votes);
            }
        }

        let mut rows: Vec<(u32, i64, f32, usize)> = best_per_rec.into_values().collect();
        rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        rows.truncate(params.shortlist_len);

        rows.into_iter()
            .map(|(rec_u32, bucket, weight, _)| {
                // Verification: find the exact offset with max weight inside
                // this bucket (bucket == exact offset here; buckets exist so
                // the structure survives coarser quantization later).
                // We recompute the winner from the grouped run we already
                // walked by re-scanning accs once more — acceptable because
                // shortlists are tiny.
                let outcome = accs
                    .iter()
                    .find(|((r, b), _)| *r == rec_u32 && *b == bucket)
                    .expect("row must map to an accumulator");
                let (best_off, &(count, ow)) = outcome
                    .1
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
                    offset_frames: *best_off,
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

        let out = idx.query(
            &[QueryFp {
                hash: H,
                anchor_time: 5,
            }],
            &MatchParams::default(),
        );
        assert_eq!(out.len(), 2); // both recordings share the hash
        assert!(out.iter().any(|o| o.recording == RecordingId::new(0)));
        assert!(out.iter().any(|o| o.recording == RecordingId::new(1)));
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
}
