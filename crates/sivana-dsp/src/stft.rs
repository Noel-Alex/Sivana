//! Streaming STFT: constant memory, zero per-frame allocations (§4.1, §8).
//!
//! The frozen prototype allocates an entire spectrogram up front. This
//! module is its replacement: feed PCM chunks, receive one magnitude frame
//! at a time through a caller-owned scratch vector. All FFT planning,
//! window coefficients and complex buffers are created once.

use rustfft::{FftPlanner, num_complex::Complex};
use std::sync::Arc;

pub struct StftStreamer {
    fft: Arc<dyn rustfft::Fft<f32>>,
    window: Vec<f32>,
    /// Leftover samples not yet consumed by a full frame.
    tail: Vec<f32>,
    scratch: Vec<Complex<f32>>,
    pub window_size: usize,
    pub hop_size: usize,
    /// Total frames emitted so far (global time index).
    frame_index: u64,
}

impl StftStreamer {
    /// Create a streamer; `window_size` must be a power of two.
    pub fn new(window_size: usize, hop_size: usize, window: &[f32]) -> Self {
        assert!(window_size.is_power_of_two(), "window_size must be pow2");
        assert!(hop_size >= 1 && hop_size <= window_size);
        assert_eq!(window.len(), window_size);
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(window_size);
        Self {
            fft,
            window: window.to_vec(),
            tail: Vec::with_capacity(window_size),
            scratch: vec![Complex::new(0.0, 0.0); window_size],
            window_size,
            hop_size,
            frame_index: 0,
        }
    }

    pub fn frames_emitted(&self) -> u64 {
        self.frame_index
    }

    /// Feed PCM and emit every complete frame's magnitude spectrum into
    /// `out_mags` (length window/2+1), invoking `emit(frame_index, mags)`.
    ///
    /// `out_mags` is reused between calls — never stored.
    pub fn process(
        &mut self,
        samples: &[f32],
        out_mags: &mut Vec<f32>,
        mut emit: impl FnMut(u64, &[f32]),
    ) {
        out_mags.clear();
        if out_mags.capacity() < self.window_size / 2 + 1 {
            out_mags.reserve(self.window_size / 2 + 1 - out_mags.capacity());
        }

        // Append incoming samples to the tail buffer.
        self.tail.extend_from_slice(samples);

        // Emit frames while a full window is available.
        while self.tail.len() >= self.window_size {
            for j in 0..self.window_size {
                self.scratch[j] = Complex::new(self.tail[j] * self.window[j], 0.0);
            }
            self.fft.process(&mut self.scratch);

            let bins = self.window_size / 2 + 1;
            out_mags.clear();
            for k in 0..bins {
                out_mags.push(self.scratch[k].norm());
            }
            emit(self.frame_index, out_mags);
            self.frame_index += 1;

            // Slide by hop: drop the first hop samples.
            self.tail.drain(0..self.hop_size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::hann_periodic;

    fn sine(freq: f32, sr: f32, seconds: f32) -> Vec<f32> {
        (0..(seconds * sr) as usize)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / sr).sin())
            .collect()
    }

    #[test]
    fn emits_expected_frame_count() {
        let sr = 16_000.0;
        let n = (sr * 1.0) as usize; // 16000 samples
        let win = 1024;
        let hop = 512;
        let mut st = StftStreamer::new(win, hop, &hann_periodic(win));
        let sig = sine(1000.0, sr, 1.0);
        let mut mags = Vec::new();
        let mut count = 0u64;
        st.process(&sig, &mut mags, |_, _| count += 1);
        // Offline equivalent: (n - win)/hop + 1
        let expected = ((n - win) / hop + 1) as u64;
        assert_eq!(count, expected);
        assert_eq!(st.frames_emitted(), expected);
    }

    #[test]
    fn chunking_does_not_change_frames() {
        let sr = 16_000.0;
        let sig = sine(500.0, sr, 0.5);
        let run = |chunk: usize| {
            let mut st = StftStreamer::new(512, 256, &hann_periodic(512));
            let mut frames: Vec<Vec<f32>> = Vec::new();
            let mut mags = Vec::new();
            for piece in sig.chunks(chunk) {
                st.process(piece, &mut mags, |_, m| frames.push(m.to_vec()));
            }
            frames
        };
        let a = run(1600);
        let b = run(7);
        assert_eq!(a.len(), b.len());
        for (fa, fb) in a.iter().zip(b.iter()) {
            assert_eq!(fa, fb);
        }
    }

    #[test]
    fn tone_peaks_at_correct_bin() {
        let sr = 16_000.0f32;
        let freq = 2000.0f32;
        let win = 1024usize;
        let mut st = StftStreamer::new(win, win, &hann_periodic(win)); // hop=win ok
        let sig = sine(freq, sr, 0.2);
        let mut best_bin = 0usize;
        let mut mags = Vec::new();
        st.process(&sig, &mut mags, |_, m| {
            let mb = m
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap()
                .0;
            best_bin = mb;
        });
        let expected_bin = (freq / sr * win as f32).round() as usize; // 128
        assert_eq!(best_bin, expected_bin);
    }

    #[test]
    fn partial_tail_is_buffered_not_dropped() {
        let win = 256usize;
        let hop = 64usize;
        let mut st = StftStreamer::new(win, hop, &hann_periodic(win));
        let mut count = 0u64;
        let mut mags = Vec::new();
        st.process(&[0.5f32; 255], &mut mags, |_, _| count += 1);
        assert_eq!(count, 0);
        st.process(&[0.5f32; 10], &mut mags, |_, _| count += 1);
        // 265 samples >= window -> exactly one frame emitted.
        assert_eq!(count, 1);
    }
}
