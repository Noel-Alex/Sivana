//! Recording identity (research/PLAN.md §40).
//!
//! A `RecordingId` identifies one fingerprintable audio artefact. Multiple
//! metadata rows (title/artist/album) may point at the same recording.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RecordingId(u32);

impl RecordingId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for RecordingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rec-{}", self.0)
    }
}

/// Compact posting entry: `(recording_id, anchor_time)` pairs stored in
/// index postings. Kept in core because both the index builder and the
/// matcher depend on this exact layout.
///
/// The planned mmap layout (§20) packs these into a `u64`
/// (`32-bit recording_id | 24-bit anchor_time | 8-bit flags`); until that
/// phase lands we use the explicit struct form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posting {
    pub recording_id: RecordingId,
    pub anchor_time_frames: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_prefixed() {
        assert_eq!(RecordingId::new(42).to_string(), "rec-42");
    }

    #[test]
    fn ordering_supports_sorting() {
        let mut v = [RecordingId::new(7), RecordingId::new(1)];
        v.sort();
        assert_eq!(v[0].as_u32(), 1);
    }
}
