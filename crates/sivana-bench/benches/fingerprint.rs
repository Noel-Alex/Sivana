//! Hot-path benchmarks (§55): fingerprinting cost per query length, plus
//! micro-benches for the streaming DSP stages that feed Engine A.
//! Run: `cargo bench -p sivana-bench`

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sivana_audio::fixtures;
use sivana_core::config::AlgorithmConfig;
use sivana_dsp::peaks_v2::{PeakStreamer, PeaksV2Config};
use sivana_dsp::stft::StftStreamer;
use sivana_dsp::window::hann_periodic;

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

fn bench_v2_fingerprint(c: &mut Criterion) {
    let cfg = sivana_landmark::LandmarkV2Config::default();
    for seconds in [2.0f32, 8.0] {
        let samples = fixtures::synth_song(42, seconds + 0.5, 22_050);
        c.bench_function(&format!("v2_fingerprint_{seconds}s"), |b| {
            b.iter(|| sivana_landmark::fingerprint(black_box(&samples), 22_050, &cfg))
        });
    }
}

/// STFT throughput alone — isolates the FFT stage from peak detection.
fn bench_stft_streamer(c: &mut Criterion) {
    let win = 2048;
    let hop = 1024;
    let window = hann_periodic(win);
    // ~8 s of audio at 22050 Hz.
    let samples = fixtures::synth_song(42, 8.5, 22_050);
    let mut stft = StftStreamer::new(win, hop, &window);
    let mut mags = Vec::new();
    c.bench_function("stft_streamer_8s", |b| {
        b.iter(|| {
            stft.feed(black_box(&samples));
            while stft.next_frame(&mut mags).is_some() {}
        })
    });
}

/// Peak detection over the same frames the STFT emits.
fn bench_peak_streamer(c: &mut Criterion) {
    let cfg = PeaksV2Config::default();
    let win = 2048;
    let hop = 1024;
    let window = hann_periodic(win);
    let samples = fixtures::synth_song(42, 8.5, 22_050);

    // Materialize frames once (outside the timed section).
    let mut stft = StftStreamer::new(win, hop, &window);
    let mut frames: Vec<Vec<f32>> = Vec::new();
    let mut mags = Vec::new();
    stft.feed(&samples);
    while let Some(_) = stft.next_frame(&mut mags) {
        frames.push(mags.clone());
    }

    let mut ps = PeakStreamer::new(win / 2 + 1, cfg.clone());
    let mut out = Vec::new();
    c.bench_function("peak_streamer_8s", |b| {
        b.iter(|| {
            ps = PeakStreamer::new(win / 2 + 1, cfg.clone());
            out.clear();
            for f in &frames {
                ps.process_frame(black_box(f), &mut out);
            }
            ps.finish(&mut out);
        })
    });
}

criterion_group!(
    benches,
    bench_legacy_fingerprint,
    bench_v2_fingerprint,
    bench_stft_streamer,
    bench_peak_streamer
);
criterion_main!(benches);
