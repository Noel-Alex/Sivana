// src/database.rs
use rusqlite::{Connection, Result as SqlResult, params, OptionalExtension, OpenFlags}; // Added OpenFlags
use std::path::Path;
use std::collections::HashMap; // Still used for histograms

// Crate-level imports
use crate::spectrogram::create_spectrogram;
use crate::peaks::{find_peaks}; // Peak struct is not directly used in this file's public API anymore
use crate::hashing::{create_hashes, Fingerprint};

// --- Type Aliases and Structs ---
pub type SongId = u32; // Keep as u32, will cast from i64 from DB ROWID

#[derive(Debug, Clone)]
pub struct Song { // This struct can still be used to pass song info around
    pub id: SongId, // This ID will now be what's stored/retrieved from the DB
    pub name: String,
    pub file_path: Option<String>, // Good to have for reference
}

// FingerprintDB type alias (HashMap) is removed as we use SQLite connection directly.

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub song_id: SongId,
    pub score: usize,
    pub time_offset_in_song_frames: isize,
}

const DB_FILE_NAME: &str = "sivana_fingerprints.sqlite";

/// Opens a connection to the SQLite database file.
/// Creates the file if it doesn't exist.
pub fn open_db_connection() -> SqlResult<Connection> {
    // Ensure the flags allow creating the database if it doesn't exist.
    let conn = Connection::open_with_flags(
        Path::new(DB_FILE_NAME),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    // Enable Write-Ahead Logging for better performance and concurrency.
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

/// Initializes the database schema (creates tables if they don't exist).
pub fn init_db(conn: &Connection) -> SqlResult<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS songs (
             song_id INTEGER PRIMARY KEY, -- Autoincrements by default
             name TEXT NOT NULL,
             file_path TEXT UNIQUE,       -- Store the path for reference, make it unique
             enrolled_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS fingerprints (
             hash INTEGER NOT NULL,
             song_id INTEGER NOT NULL,
             anchor_time_idx INTEGER NOT NULL,
             FOREIGN KEY (song_id) REFERENCES songs(song_id) ON DELETE CASCADE -- If a song is deleted, its fingerprints are too
         );
         CREATE INDEX IF NOT EXISTS idx_fingerprints_hash ON fingerprints (hash);
         -- Optional: Index for quickly finding all fingerprints for a song
         CREATE INDEX IF NOT EXISTS idx_fingerprints_song_id ON fingerprints (song_id);
         COMMIT;"
    )?;
    println!("Database '{}' initialized successfully.", DB_FILE_NAME);
    Ok(())
}

/// Enrolls a new song by processing its audio samples, generating fingerprints,
/// and storing them in the database.
/// Returns the new SongId from the database.
#[allow(clippy::too_many_arguments)]
pub fn enroll_song(
    conn: &Connection,
    song_name: &str,                 // Pass name and path instead of pre-made Song struct
    song_file_path: Option<&str>,    // Optional file path for storage
    song_audio_samples: &[f32],
    sample_rate: u32,
    window_size: usize,
    hop_size: usize,
    peak_params: (usize, usize, f32),
    hash_params: (usize, usize, usize, usize),
) -> Result<SongId, String> { // Return the DB-generated SongId
    println!("Attempting to enroll song: Name='{}'", song_name);

    // 1. Add song metadata to 'songs' table (or find existing by path)
    // For simplicity, we'll try to insert and let UNIQUE constraint on file_path handle duplicates if path is provided.
    // A more robust approach might check if song (by path or name) already exists.
    let db_song_id: i64 = match conn.execute(
        "INSERT INTO songs (name, file_path) VALUES (?1, ?2)
         ON CONFLICT(file_path) DO UPDATE SET name = excluded.name RETURNING song_id;", // Update name if path exists, and return ID
        params![song_name, song_file_path],
    ) {
        Ok(_) => conn.last_insert_rowid(), // If new insert
        Err(e) => { // If INSERT failed (e.g. not due to conflict if ON CONFLICT wasn't perfect)
            // Try to select existing if it was a conflict on file_path that wasn't handled by RETURNING
            if let Some(p) = song_file_path {
                match conn.query_row(
                    "SELECT song_id FROM songs WHERE file_path = ?1",
                    params![p],
                    |row| row.get(0),
                ).optional() { // Use optional() to handle no row found gracefully
                    Ok(Some(id_val)) => id_val,
                    Ok(None) => return Err(format!("Failed to insert or find song '{}' by path and no ID returned: {}", song_name, e)),
                    Err(select_e) => return Err(format!("Failed to insert song '{}' and failed to select existing by path: {} (select error: {})", song_name, e, select_e)),
                }
            } else {
                // If no file_path, we can't easily check for duplicates this way, rely on insert or error.
                // This path might need a strategy if duplicate names without paths are an issue.
                // For now, assume insert or use last_insert_rowid if it succeeded despite error (unlikely).
                return Err(format!("Failed to insert song '{}' (no file_path for conflict check): {}", song_name, e));
            }
        }
    };

    // If last_insert_rowid is 0 (e.g. ON CONFLICT DO NOTHING and no RETURNING), try to SELECT
    let final_song_id_db: i64 = if db_song_id == 0 && song_file_path.is_some() {
        conn.query_row(
            "SELECT song_id FROM songs WHERE file_path = ?1",
            params![song_file_path.unwrap()], // Safe to unwrap due to is_some()
            |row| row.get(0)
        ).map_err(|e| format!("Failed to get song_id after presumed conflict for '{}': {}", song_name, e))?
    } else if db_song_id == 0 && song_file_path.is_none() {
        // This case is problematic: no path to look up, and insert didn't yield an ID.
        // This would only happen with "ON CONFLICT DO NOTHING" and no path.
        // Our current "ON CONFLICT DO UPDATE ... RETURNING" should avoid this.
        return Err(format!("Failed to obtain a valid song_id for '{}' (no file_path and insert yielded 0).", song_name));
    } else {
        db_song_id
    };


    let song_id_u32 = final_song_id_db as SongId; // Cast to u32
    println!("Enrolling with DB Song ID: {}, Name='{}'", song_id_u32, song_name);


    // 2. Fingerprint Generation (same as before)
    let spectrogram = create_spectrogram(song_audio_samples, sample_rate, window_size, hop_size);
    if spectrogram.is_empty() { return Err(format!("Failed to generate spectrogram for song ID {}", song_id_u32)); }

    let peaks = find_peaks(&spectrogram, peak_params.0, peak_params.1, peak_params.2);
    if peaks.is_empty() { return Err(format!("No peaks found for song ID {}", song_id_u32)); }
    println!("Found {} peaks for song ID {}", peaks.len(), song_id_u32);

    let fingerprints = create_hashes(&peaks, hash_params.0, hash_params.1, hash_params.2, hash_params.3);
    if fingerprints.is_empty() { return Err(format!("No fingerprints generated for song ID {}", song_id_u32)); }
    println!("Generated {} fingerprints for song ID {}", fingerprints.len(), song_id_u32);

    // 3. Store fingerprints in DB within a transaction
    let mut tx = conn.transaction().map_err(|e| format!("Failed to start transaction: {}", e))?;
    { // Scope for prepared statement
        let mut stmt = tx.prepare("INSERT INTO fingerprints (hash, song_id, anchor_time_idx) VALUES (?1, ?2, ?3)")
            .map_err(|e| format!("Failed to prepare fingerprint insert statement: {}", e))?;
        for fp in fingerprints {
            stmt.execute(params![fp.hash as i64, final_song_id_db, fp.anchor_time_idx as i64]) // Use i64 for DB
                .map_err(|e| format!("Failed to insert fingerprint for song ID {}: {}", final_song_id_db, e))?;
        }
    } // Statement is dropped here
    tx.commit().map_err(|e| format!("Failed to commit transaction: {}", e))?;

    println!("Successfully enrolled song: DB ID={}, Name='{}'", song_id_u32, song_name);
    Ok(song_id_u32)
}

/// Queries the database for matching fingerprints and determines the best song match.
#[allow(clippy::too_many_lines)]
pub fn query_db_and_match(
    conn: &Connection, // Takes a DB connection
    query_fingerprints: &[Fingerprint],
    // We no longer need to pass the full DB Song struct here, just the connection
) -> Option<MatchResult> {
    if query_fingerprints.is_empty() {
        println!("Debug: query_db - Query has no fingerprints.");
        return None;
    }
    // We don't check if DB is empty here, query will just return no results.

    println!("Debug: query_db - Querying with {} fingerprints.", query_fingerprints.len());

    let mut offset_histograms: HashMap<SongId, HashMap<isize, usize>> = HashMap::new();

    // Prepare statement for querying fingerprints
    let mut stmt = match conn.prepare("SELECT song_id, anchor_time_idx FROM fingerprints WHERE hash = ?1") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error preparing fingerprint query statement: {}", e);
            return None;
        }
    };

    for q_fp in query_fingerprints {
        let hash_i64 = q_fp.hash as i64; // Hashes are stored as i64 in DB potentially
        match stmt.query_map(params![hash_i64], |row| {
            Ok((row.get::<_, i64>(0)? as SongId, row.get::<_, i64>(1)? as usize)) // song_id (u32), anchor_time_idx (usize)
        }) {
            Ok(db_entries_iter) => {
                for db_entry_result in db_entries_iter {
                    match db_entry_result {
                        Ok((db_song_id, db_anchor_time_idx)) => {
                            let time_offset_delta = (db_anchor_time_idx as isize) - (q_fp.anchor_time_idx as isize);
                            let song_histogram = offset_histograms.entry(db_song_id).or_insert_with(HashMap::new);
                            *song_histogram.entry(time_offset_delta).or_insert(0) += 1;
                        }
                        Err(e) => {
                            eprintln!("Error processing row from fingerprint query: {}", e);
                            // Optionally continue or return None
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error executing fingerprint query for hash {}: {}", hash_i64, e);
                // Optionally continue or return None
            }
        }
    }

    if offset_histograms.is_empty() {
        println!("Debug: query_db - No matching hashes found in DB for any query fingerprint.");
        return None;
    }

    // --- Histogram Debugging (same as before) ---
    println!("\nDebug: Offset Histograms (Song ID -> <Offset Delta -> Count>):");
    for (song_id, histogram) in &offset_histograms {
        println!("  Song ID {}:", song_id);
        if histogram.is_empty() { println!("    (No matching offsets for this song)"); continue; }
        let mut sorted_histogram: Vec<_> = histogram.iter().collect();
        sorted_histogram.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        println!("    Top {} matching offsets:", sorted_histogram.len().min(5));
        for (delta, count) in sorted_histogram.iter().take(5) {
            println!("      Delta: {: >4}, Count: {}", delta, count);
        }
        if sorted_histogram.len() > 5 { println!("      ... and {} more.", sorted_histogram.len() - 5); }
    }
    println!("--- END DEBUGGING CODE ---");

    let mut best_match_overall: Option<MatchResult> = None;
    for (song_id, histogram) in &offset_histograms {
        if let Some((best_delta_for_song, &score_for_song)) = histogram.iter().max_by_key(|entry| entry.1) {
            println!("Debug: query_db - For Song ID {}: Best offset_delta {} has score {}.", song_id, best_delta_for_song, score_for_song);
            if best_match_overall.as_ref().map_or(true, |current_best| score_for_song > current_best.score) {
                best_match_overall = Some(MatchResult {
                    song_id: *song_id,
                    score: score_for_song,
                    time_offset_in_song_frames: *best_delta_for_song,
                });
            }
        }
    }

    if let Some(ref result) = best_match_overall {
        const MIN_MATCH_SCORE: usize = 100; // Keep your tuned threshold
        if result.score < MIN_MATCH_SCORE {
            println!("Debug: query_db - Best match score {} for Song ID {} is below threshold {}. Discarding.", result.score, result.song_id, MIN_MATCH_SCORE);
            return None;
        }
    }

    if best_match_overall.is_some() {
        println!("Debug: query_db - Found best overall match: {:?}", best_match_overall.as_ref().unwrap());
    } else {
        println!("Debug: query_db - No suitable match found after analyzing histograms.");
    }
    best_match_overall
}

// Optional helper function to retrieve song metadata if needed elsewhere
pub fn get_song_info(conn: &Connection, song_id: SongId) -> SqlResult<Option<Song>> {
    conn.query_row(
        "SELECT song_id, name, file_path FROM songs WHERE song_id = ?1",
        params![song_id as i64], // Query with i64 if that's the DB PK type
        |row| {
            Ok(Song {
                id: row.get::<_, i64>(0)? as SongId, // Cast back to u32
                name: row.get(1)?,
                file_path: row.get(2)?,
            })
        },
    ).optional() // Makes it return Ok(None) if no row found, instead of Err
}