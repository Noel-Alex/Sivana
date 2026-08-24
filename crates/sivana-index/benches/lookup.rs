//! Index lookup benchmarks (PLAN §81): .siv mmap segment vs HashMap vs
//! LMDB over a synthetic 200k-posting catalog.
//! Run: `cargo bench -p sivana-index`

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sivana_audio::rng::XorShift64Star;
use sivana_core::{FingerprintVersion, RecordingId};
use std::collections::HashMap;
use std::path::PathBuf;

const N_RECORDINGS: u32 = 40;
const FPS_PER_REC: usize = 5_000; // ~200k postings total

struct Fixture {
    dir: PathBuf,
    hashes: Vec<u32>,
    map: HashMap<u32, Vec<sivana_index::segment::Posting>>,
}

fn build_fixture() -> Fixture {
    let mut rng = XorShift64Star::new(777_001);
    let mut b = sivana_index::segment::SegmentBuilder::new();
    let mut map: HashMap<u32, Vec<sivana_index::segment::Posting>> = HashMap::new();
    let mut hashes = Vec::new();
    for rec in 0..N_RECORDINGS {
        let fps: Vec<(u32, u32)> = (0..FPS_PER_REC)
            .map(|_| {
                (
                    (rng.next_f32() * 1e9) as u32,
                    (rng.next_f32() * 20_000.0) as u32,
                )
            })
            .collect();
        b.add_recording(RecordingId::new(rec), &fps);
        for &(h, t) in &fps {
            hashes.push(h);
            map.entry(h)
                .or_default()
                .push(sivana_index::segment::Posting {
                    recording: RecordingId::new(rec),
                    anchor_time: t,
                });
        }
    }
    let dir = std::env::temp_dir().join("sivana-index-bench");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    b.build(
        &dir.join("bench.siv"),
        FingerprintVersion::LANDMARK_V2_32BIT,
    )
    .unwrap();
    Fixture { dir, hashes, map }
}

fn bench_lookup(c: &mut Criterion) {
    let fx = build_fixture();
    let seg = sivana_index::segment::SivSegment::open(&fx.dir.join("bench.siv")).unwrap();
    let lmdb = sivana_index::lmdb::LmdbIndex::open(&fx.dir.join("lmdb")).unwrap();
    // Populate LMDB from the same data.
    {
        use std::collections::BTreeMap;
        let mut per_rec: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();
        // Rebuild per-recording lists from the map.
        for (&h, plist) in &fx.map {
            for p in plist {
                per_rec
                    .entry(p.recording.as_u32())
                    .or_default()
                    .push((h, p.anchor_time));
            }
        }
        for (rec, fps) in per_rec {
            lmdb.add_recording(RecordingId::new(rec), &fps).unwrap();
        }
    }

    // Deterministic probe set: 512 hashes that exist.
    let probes: Vec<u32> = fx
        .hashes
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(512)
        .collect();

    let mut group = c.benchmark_group("index_lookup_512_hashes");
    let mut out = Vec::new();
    group.bench_function("siv_mmap_segment", |b| {
        b.iter(|| {
            for &h in &probes {
                seg.lookup(black_box(h), &mut out);
            }
        })
    });
    group.bench_function("std_hash_map", |b| {
        b.iter(|| {
            for &h in &probes {
                black_box(&fx.map[&h]);
            }
        })
    });
    group.bench_function("lmdb_heed", |b| {
        b.iter(|| {
            for &h in &probes {
                let _ = lmdb.lookup(black_box(h), &mut out);
            }
        })
    });
    group.finish();
}

criterion_group!(benches, bench_lookup);
criterion_main!(benches);
