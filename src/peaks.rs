// src/peaks.rs
#[derive(Debug, Clone, Copy)]
pub struct Peak { // Made public
    pub time_idx: usize,     // Fields also public
    pub freq_bin_idx: usize,
    pub magnitude: f32,
}

pub fn find_peaks( // Made public
                   spectrogram: &[Vec<f32>],
                   neighborhood_time_radius: usize,
                   neighborhood_freq_radius: usize,
                   min_magnitude_threshold: f32,
) -> Vec<Peak> {
    let mut peaks: Vec<Peak> = Vec::new();

    if spectrogram.is_empty() || spectrogram.first().map_or(true, |frame| frame.is_empty()) {
        println!("Debug: find_peaks - Spectrogram is empty or first frame is empty.");
        return peaks;
    }

    let num_frames = spectrogram.len();
    let num_freq_bins = spectrogram[0].len();

    println!(
        "Debug: find_peaks - Spectrogram: {} frames, {} freq bins.",
        num_frames, num_freq_bins
    );
    println!(
        "Debug: find_peaks - Neighborhood: TimeRadius={}, FreqRadius={}, MinMag={}",
        neighborhood_time_radius, neighborhood_freq_radius, min_magnitude_threshold
    );

    for t_idx in 0..num_frames {
        for f_idx in 0..num_freq_bins {
            let current_magnitude = spectrogram[t_idx][f_idx];

            if current_magnitude < min_magnitude_threshold {
                continue;
            }

            let mut is_local_max = true;
            let t_start = t_idx.saturating_sub(neighborhood_time_radius);
            let t_end = (t_idx + neighborhood_time_radius + 1).min(num_frames);
            let f_start = f_idx.saturating_sub(neighborhood_freq_radius);
            let f_end = (f_idx + neighborhood_freq_radius + 1).min(num_freq_bins);

            for nt_idx in t_start..t_end {
                for nf_idx in f_start..f_end {
                    if nt_idx == t_idx && nf_idx == f_idx {
                        continue;
                    }
                    if spectrogram[nt_idx][nf_idx] > current_magnitude {
                        is_local_max = false;
                        break;
                    }
                    if spectrogram[nt_idx][nf_idx] == current_magnitude && (nt_idx < t_idx || (nt_idx == t_idx && nf_idx < f_idx)) {
                        is_local_max = false;
                        break;
                    }
                }
                if !is_local_max {
                    break;
                }
            }

            if is_local_max {
                peaks.push(Peak {
                    time_idx: t_idx,
                    freq_bin_idx: f_idx,
                    magnitude: current_magnitude,
                });
            }
        }
    }
    println!("Debug: find_peaks - Found {} peaks.", peaks.len());
    peaks
}