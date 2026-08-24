//! Corpus construction: deterministic synthetic tracks (§55).
//!
//! Tracks are generated in memory from a root seed; the same root seed
//! always produces the same catalog, on any machine.

use sivana_audio::fixtures;
use std::path::Path;

pub struct CorpusTrack {
    pub recording: sivana_core::RecordingId,
    pub name: String,
    pub samples: Vec<f32>,
}

pub struct Corpus {
    pub tracks: Vec<CorpusTrack>,
    pub sample_rate: u32,
    pub duration_s: f32,
    pub seed: u64,
}

/// Generate `n_tracks` synthetic songs.
pub fn generate(n_tracks: usize, duration_s: f32, sample_rate: u32, seed: u64) -> Corpus {
    let mut tracks = Vec::with_capacity(n_tracks);
    for i in 0..n_tracks {
        let track_seed = seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let samples = fixtures::synth_song(track_seed, duration_s, sample_rate);
        tracks.push(CorpusTrack {
            recording: sivana_core::RecordingId::new(i as u32),
            name: format!("synthetic-{seed}-{i}"),
            samples,
        });
    }
    Corpus {
        tracks,
        sample_rate,
        duration_s,
        seed,
    }
}

/// Write the corpus as WAV files (for golden tests / cross-platform runs).
pub fn write_wav_files(corpus: &Corpus, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for t in &corpus.tracks {
        let path = dir.join(format!("{}.wav", t.name));
        sivana_audio::wav::write_wav(&path, corpus.sample_rate, &t.samples)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }
    Ok(())
}
