//! Sivana DSP primitives.
//!
//! Small, deterministic building blocks shared by the benchmark
//! degradations today and the streaming engine (Phase 1) tomorrow.
//! No allocations inside per-sample loops; every function is pure.

pub mod filter;
pub mod level;
pub mod noise;
pub mod resample;
pub mod window;
