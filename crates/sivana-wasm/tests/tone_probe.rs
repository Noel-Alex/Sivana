//! Native probe mirroring the browser tone test: 1 kHz sine -> the
//! dominant f1 field must equal the 512-band mapping (335), not the
//! 256-band one (167).
use sivana_landmark::LandmarkV2Config;
use sivana_wasm::FingerprintEngine;

#[test]
fn engine_runs_at_operating_bands() {
    let cfg = LandmarkV2Config {
        freq_bands: sivana_core::OPERATING_FREQ_BANDS,
        ..Default::default()
    };
    assert_eq!(cfg.freq_bands, 512);
    let mut e = FingerprintEngine::new(22_050, cfg);
    let n = 22050 * 3;
    let mut sig = vec![0.0f32; n];
    for (i, s) in sig.iter_mut().enumerate() {
        *s = 0.6 * (std::f32::consts::TAU * 1000.0 * i as f32 / 22050.0).sin();
    }
    for chunk in sig.chunks(5512) {
        e.process(chunk);
    }
    e.finish();
    let mut batch = Vec::new();
    e.take_batch(&mut batch);
    let count = u32::from_le_bytes(batch[12..16].try_into().unwrap()) as usize;
    let mut f1s = std::collections::HashMap::new();
    for k in 0..count {
        let h = u32::from_le_bytes(batch[16 + k * 8..20 + k * 8].try_into().unwrap());
        *f1s.entry(h >> 20).or_insert(0) += 1;
    }
    let top: Vec<_> = f1s.into_iter().collect();
    println!("top f1s: {:?}", &top[..top.len().min(4)]);
    // 512-band mapping of bin 93 is 335.
    assert!(
        top.iter().any(|&(f1, _)| f1 == 335),
        "expected f1=335 (512 bands)"
    );
}
