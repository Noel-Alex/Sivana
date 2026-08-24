# Sivana Index Format (`.siv`) — WORKING SPEC

Status: design draft for Phase 3 (PLAN.md §16–§21). Do not implement
before the LMDB stage proves the posting layout.

## Goals

- Immutable segment files; queries via `memmap2`; OS page cache as cache.
- Near-zero allocations in the query path.
- 32-bit hashes -> high-16 bucket directory + low-16 binary search.

## File layout

```
offset  size  field
0       4     magic "SIV1"
4       4     index_format_version (u32 LE)
8       4     fingerprint_format_version (u32 LE)
12      8     recording_count (u64)
20      8     hash_count (u64)          # distinct hashes in this segment
28      8     posting_count (u64)
36      8     checksum (FNV-1a of everything after header, or xxh3 later)
44      --    bucket_directory: 65_537 * u64 offsets (512 KiB + sentinel)
--      --    hash_entries: sorted by full u32 hash
                per entry: hash_low16 (u16), postings_offset (u40),
                           document_frequency (u24), pad (u16)
--      --    postings: contiguous runs per hash
                per posting: recording_id (u32) | anchor_time (u24) | flags (u8)
```

## Lookup

```
hash -> high16 -> [start,end) from directory
      -> binary search hash_entries on low16
      -> read df; if stop-hash: skip or cap
      -> scan postings slice
```

## Segments & manifests

Files `catalog-000123.siv`; a manifest lists active segments + versions;
servers atomically swap manifests. Compaction merges N segments into one,
recomputing document frequencies. Delta segments carry only new recordings.

## Open questions (to resolve by benchmark, not taste)

- Packed u64 postings vs delta+varint: decode speed vs size at 1e9 postings.
- xxh3 vs FNV-1a for checksums.
- Per-band posting caps for stop-hash mitigation vs near-zero weights.
