//! Sivana benchmark platform (research/PLAN.md §55, §78).
//!
//! One command compares engines over degraded queries:
//!
//! ```text
//! cargo run -p sivana-bench --release -- run
//! ```
//!
//! The runner drives the frozen legacy implementation as the control,
//! applies deterministic degradations to seeded synthetic tracks, and
//! emits JSON + markdown reports measuring recall@1, rejection and
//! latency. New engines plug in beside `legacy` as they are built.

pub mod corpus;
pub mod degradations;
pub mod report;
pub mod runner;
