// src/database.rs
use std::collections::HashMap; // Standard library imports first

// Crate-level imports
use crate::spectrogram::create_spectrogram;
use crate::peaks::find_peaks; // Grouped import for peaks
use crate::hashing::{create_hashes, Fingerprint}; // Grouped import for hashing

// --- Type Aliases and Structs ---
pub type SongId = u32;

#[derive(Debug, Clone)]
pub struct Song {
    pub id: SongId,
    pub name: String,
}

pub type FingerprintDB = HashMap<u64, Vec<(SongId, usize)>>;

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub song_id: SongId,
    pub score: usize,
    pub time_offset_in_song_frames: isize,
}

// --- Functions ---

#[allow(clippy::too_many_arguments)]
pub fn enroll_song(
    db: &mut FingerprintDB,
    song: &Song,
    song_audio_samples: &[f32],
    sample_rate: u32,
    window_size: usize,
    hop_size: usize,
    peak_params: (usize, usize, f32),
    hash_params: (usize, usize, usize, usize),
) -> Result<(), String> {
    println!("Enrolling song: ID={}, Name='{}'", song.id, song.name);

    let spectrogram = create_spectrogram(
        song_audio_samples,
        sample_rate,
        window_size,
        hop_size,
    );
    if spectrogram.is_empty() {
        return Err(format!("Failed to generate spectrogram for song {}", song.id));
    }

    let peaks = find_peaks(
        &spectrogram,
        peak_params.0,
        peak_params.1,
        peak_params.2,
    );
    if peaks.is_empty() {
        return Err(format!("No peaks found for song {}", song.id));
    }
    println!("Found {} peaks for song {}", peaks.len(), song.id);

    let fingerprints = create_hashes(
        &peaks,
        hash_params.0,
        hash_params.1,
        hash_params.2,
        hash_params.3,
    );
    if fingerprints.is_empty() {
        return Err(format!("No fingerprints generated for song {}", song.id));
    }
    println!("Generated {} fingerprints for song {}", fingerprints.len(), song.id);

    for fp in fingerprints {
        db.entry(fp.hash)
            .or_insert_with(Vec::new)
            .push((song.id, fp.anchor_time_idx));
    }

    println!("Successfully enrolled song: ID={}, Name='{}'", song.id, song.name);
    Ok(())
}

#[allow(clippy::too_many_lines)] // You might keep or remove this clippy allow
pub fn query_db_and_match(
    db: &FingerprintDB,
    query_fingerprints: &[Fingerprint],
) -> Option<MatchResult> {
    if query_fingerprints.is_empty() {
        println!("Debug: query_db - Query has no fingerprints.");
        return None;
    }
    if db.is_empty() {
        println!("Debug: query_db - Database is empty.");
        return None;
    }

    println!("Debug: query_db - Querying with {} fingerprints.", query_fingerprints.len());

    let mut offset_histograms: HashMap<SongId, HashMap<isize, usize>> = HashMap::new();

    for q_fp in query_fingerprints {
        if let Some(db_entries) = db.get(&q_fp.hash) {
            for (db_song_id, db_anchor_time_idx) in db_entries {
                let time_offset_delta = (*db_anchor_time_idx as isize) - (q_fp.anchor_time_idx as isize);
                let song_histogram = offset_histograms.entry(*db_song_id).or_insert_with(HashMap::new);
                *song_histogram.entry(time_offset_delta).or_insert(0) += 1;
            }
        }
    }

    if offset_histograms.is_empty() {
        println!("Debug: query_db - No matching hashes found in DB for any query fingerprint.");
        return None;
    }

    // --- BEGIN NEW DEBUGGING CODE ---
    println!("\nDebug: Offset Histograms (Song ID -> <Offset Delta -> Count>):");
    for (song_id, histogram) in &offset_histograms {
        println!("  Song ID {}:", song_id);
        if histogram.is_empty() {
            println!("    (No matching offsets for this song)");
            continue;
        }
        let mut sorted_histogram: Vec<_> = histogram.iter().collect();
        // Sort by count (descending), then by delta (ascending) for tie-breaking display
        sorted_histogram.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

        println!("    Top {} matching offsets:", sorted_histogram.len().min(5)); // Show up to top 5
        for (delta, count) in sorted_histogram.iter().take(5) {
            println!("      Delta: {: >4}, Count: {}", delta, count); // {: >4} for alignment
        }
        if sorted_histogram.len() > 5 {
            println!("      ... and {} more.", sorted_histogram.len() - 5);
        }
    }
    println!("--- END DEBUGGING CODE ---");
    // --- END NEW DEBUGGING CODE ---


    let mut best_match_overall: Option<MatchResult> = None;

    for (song_id, histogram) in &offset_histograms { // Iterate again for actual logic
        if let Some((best_delta_for_song, &score_for_song)) = histogram.iter().max_by_key(|entry| entry.1) {
            println!(
                "Debug: query_db - For Song ID {}: Best offset_delta {} has score {}.", // Clarified log
                song_id, best_delta_for_song, score_for_song
            );
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
        const MIN_MATCH_SCORE: usize = 3; // Tune this! Still low, okay for current test.
        if result.score < MIN_MATCH_SCORE {
            println!(
                "Debug: query_db - Best match score {} for Song ID {} is below threshold {}. Discarding.",
                result.score, result.song_id, MIN_MATCH_SCORE
            );
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