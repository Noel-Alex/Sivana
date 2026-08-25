//! Native probe mirroring the browser tone test, E12 revision.
//!
//! Minimum-statistics whitening makes the fingerprinter adaptive, so the
//! assertions here respect the adaptation window (~86 frames ~= 4 s):
//! measurements are taken from anchors well past it.
//!
//! * NOTE SEQUENCE: real spectral swings keep refreshing the floors, so
//!   landmarks flow indefinitely and their f1 field must equal the
//!   512-band mapping (bin 93 -> 335), not the 256-band one (167).
//! * PURE steady tones: stationary content is not identity; emission
//!   must cease entirely once the floors settle.
use sivana_landmark::LandmarkV2Config;
use sivana_wasm::FingerprintEngine;

fn collect(sig: &[f32]) -> Vec<(u32, u32)> {
    let cfg = LandmarkV2Config {
        freq_bands: sivana_core::OPERATING_FREQ_BANDS,
        ..Default::default()
    };
    let mut e = FingerprintEngine::new(22_050, cfg);
    for chunk in sig.chunks(5512) {
        e.process(chunk);
    }
    e.finish();
    let mut batch = Vec::new();
    e.take_batch(&mut batch);
    let count = u32::from_le_bytes(batch[12..16].try_into().unwrap()) as usize;
    (0..count)
        .map(|k| {
            let h = u32::from_le_bytes(batch[16 + k * 8..20 + k * 8].try_into().unwrap());
            let t = u32::from_le_bytes(batch[20 + k * 8..24 + k * 8].try_into().unwrap());
            (h >> 20, t)
        })
        .collect()
}

#[test]
fn engine_runs_at_operating_bands() {
    // Looping note sequence around 1 kHz; 14 s so the measurement region
    // sits far beyond the ~4 s adaptation window.
    let notes = [950.0f32, 1000.0, 1050.0, 1100.0];
    let note_len = 22050usize * 3 / 4;
    let n = 22050 * 14;
    let mut sig = vec![0.0f32; n];
    for (i, s) in sig.iter_mut().enumerate() {
        let f = notes[(i / note_len) % notes.len()];
        let into = i % note_len;
        let env = ((into.min(note_len - into) as f32 / (0.06 * 22050.0)).min(1.0)).sqrt();
        *s = 0.6 * env * (std::f32::consts::TAU * f * i as f32 / 22050.0).sin();
    }
    let fps = collect(&sig);
    // Measure only anchors in [8 s, 14 s] = frames [169, 237].
    let late: Vec<_> = fps
        .iter()
        .filter(|&&(f1, t)| t >= 169 && t <= 237 && f1 >= 320 && f1 <= 350)
        .copied()
        .collect();
    assert!(
        !late.is_empty(),
        "note sequence must keep producing landmarks near bin-93 bands after settling"
    );
    assert!(
        late.iter().any(|&(f1, _)| f1 == 335),
        "expected f1=335 (512-band mapping of bin 93) among late anchors"
    );
}

#[test]
fn stationary_tone_emission_stops_after_adaptation() {
    // A pure sine is the E12 anti-goal: stationary content carries no
    // identity. The silence->tone step may emit briefly (real onset),
    // but after the ~4 s trailing window fills with steady tone, the
    // floor pins the carrier and emission must cease entirely.
    let n = 22050 * 14;
    let mut sig = vec![0.0f32; n];
    for (i, s) in sig.iter_mut().enumerate() {
        let t = i as f32 / 22050.0;
        *s = 0.6 * (std::f32::consts::TAU * 1000.0 * t).sin();
    }
    let fps = collect(&sig);
    // Anchors past frame 130 (6 s): window fully steady for >=2 s.
    let late = fps.iter().filter(|&&(_, t)| t >= 130).count();
    assert_eq!(
        late, 0,
        "steady-state stationary tone must yield zero landmarks, got {late}"
    );
}
