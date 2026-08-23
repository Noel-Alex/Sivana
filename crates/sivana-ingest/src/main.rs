//! `sivana-ingest` — catalog ingestion and compaction CLI (Phase 6).

use clap::{Parser, Subcommand};
use sivana_ingest as ingest;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "sivana-ingest", about = "Sivana catalog ingestion platform")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ingest audio files into a catalog (idempotent per source hash).
    Add {
        /// Catalog directory (created on first use).
        #[arg(long)]
        catalog: PathBuf,
        /// Files or directories to ingest (directories are walked for
        /// mp3/flac/wav/ogg/m4a/aac files).
        files: Vec<PathBuf>,
        /// Worker threads (0 = all cores).
        #[arg(long, default_value_t = 0)]
        jobs: usize,
        /// Skip files whose name contains any of these comma-separated
        /// substrings (case-insensitive), e.g. internal WIP variants.
        #[arg(long, default_value = "")]
        exclude: String,
    },
    /// Watch an inbox folder and ingest anything new automatically.
    /// Idempotent per source hash: files already in the catalog are
    /// skipped, so leaving this running is safe. New audio lands as a
    /// delta segment + atomic manifest swap; the query server's watcher
    /// picks it up within 5 s.
    Watch {
        /// Catalog directory.
        #[arg(long)]
        catalog: PathBuf,
        /// Folder to watch for audio files.
        #[arg(long)]
        inbox: PathBuf,
        /// Poll interval in seconds.
        #[arg(long, default_value_t = 10)]
        interval: u64,
        /// Skip names containing these comma-separated substrings.
        #[arg(long, default_value = "")]
        exclude: String,
    },
    /// Merge all active segments into one and prune the rest.
    Compact {
        #[arg(long)]
        catalog: PathBuf,
    },
    /// Print catalog status.
    Status {
        #[arg(long)]
        catalog: PathBuf,
    },
}

const AUDIO_EXTS: &[&str] = &["mp3", "flac", "wav", "ogg", "m4a", "aac"];

fn collect_files(inputs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in inputs {
        if p.is_dir() {
            walk(p, &mut out);
        } else {
            out.push(p.clone());
        }
    }
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if AUDIO_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
                    out.push(p);
                }
            }
        }
    }
}

fn chrono_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string()
}

fn main() -> Result<(), String> {
    match Cli::parse().command {
        Command::Add {
            catalog,
            files,
            jobs,
            exclude,
        } => {
            let skip: Vec<String> = exclude
                .split(',')
                .map(|p| p.trim().to_lowercase())
                .filter(|p| !p.is_empty())
                .collect();
            let mut files = collect_files(&files);
            if !skip.is_empty() {
                files.retain(|p| {
                    let name = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    !skip.iter().any(|pat| name.contains(pat))
                });
            }
            if files.is_empty() {
                return Err("no input files found".into());
            }
            println!(
                "ingesting {} file(s) with {} worker(s)...",
                files.len(),
                if jobs == 0 {
                    rayon::current_num_threads()
                } else {
                    jobs
                }
            );
            let stats = ingest::add_files(&catalog, &files, jobs)?;
            println!(
                "added {} recording(s), skipped {} (duplicate/corrupt-free dedup), failed {}",
                stats.added.len(),
                stats.skipped,
                stats.failed.len()
            );
            for (f, e) in &stats.failed {
                eprintln!("  FAILED {f}: {e}");
            }
            if let Some(seg) = &stats.segment {
                println!("segment: {}", seg.display());
            }
            Ok(())
        }
        Command::Watch {
            catalog,
            inbox,
            interval,
            exclude,
        } => {
            let skip: Vec<String> = exclude
                .split(',')
                .map(|p| p.trim().to_lowercase())
                .filter(|p| !p.is_empty())
                .collect();
            std::fs::create_dir_all(&inbox).map_err(|e| e.to_string())?;
            println!(
                "watching {} -> catalog {} (every {}s; Ctrl+C to stop)",
                inbox.display(),
                catalog.display(),
                interval
            );
            loop {
                let files = collect_files(&[inbox.clone()]);
                let files: Vec<PathBuf> = files
                    .into_iter()
                    .filter(|p| {
                        let name = p
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        !skip.iter().any(|pat| name.contains(pat))
                    })
                    .collect();
                if !files.is_empty() {
                    match ingest::add_files(&catalog, &files, 0) {
                        Ok(stats) => {
                            if !stats.added.is_empty() || !stats.failed.is_empty() {
                                println!(
                                    "[{}] added {}, skipped {}, failed {}",
                                    chrono_now(),
                                    stats.added.len(),
                                    stats.skipped,
                                    stats.failed.len()
                                );
                                for (f, e) in &stats.failed {
                                    eprintln!("  FAILED {f}: {e}");
                                }
                            }
                        }
                        Err(e) => eprintln!("[{}] ingest error: {e}", chrono_now()),
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(interval.max(1)));
            }
        }
        Command::Compact { catalog } => {
            let hashes = ingest::compact(&catalog)?;
            println!("compacted; {hashes} distinct hash entries merged");
            Ok(())
        }
        Command::Status { catalog } => {
            let segments = ingest::segment_names(&catalog);
            println!("catalog: {}", catalog.display());
            for s in &segments {
                println!("  segment: {s}");
            }
            match std::fs::read(catalog.join("sources.json")) {
                Ok(b) => {
                    let state: serde_json::Value =
                        serde_json::from_slice(&b).map_err(|e| e.to_string())?;
                    if let Some(o) = state.as_object() {
                        println!(
                            "  sources: {}",
                            o.get("sources")
                                .map(|m| m.as_object().map(|x| x.len()).unwrap_or(0))
                                .unwrap_or(0)
                        );
                    }
                }
                Err(_) => println!("  (empty catalog)"),
            }
            Ok(())
        }
    }
}
