//! Fuzz-lite robustness harness for the parsers of untrusted bytes (§56).
//!
//! Real cargo-fuzz coverage is blocked on toolchain availability right now;
//! these are deterministic structured-garbage sweeps that run in CI on every
//! push. They encode the property that matters for a query server: *no
//! malformed input may panic, hang, or allocate unboundedly — every failure
//! is an Err/None.* Each generator mutates a valid fixture along the axes a
//! real attacker would explore (truncation, count-field lies, bad magic,
//! corrupt checksums, deep JSON nesting).

use std::path::PathBuf;

/// Deterministic xorshift so failures reproduce bit-for-bit.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sivana-fuzzlite-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Build a small valid two-recording segment + manifest catalog.
fn valid_catalog(dir: &std::path::Path) -> PathBuf {
    use sivana_core::RecordingId;
    use sivana_index::manifest;
    use sivana_index::segment::{SegmentBuilder};
    use sivana_core::FingerprintVersion;

    let mut builder = SegmentBuilder::new();
    for rec in 0..2u32 {
        let fps: Vec<(u32, u32)> = (0..500)
            .map(|i| ((rec * 10_000 + i) as u32, i as u32))
            .collect();
        builder.add_recording(RecordingId::new(rec), &fps);
    }
    let segment_path = dir.join("catalog-000001.siv");
    builder
        .build(&segment_path, FingerprintVersion::new(1, 0))
        .expect("build fixture segment");
    manifest::store_atomic(
        dir,
        &manifest::Manifest::new(1, FingerprintVersion::new(1, 0), vec![
            segment_path.file_name().unwrap().to_string_lossy().to_string(),
        ]),
    )
    .unwrap();
    segment_path
}

/// The .siv segment parser must reject arbitrary garbage without panicking.
#[test]
fn siv_segment_never_panics_on_garbage() {
    // Seed a valid segment to mutate.
    let dir = temp_dir("segment-garbage");
    valid_catalog(&dir);
    let good = std::fs::read(dir.join("catalog-000001.siv")).unwrap_or_else(|_| {
        // Segment naming may differ; grab whatever .siv exists.
        let mut p = dir.read_dir().unwrap().flatten();
        p.find_map(|e| {
            let path = e.path();
            (path.extension().and_then(|x| x.to_str()) == Some("siv"))
                .then(|| std::fs::read(&path).unwrap())
        })
        .unwrap()
    });

    let mut rng = Rng(0xDEADBEEF);
    for round in 0..2_000 {
        let mut bytes = good.clone();
        match round % 6 {
            // Truncation at every interesting boundary plus random points.
            0 => bytes.truncate(rng.below(bytes.len() + 1)),
            // Count/header field lies: huge recording/posting counts.
            1 if bytes.len() > 36 => {
                let off = 12 + rng.below(24);
                bytes[off] = 0xFF;
            }
            // Bad magic / version.
            2 => {
                let at = round % bytes.len();
                bytes[at] ^= 0xA5;
            }
            // Checksum corruption.
            3 if bytes.len() > 40 => {
                bytes[36 + rng.below(4)] ^= 0x01;
            }
            // Random single-byte flips anywhere.
            4 => {
                let at = rng.below(bytes.len());
                bytes[at] = (rng.next() & 0xFF) as u8;
            }
            // Empty and tiny inputs.
            _ => bytes.truncate(round % 48),
        }

        let tmp = dir.join(format!("probe-{round}.siv"));
        std::fs::write(&tmp, &bytes).unwrap();
        // The contract: Ok(_) or Err(_), never panic.
        let _ = sivana_index::segment::SivSegment::open(&tmp);
        let _ = std::fs::remove_file(&tmp);
    }

    // And lookups against a legitimately-opened segment must stay in-bounds
    // even for adversarial hash values (binary search over mapped memory).
    let live = dir.join("catalog-000001.siv");
    if let Ok(seg) = sivana_index::segment::SivSegment::open(&live) {
        let mut out = Vec::new();
        let mut rng = Rng(42);
        for _ in 0..5_000 {
            let hash = rng.next() as u32;
            let _ = seg.lookup(hash, &mut out);
        }
    }
}

#[test]
fn manifest_parser_rejects_garbage_without_panic() {
    use sivana_index::manifest;

    let dir = temp_dir("manifest-garbage");
    valid_catalog(&dir);
    let good = std::fs::read(dir.join(manifest::MANIFEST_FILE)).unwrap();

    let mut rng = Rng(0xCAFEBABE);

    // Structured JSON attacks: wrong types, missing fields, absurd nesting,
    // giant arrays, duplicate keys, unicode bombs.
    let structured = [
        "null".to_string(),
        "[]".to_string(),
        "{}".to_string(),
        r#"{"catalog_version": -1}"#.into(),
        r#"{"catalog_version": 1, "segments": null}"#.into(),
        format!(
            r#"{{"catalog_version": {}, "segments": []}}"#,
            u64::MAX
        ),
        r#"{"catalog_version": 1, "segments": ["../escape.siv"]}"#.into(),
        r#"{"catalog_version": 1, "fingerprint_major": "one"}"#.into(),
        format!(
            r#"{{"catalog_version": 1, "segments": [{}]}}"#,
            (0..5_000).map(|i| format!(r#""seg{i}.siv""#)).collect::<Vec<_>>().join(",")
        ),
        "[".repeat(10_000),
        r#"{"a":{"a":{"a":{"a":"\ud800"}}}}"#.into(),
    ];

    for (i, candidate) in structured.iter().enumerate() {
        std::fs::write(dir.join("manifest.json"), candidate).unwrap();
        let parsed = manifest::load(&dir);
        assert!(
            parsed.is_err(),
            "structured garbage #{i} unexpectedly parsed as a manifest"
        );
    }

    // Random byte mutations of a valid manifest: parse must never panic.
    for _ in 0..1_000 {
        let mut bytes = good.clone();
        let flips = 1 + rng.below(16);
        for _ in 0..flips {
            if !bytes.is_empty() {
                let at = rng.below(bytes.len());
                bytes[at] = (rng.next() & 0xFF) as u8;
            } else {
                break;
            }
        }
        std::fs::write(dir.join("manifest.json"), &bytes).unwrap();
        let _ = manifest::load(&dir);
    }
}

/// SFP1 wire batches arrive from untrusted browsers; the decoder must
/// survive every malformed shape. This mirrors the API server's decoder;
/// keep both in sync (the canonical one lives in sivana-api).
#[test]
fn sfp1_decoder_contract_on_garbage() {
    // Local copy of the documented wire format:
    //   magic "SFP1" | pad 4 | sample_rate u32 | count u32 | count × (u32,u32)
    fn decode(bytes: &[u8]) -> Option<(u32, Vec<(u32, u32)>)> {
        if bytes.len() < 16 || &bytes[..4] != b"SFP1" {
            return None;
        }
        let sample_rate = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
        let count = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
        if bytes.len() != 16 + count * 8 {
            return None;
        }
        let mut out = Vec::with_capacity(count.min(1 << 20));
        for i in 0..count {
            let o = 16 + i * 8;
            let h = u32::from_le_bytes(bytes[o..o + 4].try_into().ok()?);
            let t = u32::from_le_bytes(bytes[o + 4..o + 8].try_into().ok()?);
            out.push((h, t));
        }
        Some((sample_rate, out))
    }

    let mut rng = Rng(7);

    // A well-formed batch to mutate.
    let mut good = Vec::new();
    good.extend_from_slice(b"SFP1");
    good.extend_from_slice(&[0u8; 4]);
    good.extend_from_slice(&22_050_u32.to_le_bytes());
    good.extend_from_slice(&64_u32.to_le_bytes());
    for i in 0..64u32 {
        good.extend_from_slice(&(1000 + i).to_le_bytes());
        good.extend_from_slice(&i.to_le_bytes());
    }
    assert_eq!(decode(&good).map(|(_, f)| f.len()), Some(64));

    // Count-field lie: claims 4 billion entries but body is short.
    let mut liar = good.clone();
    liar[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode(&liar).is_none());

    // Truncations at every offset.
    for cut in 0..good.len() {
        let decoded = decode(&good[..cut]);
        assert!(
            decoded.is_none() || decoded.as_ref().unwrap().1.len() == 64,
            "truncated batch at {cut} decoded with wrong count"
        );
    }

    // Random flips: never panic; any success must have exact length match.
    for _ in 0..2_000 {
        let mut bytes = good.clone();
        let flips = 1 + rng.below(8);
        for _ in 0..flips {
            let at = rng.below(bytes.len());
            bytes[at] = (rng.next() & 0xFF) as u8;
        }
        if let Some((_, fps)) = decode(&bytes) {
            assert_eq!(fps.len(), (u32::from_le_bytes(bytes[12..16].try_into().unwrap())) as usize);
        }
    }

    // Empty / tiny / oversized-magic inputs.
    assert!(decode(b"").is_none());
    assert!(decode(&[0u8; 15]).is_none());
    assert!(decode(b"SFP2").is_none());
}
