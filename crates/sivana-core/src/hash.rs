//! Compact hash packing (research/PLAN.md §13, §19, §20).
//!
//! The legacy prototype stores 28-bit hashes inside `u64` values. The
//! production index targets a true 32-bit layout:
//!
//! ```text
//! bit 31..20 : f1 (quantized anchor frequency bin)
//! bit 19..8  : f2 (quantized target frequency bin)
//! bit  7..0  : delta-t in frames
//! ```
//!
//! A 32-bit hash enables the high-16 bucket directory of §19: the top
//! half selects one of 65,536 buckets, binary search on the low half.

use serde::{Deserialize, Serialize};

pub const HASH32_F_BITS: u32 = 12;
pub const HASH32_DT_BITS: u32 = 8;
pub const HASH32_F_MAX: u16 = ((1u32 << HASH32_F_BITS) - 1) as u16;
pub const HASH32_DT_MAX: u8 = ((1u32 << HASH32_DT_BITS) - 1) as u8;

/// A packed 32-bit landmark hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hash32(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hash32Parts {
    pub f1: u16,
    pub f2: u16,
    pub dt: u8,
}

/// Pack `(f1, f2, dt)` into a 32-bit hash. Inputs are masked to their
/// field widths so callers never need pre-clipped values.
pub fn pack_hash32(f1: u16, f2: u16, dt: u8) -> Hash32 {
    let f1 = (f1 & HASH32_F_MAX) as u32;
    let f2 = (f2 & HASH32_F_MAX) as u32;
    let dt = (dt & HASH32_DT_MAX) as u32;
    Hash32((f1 << (HASH32_F_BITS + HASH32_DT_BITS)) | (f2 << HASH32_DT_BITS) | dt)
}

/// Inverse of [`pack_hash32`] (masking makes it lossy for out-of-range input).
pub fn unpack_hash32(hash: Hash32) -> Hash32Parts {
    Hash32Parts {
        f1: ((hash.0 >> (HASH32_F_BITS + HASH32_DT_BITS)) & HASH32_F_MAX as u32) as u16,
        f2: ((hash.0 >> HASH32_DT_BITS) & HASH32_F_MAX as u32) as u16,
        dt: (hash.0 & HASH32_DT_MAX as u32) as u8,
    }
}

impl Hash32 {
    /// High 16 bits — direct bucket index for the mmap directory (§19).
    pub fn high16(self) -> u16 {
        (self.0 >> 16) as u16
    }

    /// Low 16 bits — searched within a bucket.
    pub fn low16(self) -> u16 {
        self.0 as u16
    }
}

/// Pack a legacy-prototype hash exactly like `legacy::hashing` does
/// (`(f1:10, f2:10, dt:8)` in the low 28 bits of a u64). Used by the
/// benchmark harness so new and old code can be compared without touching
/// the frozen implementation.
pub fn pack_legacy_hash(f1: u64, f2: u64, dt: u64) -> u64 {
    const F_BITS: u64 = 10;
    const DT_BITS: u64 = 8;
    let f1_masked = f1 & ((1 << F_BITS) - 1);
    let f2_masked = f2 & ((1 << F_BITS) - 1);
    let dt_masked = dt & ((1 << DT_BITS) - 1);
    (f1_masked << (F_BITS + DT_BITS)) | (f2_masked << DT_BITS) | dt_masked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        let cases = [(0, 0, 0), (4095, 4095, 255), (1234, 3456, 42), (1, 2, 3)];
        for (f1, f2, dt) in cases {
            let packed = pack_hash32(f1, f2, dt);
            let parts = unpack_hash32(packed);
            assert_eq!((parts.f1, parts.f2, parts.dt), (f1, f2, dt));
        }
    }

    #[test]
    fn masking_prevents_overflow() {
        let packed = pack_hash32(u16::MAX, u16::MAX, u8::MAX);
        let parts = unpack_hash32(packed);
        assert_eq!(parts.f1, HASH32_F_MAX);
        assert_eq!(parts.f2, HASH32_F_MAX);
        assert_eq!(parts.dt, HASH32_DT_MAX);
    }

    #[test]
    fn high_low_split_reconstructs() {
        let h = pack_hash32(3000, 100, 77);
        assert_eq!(h.high16(), (h.0 >> 16) as u16);
        assert_eq!((h.high16() as u32) << 16 | h.low16() as u32, h.0);
    }

    #[test]
    fn legacy_layout_matches_frozen_code() {
        // Mirrors hashing.rs lines 58-64 of the original prototype.
        let f1 = 500u64;
        let f2 = 250u64;
        let dt = 30u64;
        let expected = (500 << 18) | (250 << 8) | 30;
        assert_eq!(pack_legacy_hash(f1, f2, dt), expected);
    }
}
