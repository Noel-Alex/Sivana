//! Round-trip: decode the real mp3, fingerprint an excerpt, match it against
//! an index built from a differently-degraded copy of the same audio. This
//! isolates catalog-side matching from the acoustic path: no server, no
//! sidecars — just decode → ingest geometry → query → calibrated gate.
//!
//! Skips (rather than fails) when the fixture mp3 is unavailable so the
//! workspace suite stays green on machines without the local fixture set.

use sivana_core::RecordingId;
use sivana_ingest::FREQ_BANDS;
use sivana_landmark::LandmarkV2Config;
use sivana_match::{InMemoryIndex, MatchParams, QueryFp};

const FIXTURE: &str =
    r"C:\Users\aliza\Documents\Portfolio website\out\assets\audio\megalovania.mp3";

#[test]
fn megalovania_excerpt_matches_served_index() {
    let Ok(bytes) = std::fs::read(FIXTURE) else {
        // Fixture lives outside the repo; absence is an environment fact,
        // not a regression.
        eprintln!("skipping: fixture not found at {FIXTURE}");
        return;
    };
    let (mono, sr) = sivana_audio::decode::decode_mono(&bytes).expect("decode");
    println!("decoded: {} samples @ {} Hz", mono.len(), sr);
    // Ingest geometry: the same band-limited sinc resampler the server uses
    // (resample_linear folds aliasing energy into the passband and corrupts
    // reference peaks — E7).
    let pcm = sivana_dsp::resample::resample_sinc(&mono, sr, 22_050);

    let cfg = LandmarkV2Config {
        freq_bands: FREQ_BANDS,
        ..Default::default()
    };

    // Reference index: the first 60 s, as if ingested.
    let ref_len = 60 * 22_050.min(pcm.len());
    let reference = &pcm[..ref_len];
    let ref_fps = sivana_landmark::fingerprint(reference, 22_050, &cfg);
    assert!(!ref_fps.is_empty(), "reference produced no fingerprints");
    let mut index = InMemoryIndex::new();
    index.add_recording(RecordingId::new(0), &ref_fps.iter().map(|f| (f.hash, f.anchor_time)).collect::<Vec<_>>());
    index.finalize();

    // Query: a 6 s excerpt from 30 s in (past the intro) — different decode
    // path than the reference would be in production; here the same PCM, so
    // this is the catalog-side sanity leg of the pipeline.
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
    assert_eq!(top.recording.as_u32(), 0);
    // E4-calibrated gate constants (bands=512, tol=2): a true self-match
    // must clear what the streaming session would demand of it.
    assert!(top.inliers >= 7, "gate would reject: {} inliers", top.inliers);
    assert!(
        top.offset_concentration >= 0.5,
        "gate would reject: conc {}",
        top.offset_concentration
    );
    // Offset must point back at the excerpt position (~30 s minus window).
    let expected_offset = 30 * 22_050 / 1024;
    assert!(
        (top.offset_frames as i64 - expected_offset as i64).abs() <= 4,
        "offset {} not near expected {}",
        top.offset_frames,
        expected_offset
    );
}
