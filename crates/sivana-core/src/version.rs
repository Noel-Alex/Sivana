//! Engine and fingerprint format versioning (research/PLAN.md §36).
//!
//! Every fingerprint stream carries these markers so recognition results
//! stay interpretable across catalog rebuilds and platform changes.

use serde::{Deserialize, Serialize};

/// Monotonic fingerprint format version.
///
/// Bump `major` when fingerprints produced by the new code cannot be
/// matched against an existing index; bump `minor` for compatible changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FingerprintVersion {
    pub major: u16,
    pub minor: u16,
}

impl FingerprintVersion {
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Version of the frozen legacy prototype's fingerprint format.
    ///
    /// The legacy hashes are 28-bit values `(f1:10, f2:10, dt:8)`; treat
    /// them as a separate major version from any future 32-bit layout.
    pub const LEGACY: Self = Self::new(0, 1);
}

/// Identifier of the engine that produced a fingerprint stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EngineId {
    /// Frozen prototype control implementation.
    Legacy,
    /// Streaming landmark engine V2 (Phase 1).
    LandmarkV2,
    /// Scale-invariant triplet invariants (Engine B1).
    InvariantTriplets,
    /// Geometric quad invariants (Engine B2).
    InvariantQuads,
}

impl EngineId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::LandmarkV2 => "landmark-v2",
            Self::InvariantTriplets => "invariant-b1-triplets",
            Self::InvariantQuads => "invariant-b2-quads",
        }
    }
}

/// The fingerprint version currently produced by this workspace build.
pub fn current_fingerprint_version() -> FingerprintVersion {
    FingerprintVersion::LEGACY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_version_ordering() {
        assert!(FingerprintVersion::LEGACY < FingerprintVersion::new(1, 0));
        assert!(FingerprintVersion::new(0, 1) < FingerprintVersion::new(0, 2));
    }

    #[test]
    fn engine_ids_are_stable_strings() {
        assert_eq!(EngineId::Legacy.as_str(), "legacy");
    }
}
