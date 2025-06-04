// src/main.rs

// Declare modules
mod spectrogram;
mod peaks;
mod hashing;
mod database;
mod audio_loader;

// --- IMPORTS ---
use crate::audio_loader::load_audio_file;
use crate::database::{
    open_db_connection, init_db, enroll_song, query_db_and_match, get_song_info,
    SongId, // MatchResult is used internally by query_db_and_match
};
use crate::hashing::{create_hashes, MAX_PAIRS_PER_ANCHOR, TARGET_ZONE_DF_ABS_MAX_BINS, TARGET_ZONE_DT_MAX_FRAMES, TARGET_ZONE_DT_MIN_FRAMES};
use crate::peaks::find_peaks;
use crate::spectrogram::create_spectrogram;

use std::path::PathBuf; // For path arguments from clap
use clap::Parser;     // For CLI argument parsing

// --- GLOBAL CONSTANTS ---
const SAMPLE_RATE: u32 = 22050;
const FFT_WINDOW_SIZE: usize = 2048;
const FFT_HOPSIZE: usize = 1024;

// --- Define CLI Arguments and Subcommands ---

#[derive(Parser, Debug)]
#[command(author, version, about = "Sivana Audio Fingerprinter", long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Parser, Debug)]
enum Commands {
    /// Enroll a new song into the fingerprint database
    Enroll {
        /// Path to the audio file to enroll
        #[arg(value_name = "FILE_PATH")]
        file_path: PathBuf,

        /// Optional display name/title for the song. If not provided, filename is used.
        #[arg(long, short)]
        title: Option<String>,
    },
    /// Query the database with an audio snippet to identify a song
    Query {
        /// Path to the audio snippet file
        #[arg(value_name = "SNIPPET_PATH")]
        snippet_path: PathBuf,
    },
    /// List all songs currently enrolled in the database
    List,
    // TODO: Consider adding DeleteSong, DbInfo, ClearDb commands later
}

// --- MAIN FUNCTION ---
fn main() -> Result<(), String> {
    let cli_args = Cli::parse();

    // --- Initialize Database Connection (common to most commands) ---
    // Make conn mutable as enroll_song needs it
    let mut conn = open_db_connection()
        .map_err(|e| format!("Failed to open/create database: {}", e))?;

    // init_db should be safe to call every time; it uses "IF NOT EXISTS"
    init_db(&conn)
        .map_err(|e| format!("Failed to initialize database tables: {}", e))?;

    // --- Parameters (could be loaded from config or become CLI options later) ---
    let spec_peak_params = (2, 5, 2.0f32); // (time_radius, freq_radius, min_magnitude_threshold)
    let hashing_params = (
        TARGET_ZONE_DT_MIN_FRAMES,
        TARGET_ZONE_DT_MAX_FRAMES,
        TARGET_ZONE_DF_ABS_MAX_BINS,
        MAX_PAIRS_PER_ANCHOR,
    );

    // Match on the parsed subcommand
    match cli_args.command {
        Commands::Enroll { file_path, title } => {
            println!("Enroll command received for: {}", file_path.display());

            if !file_path.exists() {
                return Err(format!("Enroll error: File not found at '{}'", file_path.display()));
            }

            let song_name = title.unwrap_or_else(|| {
                file_path.file_stem()
                    .unwrap_or_default() // Use empty string if no stem
                    .to_string_lossy()
                    .into_owned()
            });
            let file_path_str = file_path.to_str()
                .ok_or_else(|| format!("Invalid file path string for: {}", file_path.display()))?;

            match load_audio_file(&file_path, SAMPLE_RATE) {
                Ok(samples) => {
                    if samples.is_empty() {
                        return Err(format!("No audio samples loaded from '{}'. File might be empty, unsupported, or corrupted.", file_path.display()));
                    }
                    println!("Loaded {} samples for '{}'.", samples.len(), song_name);

                    match enroll_song(
                        &mut conn, // Pass mutable connection
                        &song_name,
                        Some(file_path_str),
                        &samples,
                        SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE,
                        spec_peak_params, hashing_params
                    ) {
                        Ok(db_song_id) => {
                            println!("Successfully enrolled '{}' with DB Song ID: {}.", song_name, db_song_id);
                            println!("File path stored: {}", file_path_str);
                        }
                        Err(e) => {
                            return Err(format!("Error during enrollment process for '{}': {}", song_name, e));
                        }
                    }
                }
                Err(e) => {
                    return Err(format!("Error loading audio file '{}': {}", file_path.display(), e));
                }
            }
        }
        Commands::Query { snippet_path } => {
            println!("Query command received for snippet: {}", snippet_path.display());

            if !snippet_path.exists() {
                return Err(format!("Query error: Snippet file not found at '{}'", snippet_path.display()));
            }

            match load_audio_file(&snippet_path, SAMPLE_RATE) {
                Ok(query_samples) => {
                    if query_samples.is_empty() {
                        return Err(format!("No audio samples loaded from snippet '{}'.", snippet_path.display()));
                    }
                    println!("Loaded {} samples for query snippet.", query_samples.len());

                    let query_spectrogram = create_spectrogram(&query_samples, SAMPLE_RATE, FFT_WINDOW_SIZE, FFT_HOPSIZE);
                    if query_spectrogram.is_empty() { println!("Warning: Query spectrogram is empty. This might lead to no match."); }

                    let query_peaks = find_peaks(&query_spectrogram, spec_peak_params.0, spec_peak_params.1, spec_peak_params.2);
                    if query_peaks.is_empty() { println!("Warning: No peaks found in query snippet. This might lead to no match."); }

                    let query_fingerprints = create_hashes(&query_peaks, hashing_params.0, hashing_params.1, hashing_params.2, hashing_params.3);
                    if query_fingerprints.is_empty() { println!("Warning: No fingerprints generated for query snippet. This might lead to no match."); }
                    println!("Generated {} fingerprints for query snippet.", query_fingerprints.len());

                    if query_fingerprints.is_empty() {
                        println!("\n======= NO FINGERPRINTS GENERATED FOR QUERY, CANNOT MATCH =======");
                        return Ok(());
                    }

                    if let Some(match_result) = query_db_and_match(&conn, &query_fingerprints) {
                        println!("\n======= MATCH FOUND! =======");

                        // Fetch full song info for better display
                        match get_song_info(&conn, match_result.song_id) {
                            Ok(Some(song_info)) => {
                                println!("Matched Song ID: {}", song_info.id);
                                println!("Matched Song Name: {}", song_info.name);
                                if let Some(path) = song_info.file_path {
                                    println!("Original File Path: {}", path);
                                }
                            }
                            Ok(None) => {
                                println!("Matched Song ID: {} (but metadata not found in 'songs' table!)", match_result.song_id);
                            }
                            Err(e) => {
                                println!("Matched Song ID: {} (error fetching full info: {})", match_result.song_id, e);
                            }
                        }

                        println!("Match Score: {}", match_result.score);
                        println!("Calculated Time Offset in Song (frames): {}", match_result.time_offset_in_song_frames);
                        let offset_seconds = (match_result.time_offset_in_song_frames as f32 * FFT_HOPSIZE as f32) / SAMPLE_RATE as f32;
                        println!("(Approx. offset in matched song: {:.2} seconds)", offset_seconds);

                    } else {
                        println!("\n======= NO MATCH FOUND =======");
                    }
                }
                Err(e) => {
                    return Err(format!("Error loading audio snippet '{}': {}", snippet_path.display(), e));
                }
            }
        }
        Commands::List => {
            println!("\n--- Enrolled Songs in Database ---");
            let mut stmt = conn.prepare("SELECT song_id, name, file_path, enrolled_at FROM songs ORDER BY name ASC")
                .map_err(|e| format!("Failed to prepare statement to list songs: {}", e))?;

            let song_iter = stmt.query_map([], |row| {
                // Directly use database::Song if its fields match and it's pub
                Ok(crate::database::Song {
                    id: row.get::<_, i64>(0)? as SongId, // Assuming SongId is u32
                    name: row.get(1)?,
                    file_path: row.get(2)?,
                    // enrolled_at: row.get(3)?, // Needs chrono feature for rusqlite for DATETIME
                })
            }).map_err(|e| format!("Failed to query songs: {}", e))?;

            let mut count = 0;
            for song_result in song_iter {
                match song_result {
                    Ok(song) => {
                        count += 1;
                        print!("ID: {:<4} | Name: {:<40} | Path: ", song.id, song.name);
                        if let Some(path) = song.file_path {
                            print!("{}", path);
                        } else {
                            print!("N/A");
                        }
                        // To print enrolled_at, you'd need to handle its type (likely String or a DateTime type if using chrono)
                        // println!(" | Enrolled: {}", row.get::<_, String>(3)?);
                        println!(); // Newline
                    }
                    Err(e) => {
                        eprintln!("Error fetching song row: {}", e);
                    }
                }
            }
            if count == 0 {
                println!("No songs found in the database.");
            } else {
                println!("--- Listed {} songs. ---", count);
            }
        }
    }

    Ok(())
}