//! Catalog ingestion platform (PLAN §41, §84; Phase 6).
//!
//! Pipeline: source bytes -> SHA-256 -> dedup -> decode (symphonia) ->
//! resample to 22.05 kHz mono -> fingerprint (Landmark V2, E4 operating
//! point: 512 bands) -> SFP1 sidecar + .siv delta segment -> manifest
//! atomic swap.
//!
//! Idempotent: a source hash already present in `sources.json` is skipped,
//! so re-running an ingest job is safe. Catalogs grow by immutable delta
//! segments; [`compact`] merges them into one and rewrites the manifest.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sivana_core::{FingerprintVersion, RecordingId};
use sivana_index::manifest::{self, Manifest};
use sivana_landmark::{Fingerprint32, LandmarkV2Config};
use sivana_wasm::encode_batch;

/// Target sample rate for all fingerprinting (Phase 0 decision).
pub const TARGET_SAMPLE_RATE: u32 = 22_050;

/// E4-calibrated band count, defined once in core so every engine and
/// the catalog agree.
pub const FREQ_BANDS: u16 = sivana_core::OPERATING_FREQ_BANDS;

/// On-disk catalog state beyond the index segments.
#[derive(Default, Serialize, Deserialize)]
pub struct CatalogState {
    /// source content hash -> recording id (dedup map).
    pub sources: HashMap<String, u32>,
    /// Highest recording id issued so far.
    pub next_recording_id: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct RecordingMetadata {
    pub title: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_provider: Option<String>,
}

fn load_json<T>(path: &Path) -> Option<T>
where
    T: for<'a> serde::de::DeserializeOwned,
{
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    write_atomic(path, &bytes).map_err(|e| e.to_string())
}

pub struct IngestStats {
    pub added: Vec<u32>,
    pub existing: Vec<u32>,
    pub skipped: usize,
    pub failed: Vec<(String, String)>,
    pub segment: Option<PathBuf>,
    pub postings: usize,
}

/// One file processed on a worker thread: everything except catalog
/// mutation, which stays on the coordinator thread.
struct Ingested {
    hash: String,
    fps: Vec<Fingerprint32>,
    title: String,
}

fn ingest_one(path: &Path, cfg: &LandmarkV2Config) -> Result<Ingested, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = format!("{:x}", hasher.finalize());

    let (mono, native_sr) = sivana_audio::decode::decode_mono(&bytes)?;
    let pcm = if native_sr == TARGET_SAMPLE_RATE {
        mono
    } else {
        sivana_dsp::resample::resample_sinc(&mono, native_sr, TARGET_SAMPLE_RATE)
    };
    let fps = sivana_landmark::fingerprint(&pcm, TARGET_SAMPLE_RATE, cfg);
    if fps.is_empty() {
        return Err("no fingerprints produced".into());
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string();
    Ok(Ingested { hash, fps, title })
}

/// Ingest audio files into the catalog at `dir`.
///
/// Files are decoded/fingerprinted on the rayon thread pool; successful
/// files become fingerprint sidecars plus postings in one new delta
/// segment, followed by an atomic manifest swap.
pub fn add_files(dir: &Path, files: &[PathBuf], jobs: usize) -> Result<IngestStats, String> {
    let requests = files
        .iter()
        .cloned()
        .map(|path| (path, None))
        .collect::<Vec<_>>();
    add_files_with_metadata(dir, &requests, jobs)
}

/// Ingest audio files while attaching authoritative external metadata.
///
/// Metadata is stored alongside the recording id in `recordings.json`. A
/// duplicate source updates its metadata without rebuilding its fingerprints.
pub fn add_files_with_metadata(
    dir: &Path,
    files: &[(PathBuf, Option<RecordingMetadata>)],
    jobs: usize,
) -> Result<IngestStats, String> {
    std::fs::create_dir_all(dir.join("fingerprints")).map_err(|e| e.to_string())?;
    let state: CatalogState = load_json(&dir.join("sources.json")).unwrap_or_default();
    let mut metas: HashMap<String, RecordingMetadata> =
        load_json(&dir.join("recordings.json")).unwrap_or_default();
    let sources = state.sources.clone();

    let cfg = LandmarkV2Config {
        freq_bands: FREQ_BANDS,
        ..Default::default()
    };

    // Dedup check needs only the hash, cheap before spawning heavy work;
    // already-present sources are skipped up front.
    let mut jobs_in = Vec::with_capacity(files.len());
    let mut stats = IngestStats {
        added: Vec::new(),
        existing: Vec::new(),
        skipped: 0,
        failed: Vec::new(),
        segment: None,
        postings: 0,
    };
    for (path, metadata) in files {
        match hash_file(path) {
            Ok(h) if state.sources.contains_key(&h) => {
                let rec_id = state.sources[&h];
                stats.skipped += 1;
                stats.existing.push(rec_id);
                if let Some(metadata) = metadata {
                    metas.insert(rec_id.to_string(), metadata.clone());
                }
            }
            Ok(_) => jobs_in.push((path.clone(), metadata.clone())),
            Err(e) => stats.failed.push((path.display().to_string(), e)),
        }
    }

    let results: Vec<(PathBuf, Option<RecordingMetadata>, Result<Ingested, String>)> =
        rayon::ThreadPoolBuilder::new()
            .num_threads(jobs.max(1))
            .build()
            .map_err(|e| e.to_string())?
            .install(|| {
                jobs_in
                    .par_iter()
                    .map(|(path, metadata)| {
                        let r = ingest_one(path, &cfg);
                        (path.clone(), metadata.clone(), r)
                    })
                    .collect()
            });

    let mut builder = sivana_index::segment::SegmentBuilder::new();
    let mut new_state = CatalogState {
        sources,
        next_recording_id: state.next_recording_id,
    };

    for (path, metadata, result) in results {
        match result {
            Ok(ing) => {
                if new_state.sources.contains_key(&ing.hash) {
                    stats.skipped += 1; // raced duplicate within this batch
                    continue;
                }
                let rec_id = new_state.next_recording_id;
                new_state.next_recording_id += 1;
                new_state.sources.insert(ing.hash.clone(), rec_id);
                metas.insert(
                    rec_id.to_string(),
                    metadata.unwrap_or(RecordingMetadata {
                        title: ing.title,
                        artist: "Unknown artist".into(),
                        artwork_url: None,
                        source_url: None,
                        source_provider: None,
                    }),
                );
                let mut batch = Vec::new();
                encode_batch(&ing.fps, TARGET_SAMPLE_RATE, &mut batch);
                write_atomic(
                    &dir.join("fingerprints").join(format!("{rec_id}.sfp")),
                    &batch,
                )
                .map_err(|e| e.to_string())?;
                builder.add_recording(
                    RecordingId::new(rec_id),
                    &ing.fps
                        .iter()
                        .map(|f| (f.hash, f.anchor_time))
                        .collect::<Vec<_>>(),
                );
                stats.added.push(rec_id);
            }
            Err(e) => stats.failed.push((path.display().to_string(), e)),
        }
    }

    if !stats.added.is_empty() {
        let existing = segment_names(dir);
        let n = existing.len() + 1;
        let name = format!("catalog-{n:06}.siv");
        let bstats = builder
            .build(&dir.join(&name), FingerprintVersion::LANDMARK_V2_32BIT)
            .map_err(|e| e.to_string())?;
        stats.postings = bstats.postings;
        stats.segment = Some(dir.join(&name));

        let mut segments = existing;
        segments.push(name);
        let version = load_json::<Manifest>(&dir.join(manifest::MANIFEST_FILE))
            .map_or(1, |m| m.catalog_version + 1);
        let m = Manifest::new(version, FingerprintVersion::LANDMARK_V2_32BIT, segments);
        manifest::store_atomic(dir, &m).map_err(|e| e.to_string())?;
    }

    // Persist dedup + metadata last: a crash mid-run loses the run but
    // cannot corrupt the previous catalog (§41 resumable).
    write_json(&dir.join("sources.json"), &new_state)?;
    write_json(&dir.join("recordings.json"), &metas)?;

    Ok(stats)
}

fn hash_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Active segment names from the live manifest (empty when absent).
pub fn segment_names(dir: &Path) -> Vec<String> {
    load_json::<Manifest>(&dir.join(manifest::MANIFEST_FILE))
        .map(|m| m.segments)
        .unwrap_or_default()
}

/// Merge every active segment into one fresh segment and swap the
/// manifest. Old segment files are pruned after the swap succeeds.
pub fn compact(dir: &Path) -> Result<usize, String> {
    let catalog =
        sivana_index::manifest::Catalog::open(dir).map_err(|e| format!("open catalog: {e}"))?;
    let mut merged = sivana_index::segment::SegmentBuilder::new();
    let mut total_hashes = 0usize;
    for seg in &catalog.segments {
        for h in seg.all_hashes() {
            let mut out = Vec::new();
            seg.lookup(h, &mut out);
            for p in out {
                merged.add_posting(h, p.recording, p.anchor_time);
            }
            total_hashes += 1;
        }
    }
    let name = "catalog-000001.siv";
    merged
        .build(&dir.join(name), FingerprintVersion::LANDMARK_V2_32BIT)
        .map_err(|e| e.to_string())?;
    let m = Manifest::new(
        catalog.manifest.catalog_version + 1,
        FingerprintVersion::LANDMARK_V2_32BIT,
        vec![name.to_string()],
    );
    manifest::store_atomic(dir, &m).map_err(|e| e.to_string())?;
    prune_unreferenced(dir, &[name.to_string()]);
    Ok(total_hashes)
}

/// Delete segment files not in the keep-set.
pub fn prune_unreferenced(dir: &Path, keep: &[String]) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("siv") {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !keep.contains(&name.to_string()) {
                    std::fs::remove_file(&p).ok();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sivana_audio::fixtures;
    use sivana_audio::wav::write_wav;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sivana-ingest-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_wav_corpus(dir: &Path, n: usize) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for i in 0..n {
            let samples = fixtures::synth_song(9000 + i as u64, 12.0, TARGET_SAMPLE_RATE);
            let p = dir.join(format!("song-{i}.wav"));
            write_wav(&p, TARGET_SAMPLE_RATE, &samples).unwrap();
            paths.push(p);
        }
        paths
    }

    /// Local SFP1 decode (mirrors sivana-api's server decoder).
    fn decode_sfp(bytes: &[u8]) -> Option<(u32, Vec<(u32, u32)>)> {
        if bytes.len() < 16 || &bytes[..4] != b"SFP1" {
            return None;
        }
        let sr = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
        let count = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
        if bytes.len() != 16 + count * 8 {
            return None;
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let o = 16 + i * 8;
            let h = u32::from_le_bytes(bytes[o..o + 4].try_into().ok()?);
            let t = u32::from_le_bytes(bytes[o + 4..o + 8].try_into().ok()?);
            out.push((h, t));
        }
        Some((sr, out))
    }

    #[test]
    fn ingest_is_idempotent_and_compaction_preserves_lookups() {
        let corpus = temp_dir("corpus");
        let catalog = temp_dir("catalog");
        let files = make_wav_corpus(&corpus, 3);

        let stats = add_files(&catalog, &files, 2).unwrap();
        assert_eq!(stats.added.len(), 3);
        assert_eq!(stats.skipped, 0);
        assert!(stats.segment.is_some());

        let again = add_files(&catalog, &files, 2).unwrap();
        assert_eq!(again.added.len(), 0);
        assert_eq!(again.skipped, 3);
        assert!(again.segment.is_none());
        assert_eq!(segment_names(&catalog).len(), 1);

        assert!(catalog.join("fingerprints").join("0.sfp").is_file());

        compact(&catalog).unwrap();
        assert_eq!(segment_names(&catalog), vec!["catalog-000001.siv"]);

        let seg =
            sivana_index::segment::SivSegment::open(&catalog.join("catalog-000001.siv")).unwrap();
        assert_eq!(
            seg.header.fingerprint_version,
            FingerprintVersion::LANDMARK_V2_32BIT
        );
        let sidecar = std::fs::read(catalog.join("fingerprints").join("0.sfp")).unwrap();
        let (_, fps) = decode_sfp(&sidecar).unwrap();
        assert!(!fps.is_empty());
        let mut out = Vec::new();
        let mut resolved = 0;
        for (h, _) in fps.iter().take(200) {
            if seg.lookup(*h, &mut out) {
                assert!(out.iter().any(|p| p.recording.as_u32() == 0));
                resolved += 1;
            }
        }
        assert!(
            resolved > 100,
            "compaction lost postings: {resolved}/200 resolved"
        );
    }

    #[test]
    fn ingested_catalog_recognizes_degraded_query() {
        use sivana_match::{InMemoryIndex, MatchParams};

        let corpus = temp_dir("corpus2");
        let catalog = temp_dir("catalog2");
        let files = make_wav_corpus(&corpus, 2);
        add_files(&catalog, &files, 1).unwrap();

        let mut idx = InMemoryIndex::new();
        for rec in 0..2u32 {
            let bytes =
                std::fs::read(catalog.join("fingerprints").join(format!("{rec}.sfp"))).unwrap();
            let (_, fps) = decode_sfp(&bytes).unwrap();
            idx.add_recording(sivana_core::RecordingId::new(rec), &fps);
        }
        idx.finalize();

        // Noisy excerpt of song 1, fingerprinted at the ingest config.
        let song = fixtures::synth_song(9001, 12.0, TARGET_SAMPLE_RATE);
        let excerpt = fixtures::excerpt(&song, TARGET_SAMPLE_RATE, 3.0, 6.0);
        let mut rng = sivana_audio::rng::XorShift64Star::new(42);
        let noisy: Vec<f32> = excerpt
            .iter()
            .map(|&s| s + 0.05 * rng.next_bipolar())
            .collect();

        let cfg = sivana_landmark::LandmarkV2Config {
            freq_bands: FREQ_BANDS,
            ..Default::default()
        };
        let q = sivana_landmark::fingerprint(&noisy, TARGET_SAMPLE_RATE, &cfg);
        let outcomes = idx.query(
            &q.iter()
                .map(|f| sivana_match::QueryFp {
                    hash: f.hash,
                    anchor_time: f.anchor_time,
                })
                .collect::<Vec<_>>(),
            &MatchParams::default(),
        );
        assert_eq!(
            outcomes.first().map(|o| o.recording.as_u32()),
            Some(1),
            "ingested catalog must identify song 1"
        );
    }

    #[test]
    fn linked_metadata_is_persisted_and_updates_duplicates() {
        let corpus = temp_dir("metadata-corpus");
        let catalog = temp_dir("metadata-catalog");
        let file = make_wav_corpus(&corpus, 1).remove(0);
        let metadata = RecordingMetadata {
            title: "A Track".into(),
            artist: "An Artist".into(),
            artwork_url: Some("https://i.ytimg.com/cover.jpg".into()),
            source_url: Some("https://music.youtube.com/watch?v=abc123".into()),
            source_provider: Some("YouTube Music".into()),
        };

        let first = add_files_with_metadata(&catalog, &[(file.clone(), Some(metadata.clone()))], 1)
            .unwrap();
        assert_eq!(first.added, vec![0]);
        let saved: HashMap<String, RecordingMetadata> =
            load_json(&catalog.join("recordings.json")).unwrap();
        assert_eq!(saved.get("0"), Some(&metadata));

        let updated = RecordingMetadata {
            title: "A Track (Official Audio)".into(),
            ..metadata
        };
        let duplicate =
            add_files_with_metadata(&catalog, &[(file, Some(updated.clone()))], 1).unwrap();
        assert_eq!(duplicate.added.len(), 0);
        assert_eq!(duplicate.existing, vec![0]);
        let saved: HashMap<String, RecordingMetadata> =
            load_json(&catalog.join("recordings.json")).unwrap();
        assert_eq!(saved.get("0"), Some(&updated));
    }
}
