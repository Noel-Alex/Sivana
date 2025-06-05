// src/hashing.rs
use crate::peaks::Peak; // Import Peak from our peaks module

// Parameters for landmark hashing
pub const TARGET_ZONE_DT_MIN_FRAMES: usize = 1;
pub const TARGET_ZONE_DT_MAX_FRAMES: usize = 50;
pub const TARGET_ZONE_DF_ABS_MAX_BINS: usize = 200;
pub const MAX_PAIRS_PER_ANCHOR: usize = 5;
pub const HASH_FREQ_BITS: u32 = 10;
pub const HASH_DELTA_TIME_BITS: u32 = 8;

#[derive(Debug, Clone, Copy)]
pub struct Fingerprint { // Made public
    pub hash: u64,          // Fields public
    pub anchor_time_idx: usize,
}

pub fn create_hashes( // Made public
                      peaks: &[Peak],
                      dt_min_frames: usize,
                      dt_max_frames: usize,
                      df_abs_max_bins: usize,
                      max_pairs_per_anchor: usize,
) -> Vec<Fingerprint> {
    let mut fingerprints: Vec<Fingerprint> = Vec::new();

    if peaks.len() < 2 {
        println!("Debug: create_hashes - Not enough peaks to form pairs (need at least 2).");
        return fingerprints;
    }

    println!(
        "Debug: create_hashes - Processing {} peaks. Target zone: dt=[{}-{}], df_abs_max={}, max_pairs={}",
        peaks.len(), dt_min_frames, dt_max_frames, df_abs_max_bins, max_pairs_per_anchor
    );

    for i in 0..peaks.len() {
        let anchor_peak = &peaks[i];
        let mut pairs_found_for_this_anchor = 0;

        for j in (i + 1)..peaks.len() {
            if pairs_found_for_this_anchor >= max_pairs_per_anchor {
                break;
            }
            let target_peak = &peaks[j];
            let delta_time_frames = target_peak.time_idx.saturating_sub(anchor_peak.time_idx);

            if delta_time_frames < dt_min_frames { continue; }
            if delta_time_frames > dt_max_frames { continue; }

            let delta_freq_bins_abs = (target_peak.freq_bin_idx as isize - anchor_peak.freq_bin_idx as isize).abs() as usize;
            if delta_freq_bins_abs > df_abs_max_bins { continue; }

            let f1 = anchor_peak.freq_bin_idx as u64;
            let f2 = target_peak.freq_bin_idx as u64;
            let dt = delta_time_frames as u64;

            let f1_masked = f1 & ((1 << HASH_FREQ_BITS) - 1);
            let f2_masked = f2 & ((1 << HASH_FREQ_BITS) - 1);
            let dt_masked = dt & ((1 << HASH_DELTA_TIME_BITS) - 1);

            let robust_hash_val = (f1_masked << (HASH_FREQ_BITS + HASH_DELTA_TIME_BITS)) |
                (f2_masked << HASH_DELTA_TIME_BITS) |
                dt_masked;

            fingerprints.push(Fingerprint {
                hash: robust_hash_val,
                anchor_time_idx: anchor_peak.time_idx,
            });
            pairs_found_for_this_anchor += 1;
        }
    }
    println!("Debug: create_hashes - Generated {} fingerprints.", fingerprints.len());
    fingerprints
}