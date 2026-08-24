//! Sivana production index (PLAN §16-§21, Phase 3).
//!
//! Two backends behind one lookup interface:
//!
//! * [`lmdb`] — LMDB via `heed`, the Stage 1 production-capable store
//!   (§17). Proves the posting layout against a mature B+tree engine.
//! * [`segment`] — the custom immutable memory-mapped `.siv` format
//!   (§18-§21, index-format/SPEC.md): 32-bit hashes split into a 65,536
//!   bucket high-16 directory with binary search on low-16 inside, and
//!   fixed-width 8-byte postings. The OS page cache is the cache.
//!
//! Segments are immutable; catalogs grow by writing new segment files and
//! atomically swapping a manifest that lists the active set ([`manifest`]).

pub mod lmdb;
pub mod manifest;
pub mod segment;
