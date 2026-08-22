//! Hot-path benchmarks (§55): legacy fingerprinting cost per query length.
//! Run: `cargo bench -p sivana-bench`

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sivana_audio::fixtures;
use sivana_core::config::AlgorithmConfig;

fn legacy_fingerprint(
    samples: &[f32],
    sample_rate: u32,
    cfg: &AlgorithmConfig,
) -> Vec<sivana_legacy::hashing::Fingerprint> {
    let spec = sivana_legacy::spectrogram::create_spectrogram(
        samples,
        sample_rate,
        cfg.fft.window_size,
        cfg.fft.hop_size,
    );
    let peaks = sivana_legacy::peaks::find_peaks(
        &spec,
        cfg.peaks.neighborhood_time_radius,
        cfg.peaks.neighborhood_freq_radius,
        cfg.peaks.min_magnitude_threshold,
    );
    sivana_legacy::hashing::create_hashes(
        &peaks,
        cfg.landmarks.dt_min_frames,
        cfg.landmarks.dt_max_frames,
        cfg.landmarks.df_abs_max_bins,
        cfg.landmarks.fanout,
    )
}

fn bench_legacy_fingerprint(c: &mut Criterion) {
    let cfg = AlgorithmConfig::legacy();
    for seconds in [2.0f32, 8.0] {
        let samples = fixtures::synth_song(42, seconds + 0.5, 22_050);
        c.bench_function(&format!("legacy_fingerprint_{seconds}s"), |b| {
            b.iter(|| legacy_fingerprint(black_box(&samples), 22_050, &cfg))
        });
    }
}

criterion_group!(benches, bench_legacy_fingerprint);
criterion_main!(benches);
