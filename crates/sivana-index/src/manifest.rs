//! Catalog manifest: the atomic swap point of the segment set (§21).
//!
//! A catalog directory holds immutable `*.siv` segments plus a
//! `manifest.json` listing the active set. Writers produce a new manifest
//! atomically (write temp + rename), so query servers either see the old
//! catalog or the new one, never a mix; rollback is re-writing the
//! previous manifest.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sivana_core::FingerprintVersion;

pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Monotonic catalog revision; bumped on every swap.
    pub catalog_version: u64,
    /// Fingerprint format every listed segment was built with.
    #[serde(flatten)]
    pub fingerprint_version: FingerprintVersionFlatten,
    /// Active segment file names (relative to the catalog directory),
    /// oldest first.
    pub segments: Vec<String>,
}

/// Serde-friendly flattened version pair.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FingerprintVersionFlatten {
    pub fingerprint_major: u16,
    pub fingerprint_minor: u16,
}

impl Manifest {
    pub fn new(
        catalog_version: u64,
        fp_version: FingerprintVersion,
        segments: Vec<String>,
    ) -> Self {
        Self {
            catalog_version,
            fingerprint_version: FingerprintVersionFlatten {
                fingerprint_major: fp_version.major,
                fingerprint_minor: fp_version.minor,
            },
            segments,
        }
    }

    pub fn fingerprint_version(&self) -> FingerprintVersion {
        FingerprintVersion::new(
            self.fingerprint_version.fingerprint_major,
            self.fingerprint_version.fingerprint_minor,
        )
    }
}

/// Load `dir/manifest.json`.
pub fn load(dir: &Path) -> io::Result<Manifest> {
    let bytes = std::fs::read(dir.join(MANIFEST_FILE))?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Atomically install a manifest into `dir`: serialize to
/// `.manifest.json.tmp` then rename over the live file (rename is atomic
/// on POSIX and Windows same-volume).
pub fn store_atomic(dir: &Path, manifest: &Manifest) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(".manifest.json.tmp");
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(MANIFEST_FILE))
}

/// An open catalog: the active segments, memory-mapped.
pub struct Catalog {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub segments: Vec<super::segment::SivSegment>,
}

impl Catalog {
    /// Open every manifest-listed segment; fails if any is corrupt or
    /// built with a different fingerprint version than the manifest claims.
    pub fn open(dir: &Path) -> Result<Self, super::segment::OpenError> {
        let manifest = load(dir).map_err(super::segment::OpenError::Io)?;
        let claimed = manifest.fingerprint_version();
        let mut segments = Vec::with_capacity(manifest.segments.len());
        for name in &manifest.segments {
            let seg = super::segment::SivSegment::open(&dir.join(name))?;
            if seg.header.fingerprint_version != claimed {
                return Err(super::segment::OpenError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "segment {name} fingerprint v{}.{}, manifest v{}.{}",
                        seg.header.fingerprint_version.major,
                        seg.header.fingerprint_version.minor,
                        claimed.major,
                        claimed.minor
                    ),
                )));
            }
            segments.push(seg);
        }
        Ok(Self {
            manifest,
            dir: dir.to_path_buf(),
            segments,
        })
    }

    /// Union posting list for `hash` across all active segments.
    /// `out` is cleared first; allocation only on result growth.
    pub fn lookup(&self, hash: u32, out: &mut Vec<super::segment::Posting>) {
        out.clear();
        let mut one = Vec::new();
        for seg in &self.segments {
            if seg.lookup(hash, &mut one) {
                out.extend_from_slice(&one);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{Posting, SegmentBuilder};
    use sivana_core::RecordingId;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sivana-manifest-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn multi_segment_catalog_unions_postings() {
        let dir = temp_dir("union");

        // Segment 0: rec 0; segment 1: rec 1 — same hash in both.
        let mut b0 = SegmentBuilder::new();
        b0.add_recording(RecordingId::new(0), &[(77, 10)]);
        let s0 = dir.join("catalog-000001.siv");
        b0.build(&s0, FingerprintVersion::LEGACY).unwrap();

        let mut b1 = SegmentBuilder::new();
        b1.add_recording(RecordingId::new(1), &[(77, 20)]);
        let s1 = dir.join("catalog-000002.siv");
        b1.build(&s1, FingerprintVersion::LEGACY).unwrap();

        // Manifest lists both; swap it in atomically.
        let m = Manifest::new(
            1,
            FingerprintVersion::LEGACY,
            vec![
                s0.file_name().unwrap().to_string_lossy().into_owned(),
                s1.file_name().unwrap().to_string_lossy().into_owned(),
            ],
        );
        store_atomic(&dir, &m).unwrap();

        let cat = Catalog::open(&dir).unwrap();
        assert_eq!(cat.manifest.catalog_version, 1);
        let mut out = Vec::new();
        cat.lookup(77, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&Posting {
            recording: RecordingId::new(0),
            anchor_time: 10
        }));
        assert!(out.contains(&Posting {
            recording: RecordingId::new(1),
            anchor_time: 20
        }));
    }

    #[test]
    fn rollback_restores_previous_catalog() {
        let dir = temp_dir("rollback");
        let mut b = SegmentBuilder::new();
        b.add_recording(RecordingId::new(0), &[(5, 5)]);
        let s0 = dir.join("catalog-000001.siv");
        b.build(&s0, FingerprintVersion::LEGACY).unwrap();
        let old = Manifest::new(
            1,
            FingerprintVersion::LEGACY,
            vec![s0.file_name().unwrap().to_string_lossy().into_owned()],
        );
        store_atomic(&dir, &old).unwrap();

        // "Deploy" a new revision adding a second segment.
        let mut b2 = SegmentBuilder::new();
        b2.add_recording(RecordingId::new(1), &[(6, 6)]);
        let s1 = dir.join("catalog-000002.siv");
        b2.build(&s1, FingerprintVersion::LEGACY).unwrap();
        let new_manifest = Manifest::new(
            2,
            FingerprintVersion::LEGACY,
            vec![
                s0.file_name().unwrap().to_string_lossy().into_owned(),
                s1.file_name().unwrap().to_string_lossy().into_owned(),
            ],
        );
        store_atomic(&dir, &new_manifest).unwrap();
        assert_eq!(Catalog::open(&dir).unwrap().segments.len(), 2);

        // Rollback: rewrite the old manifest; queries see one segment again.
        store_atomic(&dir, &old).unwrap();
        let rolled = Catalog::open(&dir).unwrap();
        assert_eq!(rolled.segments.len(), 1);
        assert_eq!(rolled.manifest.catalog_version, 1);
    }

    #[test]
    fn version_mismatch_between_manifest_and_segment_fails() {
        let dir = temp_dir("mismatch");
        let mut b = SegmentBuilder::new();
        b.add_recording(RecordingId::new(0), &[(9, 9)]);
        let s0 = dir.join("seg.siv");
        b.build(&s0, FingerprintVersion::LEGACY).unwrap();
        // Claim a different fingerprint version than the segment has.
        let m = Manifest::new(
            1,
            FingerprintVersion::LANDMARK_V2_32BIT,
            vec![s0.file_name().unwrap().to_string_lossy().into_owned()],
        );
        store_atomic(&dir, &m).unwrap();
        assert!(Catalog::open(&dir).is_err());
    }
}
