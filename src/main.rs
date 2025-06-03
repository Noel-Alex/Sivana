// src/main.rs

// Declare modules
mod spectrogram;
mod peaks;
mod hashing;
mod database;
mod audio_loader;

// --- IMPORTS from our modules ---
use crate::audio_loader::load_audio_file;
use crate::database::{enroll_song, query_db_and_match, FingerprintDB, Song}; // Added MatchResult
use crate::hashing::{create_hashes, MAX_PAIRS_PER_ANCHOR, TARGET_ZONE_DF_ABS_MAX_BINS, TARGET_ZONE_DT_MAX_FRAMES, TARGET_ZONE_DT_MIN_FRAMES};
use crate::peaks::find_peaks;
use crate::spectrogram::create_spectrogram;

// --- Standard Library and Other IMPORTS ---
use std::collections::HashMap;
 // Keep PI if you decide to add back dummy generation for quick tests
use std::path::Path;

// --- GLOBAL CONSTANTS ---
const SAMPLE_RATE: u32 = 22050;
const FFT_WINDOW_SIZE: usize = 2048;
const FFT_HOPSIZE: usize = 1024;

// --- MAIN FUNCTION ---
fn main() -> Result<(), String> { // Main can return a Result for easier error handling
    println!("Sivana Audio Fingerprinter - Enrollment and Query Mode");

    let mut fingerprint_database: FingerprintDB = HashMap::new();

    // Parameters for different stages
    let spec_peak_params = (2, 5, 2.0f32); // neighborhood_t_r, neighborhood_f_r, min_mag_thresh
    // These peak params (esp. min_mag_thresh) will likely need
    // significant tuning for real audio.
    let hashing_params = (
        TARGET_ZONE_DT_MIN_FRAMES,
        TARGET_ZONE_DT_MAX_FRAMES,
        TARGET_ZONE_DF_ABS_MAX_BINS,
        MAX_PAIRS_PER_ANCHOR,
    );

    // --- Enroll Song 0 (From File) ---
    println!("\n--- ENROLLING SONG 0 (FROM FILE) ---");
    let song0_path_str = "data/a.mp3";
    let song0_path = Path::new(song0_path_str);
    if !song0_path.exists() {
        return Err(format!("Audio file for Song 0 not found: {}", song0_path_str));
    }
    let song0 = Song { id: 0, name: format!("Song from file: {}", song0_path.file_name().unwrap_or_default().to_string_lossy()) };

    let samples_song0 = load_audio_file(song0_path, SAMPLE_RATE)
        .map_err(|e| format!("Error loading Song 0 ({}): {}", song0_path_str, e))?;

    println!("Loaded {} samples for Song 0.", samples_song0.len());
    if samples_song0.is_empty() {
        return Err(format!("No samples loaded for Song 0 ({}). File might be empty or invalid after decoding.", song0_path_str));
    }

    enroll_song(&mut fingerprint_database, &song0, &samples_song0, SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE, spec_peak_params, hashing_params)
        .map_err(|e| format!("Error enrolling Song 0: {}", e))?;

    // --- Enroll Song 1 (From File) ---
    println!("\n--- ENROLLING SONG 1 (FROM FILE) ---");
    let song1_path_str = "data/Whispers of the Wind.mp3";
    let song1_path = Path::new(song1_path_str);
    if !song1_path.exists() {
        return Err(format!("Audio file for Song 1 not found: {}", song1_path_str));
    }
    let song1 = Song { id: 1, name: format!("Song from file: {}", song1_path.file_name().unwrap_or_default().to_string_lossy()) };

    let samples_song1 = load_audio_file(song1_path, SAMPLE_RATE)
        .map_err(|e| format!("Error loading Song 1 ({}): {}", song1_path_str, e))?;

    println!("Loaded {} samples for Song 1.", samples_song1.len());
    if samples_song1.is_empty() {
        return Err(format!("No samples loaded for Song 1 ({}). File might be empty or invalid after decoding.", song1_path_str));
    }

    enroll_song(&mut fingerprint_database, &song1, &samples_song1, SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE, spec_peak_params, hashing_params)
        .map_err(|e| format!("Error enrolling Song 1: {}", e))?;

    println!("Current DB size (unique hashes after enrolling all songs): {}", fingerprint_database.len());

    // --- Prepare Query Snippet (e.g., a slice of the loaded Song 0) ---
    // We'll use the `samples_song0` that was loaded from file earlier.
    println!("\n--- PREPARING QUERY SNIPPET (from loaded Song 0) ---");
    if samples_song0.is_empty() {
        // This check is somewhat redundant if enrollment succeeded, but good for safety.
        return Err("Song 0 samples are unexpectedly empty, cannot create query snippet.".to_string());
    }

    let slice_duration_seconds = 5.0; // Duration of the query snippet in seconds
    let slice_start_seconds = 10.0; // When to start the slice in the song

    let slice_start_sample_offset = (SAMPLE_RATE as f32 * slice_start_seconds) as usize;
    let slice_end_sample_offset = slice_start_sample_offset + (SAMPLE_RATE as f32 * slice_duration_seconds) as usize;

    let frame_offset_approx_for_query = slice_start_sample_offset / FFT_HOPSIZE;

    if slice_start_sample_offset >= samples_song0.len() {
        return Err(format!(
            "Query slice start point (sample {}) is beyond Song 0 length ({} samples). Try a shorter start time or ensure song is long enough.",
            slice_start_sample_offset,
            samples_song0.len()
        ));
    }
    // Ensure slice_end_sample_offset doesn't exceed song length
    let actual_slice_end = slice_end_sample_offset.min(samples_song0.len());
    if slice_start_sample_offset >= actual_slice_end {
        return Err(format!(
            "Query slice start (sample {}) is at or after calculated end (sample {}). Snippet would be empty. Song might be too short for chosen slice parameters.",
            slice_start_sample_offset,
            actual_slice_end
        ));
    }


    let query_audio_slice = &samples_song0[slice_start_sample_offset..actual_slice_end];
    println!(
        "Query snippet from loaded Song 0: {} samples (from sample {} to {}), approx frame offset {}.",
        query_audio_slice.len(),
        slice_start_sample_offset,
        actual_slice_end,
        frame_offset_approx_for_query
    );

    if query_audio_slice.is_empty() {
        return Err("Query audio slice is empty. Check slice parameters and song length.".to_string());
    }

    // Generate fingerprints for the query snippet
    let query_spectrogram = create_spectrogram(query_audio_slice, SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE);
    if query_spectrogram.is_empty() {
        println!("Warning: Query spectrogram is empty. No fingerprints will be generated.");
        // Allow continuing to see if NO MATCH is found, or return Err(...)
    }

    let query_peaks = find_peaks(&query_spectrogram, spec_peak_params.0, spec_peak_params.1, spec_peak_params.2);
    if query_peaks.is_empty() {
        println!("Warning: No peaks found in query snippet. No fingerprints will be generated.");
        // Allow continuing
    }

    let query_fingerprints = create_hashes(&query_peaks, hashing_params.0, hashing_params.1, hashing_params.2, hashing_params.3);
    if query_fingerprints.is_empty() {
        println!("Warning: No fingerprints generated for query snippet.");
        // Allow continuing
    }
    println!("Generated {} fingerprints for query snippet.", query_fingerprints.len());

    // --- Perform Matching ---
    println!("\n--- PERFORMING MATCHING ---");
    if let Some(match_result) = query_db_and_match(&fingerprint_database, &query_fingerprints) {
        println!("\n======= MATCH FOUND! =======");
        println!("Matched Song ID: {}", match_result.song_id);

        let matched_song_name = if match_result.song_id == song0.id {
            &song0.name
        } else if match_result.song_id == song1.id {
            &song1.name
        } else {
            // This case shouldn't happen if only song0 and song1 are enrolled with these IDs
            "Unknown Song Name (Error in logic or DB)"
        };
        println!("Matched Song Name: {}", matched_song_name);
        println!("Match Score: {}", match_result.score);
        println!("Calculated Time Offset in Song (frames): {}", match_result.time_offset_in_song_frames);
        println!("(Recall query snippet started approx {} frames into the original at {} seconds)", frame_offset_approx_for_query, slice_start_seconds);
    } else {
        println!("\n======= NO MATCH FOUND =======");
    }

    Ok(()) // Main returns Ok if everything succeeded
}