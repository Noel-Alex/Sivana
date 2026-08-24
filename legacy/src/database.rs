// src/database.rs
use rusqlite::{Connection, OpenFlags, OptionalExtension, Result as SqlResult, params};
use std::collections::HashMap;
use std::path::Path; // Still used for histograms

// Crate-level imports
use crate::hashing::{Fingerprint, create_hashes};
use crate::peaks::find_peaks;
use crate::spectrogram::create_spectrogram;

// --- Type Aliases and Structs ---
pub type SongId = u32;

#[derive(Debug, Clone)]
pub struct Song {
    pub id: SongId,
    pub name: String,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub song_id: SongId,
    pub score: usize,
    pub time_offset_in_song_frames: isize,
}

const DB_FILE_NAME: &str = "sivana_fingerprints.sqlite";

pub fn open_db_connection() -> SqlResult<Connection> {
    open_db_connection_at(Path::new(DB_FILE_NAME))
}

/// Additive helper: same behaviour as [`open_db_connection`] but at an
/// explicit path, so benchmark harnesses can use isolated databases.
pub fn open_db_connection_at(path: &Path) -> SqlResult<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

pub fn init_db(conn: &Connection) -> SqlResult<()> {
    // init_db can take &Connection if execute_batch allows
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS songs (
             song_id INTEGER PRIMARY KEY,
             name TEXT NOT NULL,
             file_path TEXT UNIQUE,
             enrolled_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS fingerprints (
             hash INTEGER NOT NULL,
             song_id INTEGER NOT NULL,
             anchor_time_idx INTEGER NOT NULL,
             FOREIGN KEY (song_id) REFERENCES songs(song_id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS idx_fingerprints_hash ON fingerprints (hash);
         CREATE INDEX IF NOT EXISTS idx_fingerprints_song_id ON fingerprints (song_id);
         COMMIT;",
    )?;
    crate::leg_dbg!("Database '{}' initialized successfully.", DB_FILE_NAME);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn enroll_song(
    conn: &mut Connection, // <<< CHANGED TO &mut Connection HERE
    song_name: &str,
    song_file_path: Option<&str>,
    song_audio_samples: &[f32],
    sample_rate: u32,
    window_size: usize,
    hop_size: usize,
    peak_params: (usize, usize, f32),
    hash_params: (usize, usize, usize, usize),
) -> Result<SongId, String> {
    crate::leg_dbg!("Attempting to enroll song: Name='{}'", song_name);

    // It's good practice to wrap song insertion and fingerprint insertion in one transaction
    // if possible, but song insertion might need to happen first to get an ID,
    // or handle conflicts. For simplicity, we'll do song insertion, then a
    // transaction for fingerprints. More advanced: one transaction for all.

    // 1. Add song metadata to 'songs' table
    // Using a transaction for the whole enrollment might be better, but requires careful handling of last_insert_rowid
    // Let's keep song insertion separate for now to easily get last_insert_rowid,
    // and then use a transaction for the bulk fingerprint inserts.

    let preliminary_song_id_result = conn.query_row(
        "INSERT INTO songs (name, file_path) VALUES (?1, ?2)
         ON CONFLICT(file_path) DO UPDATE SET name = excluded.name, enrolled_at = CURRENT_TIMESTAMP RETURNING song_id;",
        params![song_name, song_file_path],
        |row| row.get::<_, i64>(0),
    );

    let db_song_id_i64: i64 = match preliminary_song_id_result {
        Ok(_) => conn.last_insert_rowid(),
        Err(e_insert) => {
            // If INSERT with ON CONFLICT RETURNING failed, try to SELECT the ID by file_path if provided
            if let Some(p) = song_file_path {
                match conn
                    .query_row(
                        "SELECT song_id FROM songs WHERE file_path = ?1",
                        params![p],
                        |row| row.get(0),
                    )
                    .optional()
                {
                    Ok(Some(id_val)) => id_val, // Found existing song by path
                    Ok(None) => {
                        return Err(format!(
                            "Failed to insert song '{}' and it was not found by path '{}' after conflict: {}",
                            song_name, p, e_insert
                        ));
                    }
                    Err(e_select) => {
                        return Err(format!(
                            "Failed to insert song '{}' (error: {}), and also failed to retrieve by path '{}' (error: {})",
                            song_name, e_insert, p, e_select
                        ));
                    }
                }
            } else {
                // No file_path to check for existing, and insert failed
                return Err(format!(
                    "Failed to insert song '{}' (no file_path for conflict lookup): {}",
                    song_name, e_insert
                ));
            }
        }
    };

    if db_song_id_i64 == 0 {
        // This case should ideally be caught by the RETURNING clause or the subsequent SELECT.
        // If file_path was None, and insert somehow didn't error but gave 0, it's an issue.
        return Err(format!(
            "Failed to obtain a valid database song ID for '{}'. last_insert_rowid was 0.",
            song_name
        ));
    }

    let song_id_u32 = db_song_id_i64 as SongId;
    crate::leg_dbg!(
        "Enrolling with DB Song ID: {}, Name='{}'",
        song_id_u32,
        song_name
    );

    // --- Fingerprint Generation ---
    let spectrogram = create_spectrogram(song_audio_samples, sample_rate, window_size, hop_size);
    if spectrogram.is_empty() {
        return Err(format!(
            "Failed to generate spectrogram for song ID {}",
            song_id_u32
        ));
    }

    let peaks = find_peaks(&spectrogram, peak_params.0, peak_params.1, peak_params.2);
    if peaks.is_empty() {
        return Err(format!("No peaks found for song ID {}", song_id_u32));
    }
    crate::leg_dbg!("Found {} peaks for song ID {}", peaks.len(), song_id_u32);

    let fingerprints = create_hashes(
        &peaks,
        hash_params.0,
        hash_params.1,
        hash_params.2,
        hash_params.3,
    );
    if fingerprints.is_empty() {
        return Err(format!(
            "No fingerprints generated for song ID {}",
            song_id_u32
        ));
    }
    crate::leg_dbg!(
        "Generated {} fingerprints for song ID {}",
        fingerprints.len(),
        song_id_u32
    );

    // --- Store fingerprints in DB within a transaction ---
    // conn is now &mut Connection, so conn.transaction() is valid.
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to start transaction for fingerprints: {}", e))?;
    {
        // Optimization: Clear old fingerprints for this song_id before inserting new ones if re-enrolling
        // This prevents duplicate fingerprints if a song is enrolled multiple times.
        tx.execute(
            "DELETE FROM fingerprints WHERE song_id = ?1",
            params![db_song_id_i64],
        )
        .map_err(|e| {
            format!(
                "Failed to clear old fingerprints for song ID {}: {}",
                db_song_id_i64, e
            )
        })?;

        let mut stmt = tx
            .prepare(
                "INSERT INTO fingerprints (hash, song_id, anchor_time_idx) VALUES (?1, ?2, ?3)",
            )
            .map_err(|e| format!("Failed to prepare fingerprint insert statement: {}", e))?;
        for fp in fingerprints {
            stmt.execute(params![
                fp.hash as i64,
                db_song_id_i64,
                fp.anchor_time_idx as i64
            ])
            .map_err(|e| {
                format!(
                    "Failed to insert fingerprint for song ID {}: {}",
                    db_song_id_i64, e
                )
            })?;
        }
    }
    tx.commit()
        .map_err(|e| format!("Failed to commit fingerprint transaction: {}", e))?;

    crate::leg_dbg!(
        "Successfully enrolled song: DB ID={}, Name='{}'",
        song_id_u32,
        song_name
    );
    Ok(song_id_u32)
}

#[allow(clippy::too_many_lines)]
pub fn query_db_and_match(
    conn: &Connection, // Querying only needs &Connection
    query_fingerprints: &[Fingerprint],
) -> Option<MatchResult> {
    query_db_and_match_with_threshold(conn, query_fingerprints, 100)
}

/// Additive helper: identical matching logic to [`query_db_and_match`] but
/// with the hard-coded minimum score (100) made a parameter, so benchmark
/// harnesses can evaluate short queries and rejection behaviour.
pub fn query_db_and_match_with_threshold(
    conn: &Connection,
    query_fingerprints: &[Fingerprint],
    min_match_score: usize,
) -> Option<MatchResult> {
    // ... (rest of query_db_and_match remains the same as your previous version, it was correct)
    if query_fingerprints.is_empty() {
        crate::leg_dbg!("Debug: query_db - Query has no fingerprints.");
        return None;
    }

    crate::leg_dbg!(
        "Debug: query_db - Querying with {} fingerprints.",
        query_fingerprints.len()
    );

    let mut offset_histograms: HashMap<SongId, HashMap<isize, usize>> = HashMap::new();

    let mut stmt =
        match conn.prepare("SELECT song_id, anchor_time_idx FROM fingerprints WHERE hash = ?1") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error preparing fingerprint query statement: {}", e);
                return None;
            }
        };

    for q_fp in query_fingerprints {
        let hash_i64 = q_fp.hash as i64;
        match stmt.query_map(params![hash_i64], |row| {
            Ok((
                row.get::<_, i64>(0)? as SongId,
                row.get::<_, i64>(1)? as usize,
            ))
        }) {
            Ok(db_entries_iter) => {
                for db_entry_result in db_entries_iter {
                    match db_entry_result {
                        Ok((db_song_id, db_anchor_time_idx)) => {
                            let time_offset_delta =
                                (db_anchor_time_idx as isize) - (q_fp.anchor_time_idx as isize);
                            let song_histogram = offset_histograms
                                .entry(db_song_id)
                                .or_insert_with(HashMap::new);
                            *song_histogram.entry(time_offset_delta).or_insert(0) += 1;
                        }
                        Err(e) => {
                            eprintln!("Error processing row from fingerprint query: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "Error executing fingerprint query for hash {}: {}",
                    hash_i64, e
                );
            }
        }
    }

    if offset_histograms.is_empty() {
        crate::leg_dbg!(
            "Debug: query_db - No matching hashes found in DB for any query fingerprint."
        );
        return None;
    }

    crate::leg_dbg!("\nDebug: Offset Histograms (Song ID -> <Offset Delta -> Count>):");
    for (song_id, histogram) in &offset_histograms {
        crate::leg_dbg!("  Song ID {}:", song_id);
        if histogram.is_empty() {
            crate::leg_dbg!("    (No matching offsets for this song)");
            continue;
        }
        let mut sorted_histogram: Vec<_> = histogram.iter().collect();
        sorted_histogram.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        crate::leg_dbg!(
            "    Top {} matching offsets:",
            sorted_histogram.len().min(5)
        );
        for (delta, count) in sorted_histogram.iter().take(5) {
            crate::leg_dbg!("      Delta: {: >4}, Count: {}", delta, count);
        }
        if sorted_histogram.len() > 5 {
            crate::leg_dbg!("      ... and {} more.", sorted_histogram.len() - 5);
        }
    }
    crate::leg_dbg!("--- END DEBUGGING CODE ---");

    let mut best_match_overall: Option<MatchResult> = None;
    for (song_id, histogram) in &offset_histograms {
        if let Some((best_delta_for_song, &score_for_song)) =
            histogram.iter().max_by_key(|entry| entry.1)
        {
            crate::leg_dbg!(
                "Debug: query_db - For Song ID {}: Best offset_delta {} has score {}.",
                song_id,
                best_delta_for_song,
                score_for_song
            );
            if best_match_overall
                .as_ref()
                .map_or(true, |current_best| score_for_song > current_best.score)
            {
                best_match_overall = Some(MatchResult {
                    song_id: *song_id,
                    score: score_for_song,
                    time_offset_in_song_frames: *best_delta_for_song,
                });
            }
        }
    }

    if let Some(ref result) = best_match_overall {
        if result.score < min_match_score {
            crate::leg_dbg!(
                "Debug: query_db - Best match score {} for Song ID {} is below threshold {}. Discarding.",
                result.score,
                result.song_id,
                min_match_score
            );
            return None;
        }
    }

    if best_match_overall.is_some() {
        crate::leg_dbg!(
            "Debug: query_db - Found best overall match: {:?}",
            best_match_overall.as_ref().unwrap()
        );
    } else {
        crate::leg_dbg!("Debug: query_db - No suitable match found after analyzing histograms.");
    }
    best_match_overall
}

pub fn get_song_info(conn: &Connection, song_id: SongId) -> SqlResult<Option<Song>> {
    conn.query_row(
        "SELECT song_id, name, file_path FROM songs WHERE song_id = ?1",
        params![song_id as i64],
        |row| {
            Ok(Song {
                id: row.get::<_, i64>(0)? as SongId,
                name: row.get(1)?,
                file_path: row.get(2)?,
            })
        },
    )
    .optional()
}
