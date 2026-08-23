//! Sivana DSP primitives.
//!
//! Small, deterministic building blocks shared by the benchmark
//! degradations today and the streaming engine (Phase 1) tomorrow.
//! No allocations inside per-sample loops; every function is pure.

pub mod filter;
pub mod level;
pub mod noise;
pub mod peaks_v2;
pub mod resample;
pub mod sliding_max;
pub mod stft;
pub mod window;
pub mod wsola;

pub use peaks_v2::{Peak, PeakStreamer};
