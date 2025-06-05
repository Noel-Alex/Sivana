// src/spectrogram.rs
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use std::f32::consts::PI;

// This function is only used by create_spectrogram in this module, so it doesn't need to be pub
fn hann_window(window_size: usize) -> Vec<f32> {
    let mut window = Vec::with_capacity(window_size);
    if window_size == 0 {
        return window;
    }
    if window_size == 1 {
        window.push(1.0);
        return window;
    }
    for i in 0..window_size {
        window.push(0.5 * (1.0 - (2.0 * PI * i as f32 / (window_size - 1) as f32).cos()));
    }
    window
}

pub fn create_spectrogram( // Made public
                           samples: &[f32],
                           _sample_rate: u32,
                           window_size: usize,
                           hop_size: usize,
) -> Vec<Vec<f32>> {
    if samples.len() < window_size {
        println!("Not enough samples for a full FFT window.");
        return vec![];
    }

    let num_frames = (samples.len() - window_size) / hop_size + 1;
    if num_frames == 0 {
        println!("Calculated zero frames. Check sample length, window size, and hop size.");
        return vec![];
    }

    println!(
        "Debug: create_spectrogram - Samples: {}, Window: {}, Hop: {}, Frames: {}",
        samples.len(), window_size, hop_size, num_frames
    );

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(window_size);
    let mut buffer: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); window_size];
    let mut spectrogram: Vec<Vec<f32>> = Vec::with_capacity(num_frames);

    let window_values = hann_window(window_size); // Calls local hann_window

    for i in 0..num_frames {
        let start = i * hop_size;
        let end = start + window_size;
        let audio_chunk = &samples[start..end];

        for (j, sample) in audio_chunk.iter().enumerate() {
            buffer[j] = Complex::new(*sample * window_values[j], 0.0);
        }

        fft.process(&mut buffer);

        let num_bins_to_keep = window_size / 2 + 1;
        let mut magnitudes: Vec<f32> = Vec::with_capacity(num_bins_to_keep);
        for k in 0..num_bins_to_keep {
            magnitudes.push(buffer[k].norm());
        }
        spectrogram.push(magnitudes);
    }
    spectrogram
}