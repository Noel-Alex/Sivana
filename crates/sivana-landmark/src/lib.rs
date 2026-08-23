//! Sivana Landmark Engine V2.
//!
//! Streaming-friendly replacement for the frozen prototype's landmarking:
//!
//! * STFT + PeakDetector V2 from `sivana-dsp`
//! * **scored** target-zone selection: temporal spread across the zone
//!   instead of "first N peaks" (§11)
//! * 32-bit hashes `f1:12 | f2:12 | dt:8`, ready for the bucket directory
//!   index (§13/§19)

pub mod fingerprinter;

pub use fingerprinter::{Fingerprint32, LandmarkStreamer, LandmarkV2Config, fingerprint};
