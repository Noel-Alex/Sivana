// src/main.rs

// Declare modules
mod spectrogram;
mod peaks;
mod hashing;
mod database;
mod audio_loader;

// --- IMPORTS from our modules ---
use crate::audio_loader::load_audio_file;
// Import the new DB functions and necessary structs/types
use crate::database::{
    open_db_connection, init_db, enroll_song, query_db_and_match,
    SongId, MatchResult
};
use crate::hashing::{create_hashes, MAX_PAIRS_PER_ANCHOR, TARGET_ZONE_DF_ABS_MAX_BINS, TARGET_ZONE_DT_MAX_FRAMES, TARGET_ZONE_DT_MIN_FRAMES};
use crate::peaks::find_peaks;
use crate::spectrogram::create_spectrogram;

// --- Standard Library and Other IMPORTS ---
use std::collections::HashMap;
use std::path::Path;

// --- GLOBAL CONSTANTS ---
const SAMPLE_RATE: u32 = 22050;
const FFT_WINDOW_SIZE: usize = 2048;
const FFT_HOPSIZE: usize = 1024;

// --- MAIN FUNCTION ---
fn main() -> Result<(), String> {
    println!("Sivana Audio Fingerprinter - Persistent DB Mode - Testing with 11 Songs");

    // --- Initialize Database Connection ---
    // Make conn mutable so we can pass &mut conn to enroll_song
    let mut conn = open_db_connection() // <<< CHANGED: let conn -> let mut conn
        .map_err(|e| format!("Failed to open/create database: {}", e))?;
    init_db(&conn) // init_db takes &Connection, this is fine
        .map_err(|e| format!("Failed to initialize database tables: {}", e))?;

    // --- Parameters ---
    let spec_peak_params = (2, 5, 2.0f32);
    let hashing_params = (
        TARGET_ZONE_DT_MIN_FRAMES,
        TARGET_ZONE_DT_MAX_FRAMES,
        TARGET_ZONE_DF_ABS_MAX_BINS,
        MAX_PAIRS_PER_ANCHOR,
    );

    // --- List of Songs to Process (UPDATE THESE PATHS AND DESCRIPTIONS) ---
    let song_processing_list = vec![
        ("data/1.mp3", "Song 01 - [Artist A - Title X]"),
        ("data/2.mp3", "Song 02 - [Artist A - Title Y]"),
        ("data/3.mp3", "Song 03 - [Artist B - Title Z]"),
        ("data/4.mp3", "Song 04 - [Artist C - Title P]"),
        ("data/5.mp3", "Song 05 - [Artist D - Title Q]"),
        ("data/6.mp3", "Song 06 - [Artist E - Title R]"),
        ("data/7.mp3", "Song 07 - [Artist F - Title S]"),
        ("data/8.mp3", "Song 08 - [Artist G - Title T]"),
        ("data/9.mp3", "Song 09 - [Artist H - Title U]"),
        ("data/10.mp3", "Song 10 - [Artist I - Title V]"),
        ("data/11.mp3", "Song 11 - [Artist J - Title W]"),
    ];

    if song_processing_list.len() != 11 {
        println!("Warning: Expected 11 songs in song_processing_list, found {}. Please update the list.", song_processing_list.len());
    }

    let mut enrolled_song_data: Vec<(SongId, String, Vec<f32>)> = Vec::new();

    // --- Enroll All Songs ---
    println!("\n--- Starting Song Enrollment Process ---");
    for (path_str, display_name) in &song_processing_list {
        println!("\n--- Processing enrollment for: '{}' ---", display_name);

        let song_path = Path::new(path_str);
        if !song_path.exists() {
            eprintln!("!ERROR: Audio file '{}' not found. Skipping.", path_str);
            continue;
        }

        match load_audio_file(song_path, SAMPLE_RATE) {
            Ok(samples) => {
                if samples.is_empty() {
                    eprintln!("!ERROR: No samples loaded for '{}'. Skipping.", display_name);
                    continue;
                }
                println!("Loaded {} samples for '{}'.", samples.len(), display_name);

                match enroll_song(
                    &mut conn, // <<< CHANGED: Pass &mut conn
                    display_name,
                    Some(path_str),
                    &samples,
                    SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE,
                    spec_peak_params, hashing_params
                ) {
                    Ok(db_song_id) => {
                        println!("Successfully enrolled '{}' with DB Song ID: {}", display_name, db_song_id);
                        enrolled_song_data.push((db_song_id, display_name.to_string(), samples));
                    }
                    Err(e) => {
                        eprintln!("!ERROR enrolling '{}': {}", display_name, e);
                    }
                }
            }
            Err(e) => {
                eprintln!("!ERROR loading '{}' from path '{}': {}", display_name, path_str, e);
            }
        }
    }
    println!("\n--- All Enrollments Attempted ---");
    match conn.query_row("SELECT COUNT(*) FROM songs", [], |row| row.get::<_, i64>(0)) {
        Ok(count) => println!("Total songs in DB 'songs' table: {}", count),
        Err(e) => println!("Could not get song count from DB: {}", e),
    }

    if enrolled_song_data.is_empty() {
        return Err("No songs were successfully loaded and enrolled in this session. Aborting tests.".to_string());
    }

    // --- Define Queries ---
    #[derive(Debug)]
    struct QueryTest<'a> {
        query_source_song_idx: usize,
        expected_match_song_idx: usize,
        description: &'a str,
        snippet_start_seconds: f32,
        snippet_duration_seconds: f32,
    }

    let queries_to_run = vec![
        QueryTest { query_source_song_idx: 0, expected_match_song_idx: 0, description: "Snippet from Song 01 (idx 0)", snippet_start_seconds: 30.0, snippet_duration_seconds: 10.0 },
        QueryTest { query_source_song_idx: 1, expected_match_song_idx: 1, description: "Snippet from Song 02 (idx 1)", snippet_start_seconds: 45.0, snippet_duration_seconds: 7.0 },
        QueryTest { query_source_song_idx: 2, expected_match_song_idx: 2, description: "Snippet from Song 03 (idx 2)", snippet_start_seconds: 60.0, snippet_duration_seconds: 10.0 },
        // Add queries for idx 3 through 10 if you have 11 songs and they all enrolled successfully
        // Example: QueryTest { query_source_song_idx: 10, expected_match_song_idx: 10, description: "Snippet from Song 11 (idx 10)", snippet_start_seconds: 55.0, snippet_duration_seconds: 5.0 },
    ];

    // --- Run All Queries ---
    for test_query in &queries_to_run {
        let (query_db_song_id, query_song_name, source_samples) =
            match enrolled_song_data.get(test_query.query_source_song_idx) {
                Some(data) => (data.0, &data.1, &data.2),
                None => {
                    eprintln!("!WARNING: Query source index {} is out of bounds for enrolled songs ({} available). Skipping query: '{}'.",
                              test_query.query_source_song_idx, enrolled_song_data.len(), test_query.description);
                    continue;
                }
            };

        let expected_db_song_id =
            match enrolled_song_data.get(test_query.expected_match_song_idx) {
                Some(data) => data.0,
                None => {
                    eprintln!("!WARNING: Expected match index {} is out of bounds for enrolled songs. Cannot determine expected DB ID for query: '{}'. Skipping.",
                              test_query.expected_match_song_idx, test_query.description);
                    continue;
                }
            };

        println!("\n--- RUNNING QUERY: {} (Source: '{}' [DB ID {}], Expected Match: Song at idx {} [DB ID {}]) ---",
                 test_query.description, query_song_name, query_db_song_id,
                 test_query.expected_match_song_idx, expected_db_song_id);

        let slice_start_sample_offset = (SAMPLE_RATE as f32 * test_query.snippet_start_seconds) as usize;
        let slice_end_sample_offset = slice_start_sample_offset + (SAMPLE_RATE as f32 * test_query.snippet_duration_seconds) as usize;
        let frame_offset_approx_for_query = slice_start_sample_offset / FFT_HOPSIZE;

        if slice_start_sample_offset >= source_samples.len() { eprintln!("!WARNING: Query slice start for '{}' ... Skipping.", test_query.description); continue; }
        let actual_slice_end = slice_end_sample_offset.min(source_samples.len());
        if slice_start_sample_offset >= actual_slice_end { eprintln!("!WARNING: Query slice for '{}' would be empty. Skipping.", test_query.description); continue; }
        let query_audio_slice = &source_samples[slice_start_sample_offset..actual_slice_end];
        if query_audio_slice.is_empty() { eprintln!("!WARNING: Query audio slice for '{}' is empty. Skipping.", test_query.description); continue; }

        println!("Query snippet: {} samples, approx frame offset {}.", query_audio_slice.len(), frame_offset_approx_for_query);
        let query_spectrogram = create_spectrogram(query_audio_slice, SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE);
        if query_spectrogram.is_empty() { println!("Warning: Query spectrogram empty for '{}'.", test_query.description); }
        let query_peaks = find_peaks(&query_spectrogram, spec_peak_params.0, spec_peak_params.1, spec_peak_params.2);
        if query_peaks.is_empty() { println!("Warning: No peaks in query for '{}'.", test_query.description); }
        let query_fingerprints = create_hashes(&query_peaks, hashing_params.0, hashing_params.1, hashing_params.2, hashing_params.3);
        if query_fingerprints.is_empty() { println!("Warning: No fingerprints for query for '{}'.", test_query.description); }
        println!("Generated {} fingerprints for query '{}'.", query_fingerprints.len(), test_query.description);

        if query_fingerprints.is_empty() {
            println!("\n======= NO FINGERPRINTS GENERATED FOR QUERY '{}' =======", test_query.description);
            if enrolled_song_data.iter().any(|(id, _, _)| *id == expected_db_song_id) {
                println!("##### VERDICT: POTENTIAL FALSE NEGATIVE (No query fingerprints to match with an enrolled song) #####");
            } else {
                println!("##### VERDICT: CORRECTLY NO MATCH (Query yielded no fingerprints, expected song not enrolled/available) #####");
            }
            continue;
        }

        if let Some(match_result) = query_db_and_match(&conn, &query_fingerprints) { // Pass &conn (immutable)
            println!("\n======= MATCH RESULT FOR '{}' =======", test_query.description);
            let matched_song_name_from_db = database::get_song_info(&conn, match_result.song_id)
                .ok().flatten()
                .map_or_else(|| format!("Unknown DB Song ID {}", match_result.song_id), |s| s.name);

            println!("Matched Song DB ID: {} (Expected DB ID: {})", match_result.song_id, expected_db_song_id);
            println!("Matched Song Name: {}", matched_song_name_from_db);
            println!("Match Score: {}", match_result.score);
            println!("Calculated Time Offset in Song (frames): {}", match_result.time_offset_in_song_frames);
            println!("(Query snippet started approx {} frames into source at {} seconds)", frame_offset_approx_for_query, test_query.snippet_start_seconds);

            if match_result.song_id == expected_db_song_id {
                println!("##### VERDICT: CORRECT MATCH! #####");
            } else {
                let expected_song_name_from_list = enrolled_song_data.iter()
                    .find(|(id,_,_)| *id == expected_db_song_id)
                    .map_or("Unknown expected song", |(_,name,_)| name.as_str());
                println!("##### VERDICT: INCORRECT MATCH! (Expected '{}' [DB ID {}], Got '{}' [DB ID {}]) #####",
                         expected_song_name_from_list, expected_db_song_id,
                         matched_song_name_from_db, match_result.song_id);
            }
        } else {
            println!("\n======= NO MATCH FOUND FOR '{}' =======", test_query.description);
            if enrolled_song_data.iter().any(|(id, _, _)| *id == expected_db_song_id) {
                let expected_song_name_from_list = enrolled_song_data.iter()
                    .find(|(id,_,_)| *id == expected_db_song_id)
                    .map_or("Unknown expected song", |(_,name,_)| name.as_str());
                println!("##### VERDICT: POTENTIAL FALSE NEGATIVE (Expected to find '{}' [DB ID {}]) #####", expected_song_name_from_list, expected_db_song_id);
            } else {
                println!("##### VERDICT: CORRECTLY NO MATCH (Expected song not enrolled/available or query had no fingerprints) #####");
            }
        }
    }
    println!("\n--- All Queries Processed ---");
    Ok(())
}