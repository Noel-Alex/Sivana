//! Deterministic audio IO and synthetic fixtures.
//!
//! Benchmarks must be reproducible (research/PLAN.md §36, §55): all
//! randomness comes from a seeded xorshift generator, so the same seed
//! produces bit-identical audio on every platform.

pub mod fixtures;
pub mod rng;
pub mod wav;

pub use rng::XorShift64Star;
