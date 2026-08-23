//! Round-trip: decode the real mp3, query the served sidecar index with a
//! self-captured excerpt, apply the calibrated gate. Isolates catalog-side
//! matching from the acoustic path.

use sivana_core::RecordingId;
use sivana_landmark::LandmarkV2Config;
use sivana_match::{InMemoryIndex, MatchParams, QueryFp};

const CATALOG: &str = r"C:\Users\aliza\AppData\Local\Temp\sivana-catalog2";

#[test]
fn megalovania_excerpt_matches_served_index() {
    let bytes = std::fs::read(
        r"C:\Users\aliza\Documents\Portfolio website\out\assets\audio\megalovania.mp3",
    )
    .expect("read mp3");
    let (mono, sr) = sivana_audio::decode::decode_mono(&bytes).expect("decode");
    println!("decoded: {} samples @ {} Hz", mono.len(), sr);
    let pcm = sivana_dsp::resample::resample_linear(&mono, sr, 22_050);

    // Build the index EXACTLY like the server: from sidecars.
    let cfg = LandmarkV2Config {
        freq_bands: sivana_ingest::FREQ_BANDS,
        ..Default::default()
    };
    let mut index = InMemoryIndex::new();
    let mut loaded = 0;
    for entry in std::fs::read_dir(format!(r"{CATALOG}\fingerprints")).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("sfp") {
            continue;
        }
        let raw = std::fs::read(&p).unwrap();
        let (_, fps) = decode(&raw).expect("sidecar decodes");
        let id: u32 = p.file_stem().unwrap().to_str().unwrap().parse().unwrap();
        index.add_recording(RecordingId::new(id), &fps);
        loaded += 1;
    }
    index.finalize();
    println!("index: {loaded} recordings, {} hashes", index.len());

    // Query: a 6 s excerpt from 30 s in (past the intro).
    let start = 30 * 22_050;
    let excerpt = &pcm[start..start + 6 * 22_050];
    let q = sivana_landmark::fingerprint(excerpt, 22_050, &cfg);
    println!("query fingerprints: {}", q.len());
    let qfps: Vec<QueryFp> = q
        .iter()
        .map(|f| QueryFp {
            hash: f.hash,
            anchor_time: f.anchor_time,
        })
        .collect();

    let outcomes = index.query(&qfps, &MatchParams::default());
    for o in &outcomes {
        println!(
            "candidate rec {} score {:.3} inliers {} conc {:.2} offset {}",
            o.recording.as_u32(),
            o.weighted_score,
            o.inliers,
            o.offset_concentration,
            o.offset_frames
        );
    }
    let top = outcomes.first().expect("no candidates at all");
    assert_eq!(
        top.recording.as_u32(),
        4,
        "must identify Megalovania (rec 4)"
    );
    assert!(
        top.inliers >= 7,
        "gate would reject: {} inliers",
        top.inliers
    );
}

/// Local SFP1 decode (mirrors the server decoder).
fn decode(bytes: &[u8]) -> Option<(u32, Vec<(u32, u32)>)> {
    if bytes.len() < 16 || &bytes[..4] != b"SFP1" {
        return None;
    }
    let sr = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let count = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
    if bytes.len() != 16 + count * 8 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let o = 16 + i * 8;
        let h = u32::from_le_bytes(bytes[o..o + 4].try_into().ok()?);
        let t = u32::from_le_bytes(bytes[o + 4..o + 8].try_into().ok()?);
        out.push((h, t));
    }
    Some((sr, out))
}
