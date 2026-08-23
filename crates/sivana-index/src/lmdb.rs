//! Stage 1 LMDB backend via `heed` (PLAN §17).
//!
//! LMDB is the production-capable intermediate store that proves the
//! posting layout against a mature B+tree engine before the custom mmap
//! segments take over. Values use the exact same packed 8-byte posting
//! encoding as the `.siv` format (rec u32 | t u24 | flags u8), so layout
//! learnings transfer between backends.

use std::path::Path;

use heed::types::{Bytes, U32};
use sivana_core::{FingerprintVersion, RecordingId};

use crate::segment::pack_posting;

const ENV_SIZE: usize = 1024 * 1024 * 1024; // 1 GiB upper bound, pages on demand

/// A read-heavy LMDB posting store: hash -> concatenated postings.
pub struct LmdbIndex {
    env: heed::Env,
    db: heed::Database<U32<heed::byteorder::LittleEndian>, Bytes>,
}

impl LmdbIndex {
    pub fn open(path: &Path) -> heed::Result<Self> {
        std::fs::create_dir_all(path)?;
        // Safety: single-process usage today; LMDB itself mediates
        // multi-process readers.
        let env = unsafe {
            heed::EnvOpenOptions::new()
                .map_size(ENV_SIZE)
                .max_dbs(1)
                .open(path)?
        };
        let mut wtxn = env.write_txn()?;
        let db = env.create_database(&mut wtxn, None)?;
        wtxn.commit()?;
        Ok(Self { env, db })
    }

    /// Append all fingerprints of one recording in a single transaction.
    /// Postings are merged with any existing list for shared hashes.
    pub fn add_recording(&self, recording: RecordingId, fps: &[(u32, u32)]) -> heed::Result<()> {
        let mut grouped: std::collections::BTreeMap<u32, Vec<(u32, u32)>> =
            std::collections::BTreeMap::new();
        for (h, t) in fps {
            grouped.entry(*h).or_default().push((*t, *t));
            grouped.get_mut(h).unwrap().last_mut().unwrap().0 = *t;
        }
        let mut wtxn = self.env.write_txn()?;
        for (hash, times) in &grouped {
            let mut value: Vec<u8> = match self.db.get(&wtxn, hash)? {
                Some(existing) => existing.to_vec(),
                None => Vec::new(),
            };
            for _ in 0..times.len() {
                value.extend_from_slice(&pack_posting(recording, times[0].0, 0));
            }
            self.db.put(&mut wtxn, hash, &value)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Collect postings for one hash into `out` (cleared first).
    pub fn lookup(&self, hash: u32, out: &mut Vec<crate::segment::Posting>) -> heed::Result<bool> {
        out.clear();
        let rtxn = self.env.read_txn()?;
        match self.db.get(&rtxn, &hash)? {
            Some(bytes) => {
                for chunk in bytes.chunks_exact(8) {
                    let rec = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    let t = ((chunk[4] as u32) << 16) | ((chunk[5] as u32) << 8) | chunk[6] as u32;
                    out.push(crate::segment::Posting {
                        recording: RecordingId::new(rec),
                        anchor_time: t,
                    });
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn fingerprint_version_note() -> FingerprintVersion {
        FingerprintVersion::LANDMARK_V2_32BIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::Posting;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sivana-lmdb-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lmdb_roundtrip_and_merge() {
        let dir = temp_dir("roundtrip");
        let idx = LmdbIndex::open(&dir).unwrap();
        idx.add_recording(RecordingId::new(0), &[(42, 10)]).unwrap();
        idx.add_recording(RecordingId::new(1), &[(42, 20), (43, 5)])
            .unwrap();

        let mut out = Vec::new();
        assert!(idx.lookup(42, &mut out).unwrap());
        assert_eq!(
            out,
            vec![
                Posting {
                    recording: RecordingId::new(0),
                    anchor_time: 10
                },
                Posting {
                    recording: RecordingId::new(1),
                    anchor_time: 20
                },
            ]
        );
        assert!(idx.lookup(43, &mut out).unwrap());
        assert_eq!(out.len(), 1);
        assert!(!idx.lookup(99, &mut out).unwrap());
        assert!(out.is_empty());

        // Reopening the same directory sees persisted data.
        let reopened = LmdbIndex::open(&dir).unwrap();
        assert!(reopened.lookup(42, &mut out).unwrap());
        assert_eq!(out.len(), 2);
    }
}
