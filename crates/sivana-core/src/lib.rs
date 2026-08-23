//! Shared Sivana core types.
//!
//! Everything that must stay consistent across engines, platforms and
//! language boundaries lives here: recording identity, fingerprint format
//! versions, compact hash packing and the algorithm configuration schema.
//!
//! Rule (research/PLAN.md §92): native and WASM code share these exact
//! definitions so implementations cannot drift apart.

pub mod config;
pub mod hash;
pub mod ids;
pub mod version;

pub use config::{AlgorithmConfig, OPERATING_FREQ_BANDS};
pub use hash::{Hash32, pack_hash32, unpack_hash32};
pub use ids::RecordingId;
pub use version::{EngineId, FingerprintVersion, current_fingerprint_version};
