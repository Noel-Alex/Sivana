//! The custom memory-mapped `.siv` segment format (PLAN §18-§21,
//! index-format/SPEC.md).
//!
//! File layout (all little-endian):
//!
//! ```text
//! ┌─ header (44 B) ───────────────────────────────────────────────┐
//! │ magic "SIV1" │ fmt_ver u32 │ fp_ver u32 │ n_recs u64        │
//! │ n_hashes u64 │ n_postings u64 │ checksum u32 (FNV-1a)       │
//! ├─ bucket directory (65,537 × u64) ─────────────────────────────┤
//! │ dir[b]   = absolute offset of the first hash entry whose      │
//! │            high16 >= b (suffix minimum over entries)          │
//! │ dir[2^16] = one-past-the-end sentinel                         │
//! ├─ hash entries (12 B each, sorted by full u32 hash) ───────────┤
//! │ hash_low16 u16 │ postings_off u40 │ doc_freq u24 │ pad u16   │
//! ├─ postings (8 B each) ────────────────────────────────────────┤
//! │ recording_id u32 │ anchor_time u24 │ flags u8                │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! Lookup: `hash → dir[high] .. dir[high+1] entry range → binary search
//! low16 → contiguous posting run`. The suffix-min directory makes empty
//! buckets collapse to empty ranges automatically. Everything is
//! zero-copy over the mmap except the caller's result buffer; the OS page
//! cache serves repeated queries.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sivana_core::{FingerprintVersion, RecordingId};

/// One indexed posting, unpacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posting {
    pub recording: RecordingId,
    pub anchor_time: u32,
}

/// Bump when the on-disk layout changes.
pub const INDEX_FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"SIV1";
const HEADER_LEN: usize = 44;
const DIR_SLOTS: usize = 65_537;
const DIR_LEN: usize = DIR_SLOTS * 8;
const ENTRY_LEN: usize = 12;
const POSTING_LEN: usize = 8;

#[derive(Debug, Clone, Copy)]
pub struct SegmentHeader {
    pub index_format_version: u32,
    pub fingerprint_version: FingerprintVersion,
    pub recording_count: u64,
    pub hash_count: u64,
    pub posting_count: u64,
}

// ---- packing helpers (shared shape with the LMDB backend) ----

/// Pack a posting into its 8-byte on-disk form: rec u32 | t u24 | flags u8.
pub fn pack_posting(recording: RecordingId, anchor_time: u32, flags: u8) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&recording.as_u32().to_le_bytes());
    out[4] = ((anchor_time >> 16) & 0xFF) as u8;
    out[5] = ((anchor_time >> 8) & 0xFF) as u8;
    out[6] = (anchor_time & 0xFF) as u8;
    out[7] = flags;
    out
}

fn unpack_posting(bytes: &[u8]) -> Posting {
    let rec = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let t = ((bytes[4] as u32) << 16) | ((bytes[5] as u32) << 8) | bytes[6] as u32;
    Posting {
        recording: RecordingId::new(rec),
        anchor_time: t,
    }
}

pub(crate) fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Checksum over a segment body (directory + entries + postings),
/// matching [`SegmentBuilder::build`]'s composition. `hash_count` is read
/// from the header bytes at [20..28].
fn body_checksum(map: &[u8]) -> u32 {
    let dir_end = HEADER_LEN + DIR_LEN;
    let entries_end =
        dir_end + (u64::from_le_bytes(map[20..28].try_into().unwrap()) as usize) * ENTRY_LEN;
    let mut h = fnv1a(&map[HEADER_LEN..dir_end]);
    h = h.wrapping_mul(0x0100_0193);
    h ^= fnv1a(&map[dir_end..entries_end]);
    h = h.wrapping_mul(0x0100_0193);
    h ^= fnv1a(&map[entries_end..]);
    h
}

fn put_u40(buf: &mut [u8], v: u64) {
    buf[0] = v as u8;
    buf[1] = (v >> 8) as u8;
    buf[2] = (v >> 16) as u8;
    buf[3] = (v >> 24) as u8;
    buf[4] = (v >> 32) as u8;
}

fn get_u40(buf: &[u8]) -> u64 {
    buf[0] as u64
        | (buf[1] as u64) << 8
        | (buf[2] as u64) << 16
        | (buf[3] as u64) << 24
        | (buf[4] as u64) << 32
}

fn put_u24(buf: &mut [u8], v: u64) {
    buf[0] = v as u8;
    buf[1] = (v >> 8) as u8;
    buf[2] = (v >> 16) as u8;
}

fn get_u24(buf: &[u8]) -> u64 {
    buf[0] as u64 | (buf[1] as u64) << 8 | (buf[2] as u64) << 16
}

/// Build statistics returned by [`SegmentBuilder::build`].
#[derive(Debug, Clone)]
pub struct BuildStats {
    pub path: PathBuf,
    pub recordings: usize,
    pub hashes: usize,
    pub postings: usize,
    pub bytes: usize,
}

/// Accumulates recordings and serializes a `.siv` segment.
#[derive(Default)]
pub struct SegmentBuilder {
    /// hash -> (recording, anchor) pairs, order arbitrary until build.
    postings: HashMap<u32, Vec<Posting>>,
    recordings: std::collections::BTreeSet<u32>,
}

impl SegmentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_recording(&mut self, recording: RecordingId, fps: &[(u32, u32)]) {
        self.recordings.insert(recording.as_u32());
        for (hash, anchor) in fps {
            self.postings.entry(*hash).or_default().push(Posting {
                recording,
                anchor_time: *anchor,
            });
        }
    }

    /// Add one posting directly — used by compaction when replaying
    /// existing segments.
    pub fn add_posting(&mut self, hash: u32, recording: RecordingId, anchor_time: u32) {
        self.recordings.insert(recording.as_u32());
        self.postings.entry(hash).or_default().push(Posting {
            recording,
            anchor_time,
        });
    }

    /// Serialize to `path` (atomically: write `<path>.tmp`, then rename).
    ///
    /// Postings per hash are sorted and deduplicated in place; document
    /// frequency counts distinct recordings.
    pub fn build(&mut self, path: &Path, fp_version: FingerprintVersion) -> io::Result<BuildStats> {
        let mut hashes: Vec<u32> = self.postings.keys().copied().collect();
        hashes.sort_unstable();

        // Deduped posting lists, serialized in hash order; record each
        // hash's run offset.
        let total_postings: usize = hashes.iter().map(|&h| self.postings[&h].len()).sum();
        let entries_len = hashes.len() * ENTRY_LEN;
        let entries_base = (HEADER_LEN + DIR_LEN) as u64;
        let postings_base = entries_base + entries_len as u64;

        let mut entries = vec![0u8; entries_len];
        let mut postings_buf = Vec::with_capacity(total_postings * POSTING_LEN);
        // Run boundaries: postings_off per entry index (+ final end).
        let mut run_offsets = Vec::with_capacity(hashes.len() + 1);

        for (i, &h) in hashes.iter().enumerate() {
            let plist = self.postings.get_mut(&h).expect("hash from keys map");
            plist.sort_by_key(|p| (p.recording.as_u32(), p.anchor_time));
            plist.dedup();
            run_offsets.push(postings_base + postings_buf.len() as u64);

            let e = &mut entries[i * ENTRY_LEN..(i + 1) * ENTRY_LEN];
            e[..2].copy_from_slice(&((h & 0xFFFF) as u16).to_le_bytes());
            put_u40(&mut e[2..7], postings_base + postings_buf.len() as u64);
            // Document frequency: distinct recordings in the deduped list.
            let mut df = usize::from(!plist.is_empty());
            for w in plist.windows(2) {
                if w[1].recording != w[0].recording {
                    df += 1;
                }
            }
            put_u24(&mut e[7..10], df.min(0xFF_FFFF) as u64);
            e[10..12].copy_from_slice(&0u16.to_le_bytes());

            for p in plist.iter() {
                postings_buf.extend_from_slice(&pack_posting(p.recording, p.anchor_time, 0));
            }
        }
        run_offsets.push(postings_base + postings_buf.len() as u64);

        // --- Bucket directory via suffix minimum ---
        // dir[b] = first entry offset whose high16 >= b, else end-of-entries.
        let entries_end = entries_base + entries_len as u64;
        let mut dir_vals = vec![entries_end; DIR_SLOTS];
        for (i, &h) in hashes.iter().enumerate() {
            let b = (h >> 16) as usize;
            let off = entries_base + (i * ENTRY_LEN) as u64;
            dir_vals[b] = dir_vals[b].min(off);
        }
        for b in (0..DIR_SLOTS - 1).rev() {
            dir_vals[b] = dir_vals[b].min(dir_vals[b + 1]);
        }
        debug_assert_eq!(dir_vals[DIR_SLOTS - 1], entries_end);
        let mut dir = vec![0u8; DIR_LEN];
        for (b, v) in dir_vals.iter().enumerate() {
            dir[b * 8..b * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }

        // --- Header (checksum covers directory + entries + postings) ---
        let checksum = {
            let mut h = fnv1a(&dir);
            h = h.wrapping_mul(0x0100_0193); // separator-free chaining: keep it simple but cover both parts
            h ^= fnv1a(&entries);
            h = h.wrapping_mul(0x0100_0193);
            h ^= fnv1a(&postings_buf);
            h
        };

        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(MAGIC);
        header[4..8].copy_from_slice(&INDEX_FORMAT_VERSION.to_le_bytes());
        header[8..10].copy_from_slice(&fp_version.major.to_le_bytes());
        header[10..12].copy_from_slice(&fp_version.minor.to_le_bytes());
        header[12..20].copy_from_slice(&(self.recordings.len() as u64).to_le_bytes());
        header[20..28].copy_from_slice(&(hashes.len() as u64).to_le_bytes());
        header[28..36].copy_from_slice(&(total_postings as u64).to_le_bytes());
        header[36..40].copy_from_slice(&checksum.to_le_bytes());
        // [40..44] reserved.

        // Atomic-ish write: temp file + rename.
        let tmp = path.with_extension("siv.tmp");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        {
            let mut f = io::BufWriter::new(std::fs::File::create(&tmp)?);
            f.write_all(&header)?;
            f.write_all(&dir)?;
            f.write_all(&entries)?;
            f.write_all(&postings_buf)?;
            f.flush()?;
            f.into_inner()?.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;

        Ok(BuildStats {
            path: path.to_path_buf(),
            recordings: self.recordings.len(),
            hashes: hashes.len(),
            postings: total_postings,
            bytes: HEADER_LEN + DIR_LEN + entries_len + postings_buf.len(),
        })
    }
}

#[derive(Debug)]
pub enum OpenError {
    Io(io::Error),
    BadMagic,
    UnsupportedFormat(u32),
    ChecksumMismatch { expected: u32, found: u32 },
}

impl From<io::Error> for OpenError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::BadMagic => write!(f, "not a .siv segment"),
            Self::UnsupportedFormat(v) => write!(f, "unsupported index format v{v}"),
            Self::ChecksumMismatch { expected, found } => {
                write!(
                    f,
                    "corrupt segment: checksum {found:#010x}, expected {expected:#010x}"
                )
            }
        }
    }
}

impl std::error::Error for OpenError {}

/// An opened, memory-mapped `.siv` segment.
pub struct SivSegment {
    map: memmap2::Mmap,
    pub header: SegmentHeader,
}

impl SivSegment {
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: read-only mapping; no mutable aliases can exist through
        // this handle.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        if map.len() < HEADER_LEN + DIR_LEN {
            return Err(OpenError::BadMagic);
        }
        if &map[..4] != MAGIC {
            return Err(OpenError::BadMagic);
        }
        let ver = u32::from_le_bytes(map[4..8].try_into().unwrap());
        if ver != INDEX_FORMAT_VERSION {
            return Err(OpenError::UnsupportedFormat(ver));
        }
        let header = SegmentHeader {
            index_format_version: ver,
            fingerprint_version: FingerprintVersion::new(
                u16::from_le_bytes(map[8..10].try_into().unwrap()),
                u16::from_le_bytes(map[10..12].try_into().unwrap()),
            ),
            recording_count: u64::from_le_bytes(map[12..20].try_into().unwrap()),
            hash_count: u64::from_le_bytes(map[20..28].try_into().unwrap()),
            posting_count: u64::from_le_bytes(map[28..36].try_into().unwrap()),
        };
        // Header-derived lengths are untrusted: a corrupt or hostile count
        // must be rejected before any derived slice indexes past the
        // mapping, and the multiply itself must not overflow (found by the
        // fuzz-lite sweep). The entries region must fit inside the file.
        let entries_len = header
            .hash_count
            .checked_mul(ENTRY_LEN as u64)
            .ok_or(OpenError::BadMagic)?;
        let entries_end = HEADER_LEN + DIR_LEN + entries_len as usize;
        if entries_end > map.len() {
            return Err(OpenError::BadMagic);
        }
        let expected = u32::from_le_bytes(map[36..40].try_into().unwrap());
        let found = body_checksum(&map);
        if found != expected {
            return Err(OpenError::ChecksumMismatch { expected, found });
        }
        Ok(Self { map, header })
    }

    fn dir_val(&self, slot: usize) -> usize {
        let o = HEADER_LEN + slot * 8;
        u64::from_le_bytes(self.map[o..o + 8].try_into().unwrap()) as usize
    }

    /// Byte offset of the entry region start.
    fn entries_base(&self) -> usize {
        HEADER_LEN + DIR_LEN
    }

    /// Binary search within the bucket for `hash`; returns
    /// (entry_byte_offset, postings_start, doc_freq).
    fn find_entry(&self, hash: u32) -> Option<(usize, usize, u64)> {
        let high = (hash >> 16) as usize;
        let low = hash & 0xFFFF;
        let base = self.entries_base();
        let mut lo = self.dir_val(high) - base;
        let mut hi = self.dir_val(high + 1) - base;
        let n_entries = self.header.hash_count as usize;

        while lo < hi && lo / ENTRY_LEN < n_entries {
            let mid = lo + ((hi - lo) / ENTRY_LEN) / 2 * ENTRY_LEN;
            let e = &self.map[base + mid..base + mid + ENTRY_LEN];
            let mid_low = u16::from_le_bytes(e[..2].try_into().unwrap()) as u32;
            match mid_low.cmp(&low) {
                std::cmp::Ordering::Equal => {
                    return Some((mid, get_u40(&e[2..7]) as usize, get_u24(&e[7..10])));
                }
                std::cmp::Ordering::Less => lo = mid + ENTRY_LEN,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// Document frequency of `hash` (distinct recordings containing it).
    pub fn document_frequency(&self, hash: u32) -> Option<u64> {
        let (_, _, df) = self.find_entry(hash)?;
        Some(df)
    }

    /// Every hash in the segment, ascending. Used by compaction.
    ///
    /// Entries inside `dir[b] .. dir[b+1]` all have high16 == b exactly,
    /// so walking the directory reconstructs full hashes without storing
    /// them twice.
    pub fn all_hashes(&self) -> Vec<u32> {
        let base = self.entries_base();
        let limit = (self.header.hash_count as usize) * ENTRY_LEN;
        let mut out = Vec::with_capacity(self.header.hash_count as usize);
        for b in 0..DIR_SLOTS - 1 {
            let start = self.dir_val(b) - base;
            if start >= limit {
                break; // no entries remain at or after this bucket
            }
            let end = (self.dir_val(b + 1) - base).min(limit);
            let mut off = start;
            while off < end {
                let e = &self.map[base + off..base + off + ENTRY_LEN];
                let low = u16::from_le_bytes(e[..2].try_into().unwrap()) as u32;
                out.push(((b as u32) << 16) | low);
                off += ENTRY_LEN;
            }
        }
        out
    }

    /// Collect every posting for `hash` into `out` (cleared first).
    /// Returns false when absent.
    pub fn lookup(&self, hash: u32, out: &mut Vec<Posting>) -> bool {
        out.clear();
        let Some((entry_off, mut cursor, _)) = self.find_entry(hash) else {
            return false;
        };
        // Runs are disjoint and ordered by entry, so this run ends where
        // the next entry's postings begin (or at EOF for the last entry).
        let next_entry = entry_off + ENTRY_LEN;
        let base = self.entries_base();
        let n_entries = self.header.hash_count as usize;
        let next_off = if next_entry / ENTRY_LEN < n_entries {
            let e = &self.map[base + next_entry..base + next_entry + ENTRY_LEN];
            (get_u40(&e[2..7]) as usize).min(self.map.len())
        } else {
            self.map.len()
        };
        while cursor + POSTING_LEN <= next_off {
            out.push(unpack_posting(&self.map[cursor..cursor + POSTING_LEN]));
            cursor += POSTING_LEN;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sivana_audio::rng::XorShift64Star;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sivana-index-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip_matches_hash_map_oracle() {
        let mut rng = XorShift64Star::new(2024);
        let mut b = SegmentBuilder::new();
        let mut oracle: HashMap<u32, Vec<Posting>> = HashMap::new();
        for rec in 0..8u32 {
            let fps: Vec<(u32, u32)> = (0..500)
                .map(|_| {
                    (
                        (rng.next_f32() * 1e9) as u32,
                        (rng.next_f32() * 20_000.0) as u32,
                    )
                })
                .collect();
            b.add_recording(RecordingId::new(rec), &fps);
            for (h, t) in &fps {
                oracle.entry(*h).or_default().push(Posting {
                    recording: RecordingId::new(rec),
                    anchor_time: *t,
                });
            }
        }

        let dir = temp_dir("roundtrip");
        let path = dir.join("seg.siv");
        let stats = b
            .build(&path, FingerprintVersion::LANDMARK_V2_32BIT)
            .unwrap();
        assert_eq!(stats.hashes, oracle.len());

        let seg = SivSegment::open(&path).unwrap();
        assert_eq!(
            seg.header.fingerprint_version,
            FingerprintVersion::LANDMARK_V2_32BIT
        );
        assert_eq!(seg.header.recording_count, 8);

        let mut out = Vec::new();
        for (&h, want) in &oracle {
            assert!(seg.lookup(h, &mut out), "hash {h} missing");
            let mut got: Vec<(u32, u32)> = out
                .iter()
                .map(|p| (p.recording.as_u32(), p.anchor_time))
                .collect();
            got.sort_unstable();
            let mut want: Vec<(u32, u32)> = want
                .iter()
                .map(|p| (p.recording.as_u32(), p.anchor_time))
                .collect();
            want.sort_unstable();
            want.dedup();
            assert_eq!(got, want, "postings mismatch for hash {h}");
        }
        // Unknown hash misses cleanly.
        assert!(!seg.lookup(u32::MAX - 1, &mut out));
    }

    #[test]
    fn empty_buckets_and_extreme_hashes() {
        // Deliberately sparse high16 coverage with extreme values.
        let mut b = SegmentBuilder::new();
        let cases: [(u32, u32); 5] = [
            (0x0000_0001, 7),
            (0x0001_0000, 9),
            (0xFFFF_0002, 11),
            (0xFFFF_FFFF, 13),
            (0x8000_1234, 17),
        ];
        for (i, &(h, t)) in cases.iter().enumerate() {
            b.add_recording(RecordingId::new(i as u32), &[(h, t)]);
        }
        let dir = temp_dir("sparse");
        let path = dir.join("seg.siv");
        b.build(&path, FingerprintVersion::LEGACY).unwrap();
        let seg = SivSegment::open(&path).unwrap();

        let mut out = Vec::new();
        for (i, &(h, t)) in cases.iter().enumerate() {
            assert!(seg.lookup(h, &mut out), "extreme hash {h:#010x} missing");
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].recording, RecordingId::new(i as u32));
            assert_eq!(out[0].anchor_time, t);
        }
        assert!(!seg.lookup(0xDEAD_BEEF, &mut out));
    }

    #[test]
    fn document_frequency_counts_recordings() {
        let mut b = SegmentBuilder::new();
        b.add_recording(RecordingId::new(0), &[(42, 1), (42, 2)]);
        b.add_recording(RecordingId::new(1), &[(42, 3)]);
        b.add_recording(RecordingId::new(2), &[(43, 4)]);
        let dir = temp_dir("df");
        let path = dir.join("seg.siv");
        b.build(&path, FingerprintVersion::LEGACY).unwrap();
        let seg = SivSegment::open(&path).unwrap();
        assert_eq!(seg.document_frequency(42), Some(2));
        assert_eq!(seg.document_frequency(43), Some(1));
        assert_eq!(seg.document_frequency(999), None);
    }

    #[test]
    fn corrupted_body_is_detected() {
        let mut b = SegmentBuilder::new();
        b.add_recording(RecordingId::new(0), &[(1, 2), (3, 4)]);
        let dir = temp_dir("corrupt");
        let path = dir.join("seg.siv");
        b.build(&path, FingerprintVersion::LEGACY).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        match SivSegment::open(&path).err() {
            Some(OpenError::ChecksumMismatch { .. }) => {}
            other => panic!("expected checksum mismatch, got {other:?}"),
        }
    }

    #[test]
    fn wrong_magic_and_version_rejected() {
        let dir = temp_dir("versions");
        let path = dir.join("bad.siv");
        std::fs::write(&path, b"NOPE0000").unwrap();
        assert!(matches!(SivSegment::open(&path), Err(OpenError::BadMagic)));

        let mut b = SegmentBuilder::new();
        b.add_recording(RecordingId::new(0), &[(5, 5)]);
        let good = dir.join("good.siv");
        b.build(&good, FingerprintVersion::LEGACY).unwrap();
        let mut bytes = std::fs::read(&good).unwrap();
        bytes[4..8].copy_from_slice(&99u32.to_le_bytes());
        // Fix the body checksum so the isolated failure is the version check.
        let cs = body_checksum(&bytes);
        bytes[36..40].copy_from_slice(&cs.to_le_bytes());
        std::fs::write(&good, &bytes).unwrap();
        assert!(matches!(
            SivSegment::open(&good),
            Err(OpenError::UnsupportedFormat(99))
        ));
    }
}

#[cfg(test)]
mod iter_tests {
    use super::*;
    use sivana_audio::rng::XorShift64Star;

    #[test]
    fn all_hashes_reconstructs_every_key() {
        let mut rng = XorShift64Star::new(31415);
        let mut b = SegmentBuilder::new();
        let mut expected = std::collections::BTreeSet::new();
        for rec in 0..4u32 {
            let fps: Vec<(u32, u32)> = (0..300)
                .map(|_| {
                    let h = (rng.next_f32() * 1e9) as u32;
                    expected.insert(h);
                    (h, 5u32)
                })
                .collect();
            b.add_recording(RecordingId::new(rec), &fps);
        }
        let dir = std::env::temp_dir().join("sivana-index-iter");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seg.siv");
        b.build(&path, FingerprintVersion::LEGACY).unwrap();
        let seg = SivSegment::open(&path).unwrap();

        let got: std::collections::BTreeSet<u32> = seg.all_hashes().into_iter().collect();
        assert_eq!(got, expected);

        // Every reported hash must actually resolve.
        let mut out = Vec::new();
        for &h in got.iter().take(100) {
            assert!(seg.lookup(h, &mut out), "hash {h:#010x} not resolvable");
        }
    }
}
