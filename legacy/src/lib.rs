//! Frozen legacy Sivana implementation.
//!
//! This is the original prototype, preserved unmodified as the **control
//! implementation** against which every future engine is measured
//! (see research/PLAN.md §3 and §92).
//!
//! The only additions over the original code are:
//!
//! * this library facade (`lib.rs`) exposing the modules,
//! * a global verbosity switch so benchmark harnesses can silence debug
//!   output without touching algorithmic behaviour,
//! * additive database helpers (`open_db_connection_at`,
//!   `query_db_and_match_with_threshold`) that keep the originals intact.

// Style-only clippy lints are tolerated here: this crate is frozen and must
// remain byte-for-byte equivalent to the original prototype's behaviour.
#![allow(clippy::all)]

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(true);

/// Enable or disable legacy debug printing. Enabled by default so the
/// standalone CLI behaves exactly as it always has.
pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

/// Current verbosity state.
pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Debug print that respects [`set_verbose`].
#[macro_export]
macro_rules! leg_dbg {
    ($($arg:tt)*) => {
        if $crate::is_verbose() {
            println!($($arg)*);
        }
    };
}

pub mod audio_loader;
pub mod database;
pub mod hashing;
pub mod peaks;
pub mod spectrogram;
