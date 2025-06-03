// src/main.rs

// Declare modules
mod spectrogram;
mod peaks;
mod hashing;
mod database;

// --- IMPORTS from our modules ---
use spectrogram::create_spectrogram;
use peaks::{Peak, find_peaks};
use hashing::{Fingerprint, create_hashes};
use hashing::{
    TARGET_ZONE_DT_MIN_FRAMES, TARGET_ZONE_DT_MAX_FRAMES,
    TARGET_ZONE_DF_ABS_MAX_BINS, MAX_PAIRS_PER_ANCHOR,
};
// Consolidated and complete import from database module
use database::{
    Song, SongId, FingerprintDB, enroll_song, query_db_and_match, MatchResult
};

// --- Standard Library and Other IMPORTS ---
use std::collections::{HashMap, HashSet}; // HashSet is no longer used in this version of main
use std::f32::consts::PI;

// ... rest of your main.rs (CONSTS and main function) ...
// --- GLOBAL CONSTANTS (can stay here or move if appropriate) ---
const SAMPLE_RATE: u32 = 22050;
const FFT_WINDOW_SIZE: usize = 2048;
const FFT_HOPSIZE: usize = 1024;

// --- MAIN FUNCTION ---
fn main() {
    println!("Sivana Audio Fingerprinter - Enrollment and Query Mode");

    let mut fingerprint_database: FingerprintDB = HashMap::new();

    let spec_peak_params = (2, 5, 2.0f32); // neighborhood_t_r, neighborhood_f_r, min_mag_thresh
    let hashing_params = (
        TARGET_ZONE_DT_MIN_FRAMES,
        TARGET_ZONE_DT_MAX_FRAMES,
        TARGET_ZONE_DF_ABS_MAX_BINS,
        MAX_PAIRS_PER_ANCHOR,
    );

    // --- Enroll Song 0 (Original Dummy Song) ---
    println!("\n--- ENROLLING SONG 0 ---");
    let song0 = Song { id: 0, name: "Original Dummy Song (440Hz + 880Hz)".to_string() };
    let mut samples_song0: Vec<f32> = Vec::new();
    let freq1_s0 = 440.0;
    let freq2_s0 = 880.0;
    let amp_s0 = 0.3;
    for i in 0..SAMPLE_RATE {
        let time = i as f32 / SAMPLE_RATE as f32;
        let s1 = amp_s0 * (2.0 * PI * freq1_s0 * time).sin();
        let s2 = amp_s0 * 0.7 * (2.0 * PI * freq2_s0 * time).sin();
        samples_song0.push(s1 + s2);
    }
    if let Err(e) = enroll_song(&mut fingerprint_database, &song0, &samples_song0, SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE, spec_peak_params, hashing_params) {
        println!("Error enrolling Song 0: {}", e); return;
    }

    // --- Enroll Song 1 (A Different Dummy Song) ---
    println!("\n--- ENROLLING SONG 1 ---");
    let song1 = Song { id: 1, name: "Different Dummy Song (660Hz + 1100Hz)".to_string() };
    let mut samples_song1: Vec<f32> = Vec::new();
    let freq1_s1 = 660.0; // Different frequencies
    let freq2_s1 = 1100.0;
    let amp_s1 = 0.35; // Slightly different amplitude
    for i in 0..SAMPLE_RATE { // Same duration for simplicity
        let time = i as f32 / SAMPLE_RATE as f32;
        let s1 = amp_s1 * (2.0 * PI * freq1_s1 * time).sin();
        let s2 = amp_s1 * 0.6 * (2.0 * PI * freq2_s1 * time).sin();
        samples_song1.push(s1 + s2);
    }
    if let Err(e) = enroll_song(&mut fingerprint_database, &song1, &samples_song1, SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE, spec_peak_params, hashing_params) {
        println!("Error enrolling Song 1: {}", e); return;
    }
    println!("Current DB size (unique hashes after enrolling all songs): {}", fingerprint_database.len());


    // --- Prepare Query Snippet (e.g., a slice of Song 0) ---
    println!("\n--- PREPARING QUERY SNIPPET (from Song 0) ---");
    let slice_start_sample_offset = (SAMPLE_RATE as f32 * 0.2) as usize; // Start 0.2s into Song 0
    let frame_offset_approx_for_query = slice_start_sample_offset / FFT_HOPSIZE;

    if slice_start_sample_offset >= samples_song0.len() {
        println!("Query slice start point is beyond Song 0 length. Aborting query."); return;
    }
    let query_audio_slice = &samples_song0[slice_start_sample_offset..];
    println!("Query snippet from Song 0: {} samples, sliced at sample offset {}, approx frame offset {}.",
             query_audio_slice.len(), slice_start_sample_offset, frame_offset_approx_for_query);

    // Generate fingerprints for the query snippet
    let query_spectrogram = create_spectrogram(query_audio_slice, SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE);
    if query_spectrogram.is_empty() { println!("Query spectrogram failed."); return; }
    let query_peaks = find_peaks(&query_spectrogram, spec_peak_params.0, spec_peak_params.1, spec_peak_params.2);
    if query_peaks.is_empty() { println!("No peaks in query."); return; }
    let query_fingerprints = create_hashes(&query_peaks, hashing_params.0, hashing_params.1, hashing_params.2, hashing_params.3);
    if query_fingerprints.is_empty() { println!("No fingerprints for query."); return; }
    println!("Generated {} fingerprints for query snippet.", query_fingerprints.len());


    // --- Perform Matching ---
    println!("\n--- PERFORMING MATCHING ---");
    if let Some(match_result) = query_db_and_match(&fingerprint_database, &query_fingerprints) {
        println!("\n======= MATCH FOUND! =======");
        println!("Matched Song ID: {}", match_result.song_id);
        // Find song name (if we had a way to store/lookup Song objects by ID easily)
        let matched_song_name = if match_result.song_id == song0.id {
            &song0.name
        } else if match_result.song_id == song1.id {
            &song1.name
        } else {
            "Unknown Song Name"
        };
        println!("Matched Song Name: {}", matched_song_name);
        println!("Match Score: {}", match_result.score);
        println!("Calculated Time Offset in Song (frames): {}", match_result.time_offset_in_song_frames);
        println!("(Recall query snippet started approx {} frames into the original)", frame_offset_approx_for_query);
        // We expect match_result.time_offset_in_song_frames to be close to frame_offset_approx_for_query
    } else {
        println!("\n======= NO MATCH FOUND =======");
    }
}