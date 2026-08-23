//! `sivana-bench` CLI — the one-command benchmark platform (§78).

use clap::{Parser, Subcommand};
use sivana_bench::corpus;
use sivana_bench::degradations::Degradation;
use sivana_bench::report;
use sivana_bench::runner;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "sivana-bench", about = "Sivana recognition benchmark platform")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a deterministic WAV fixture corpus.
    Fixtures {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 4)]
        tracks: usize,
        #[arg(long, default_value_t = 20.0)]
        seconds: f32,
        #[arg(long, default_value_t = 22_050)]
        sample_rate: u32,
        #[arg(long, default_value_t = 2026)]
        seed: u64,
    },
    /// Run the baseline benchmark (legacy engine) and emit reports.
    Run {
        #[arg(long, default_value = "bench-work/baseline.json")]
        json: PathBuf,
        #[arg(long, default_value = "bench-work/BASELINE.md")]
        markdown: PathBuf,
        #[arg(long, default_value = "bench-work/bench.sqlite")]
        db: PathBuf,
        #[arg(long, default_value_t = 4)]
        tracks: usize,
        #[arg(long, default_value_t = 20.0)]
        seconds: f32,
        #[arg(long, default_value_t = 22_050)]
        sample_rate: u32,
        #[arg(long, default_value_t = 2026)]
        seed: u64,
        #[arg(long, default_value_t = 8.0)]
        excerpt_seconds: f32,
        #[arg(long, default_value_t = 2)]
        positions_per_track: usize,
        /// Comma-separated white-noise SNR cells, e.g. "20,10,0"
        #[arg(long, default_value = "20,10")]
        snr: String,
        /// Comma-separated speed factors, e.g. "0.9,1.05"
        #[arg(long, default_value = "0.90,1.05")]
        speeds: String,
        /// Comma-separated V2 log-band counts to sweep (E3), e.g. "64,128,256"
        #[arg(long, default_value = "256")]
        bands: String,
        #[arg(long, default_value_t = false)]
        verbose: bool,
    },
}

fn parse_list(s: &str) -> Vec<f32> {
    s.split(',')
        .filter_map(|p| p.trim().parse::<f32>().ok())
        .collect()
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Fixtures {
            out,
            tracks,
            seconds,
            sample_rate,
            seed,
        } => {
            let c = corpus::generate(tracks, seconds, sample_rate, seed);
            corpus::write_wav_files(&c, &out).map_err(|e| e.to_string())?;
            println!("wrote {} fixtures to {}", c.tracks.len(), out.display());
            Ok(())
        }
        Command::Run {
            json,
            markdown,
            db,
            tracks,
            seconds,
            sample_rate,
            seed,
            excerpt_seconds,
            positions_per_track,
            snr,
            speeds,
            bands,
            verbose,
        } => {
            let mut grid = runner::default_grid();
            grid.excerpt_seconds = excerpt_seconds;
            grid.positions_per_track = positions_per_track.max(1);
            grid.verbose = verbose;

            // Rebuild the degradation grid from CLI flags (clean always included).
            let mut degs = vec![Degradation::None];
            for v in parse_list(&snr) {
                degs.push(Degradation::WhiteNoise { snr_db: v });
            }
            degs.push(Degradation::PinkNoise { snr_db: 10.0 });
            for f in parse_list(&speeds) {
                if f > 0.0 {
                    degs.push(Degradation::Speed { factor: f });
                }
            }
            degs.push(Degradation::LowPass { cutoff_hz: 3000.0 });
            degs.push(Degradation::HighPass { cutoff_hz: 150.0 });
            degs.push(Degradation::Clip { threshold: 0.30 });
            degs.push(Degradation::Echo {
                delay_s: 0.15,
                gain: 0.40,
            });
            grid.degradations = degs;

            let started = std::time::Instant::now();
            let c = corpus::generate(tracks, seconds, sample_rate, seed);
            let grid = &grid;
            let legacy = runner::run_baseline(&c, grid, &db)
                .map_err(|e| format!("legacy run failed: {e}"))?;
            let band_list = parse_band_list(&bands);
            if band_list.is_empty() {
                return Err("no valid band counts in --bands".into());
            }
            let mut v2_runs = Vec::new();
            for bands_n in &band_list {
                let v2 = runner::run_landmark_v2(&c, grid, *bands_n)
                    .map_err(|e| format!("landmark-v2 (bands={bands_n}) run failed: {e}"))?;
                // Single-band runs keep the historical filename; sweeps
                // suffix each JSON with its band count.
                let v2_json = if band_list.len() == 1 {
                    json.with_extension("v2.json")
                } else {
                    json.with_extension(format!("v2-b{bands_n}.json"))
                };
                report::write_json(&v2, &v2_json)?;
                v2_runs.push(v2);
            }

            report::write_json(&legacy, &json)?;
            report::write_markdown(&legacy, &markdown)?;

            let mut engines: Vec<&runner::RunSummary> = vec![&legacy];
            engines.extend(v2_runs.iter());
            report::write_comparison(&engines, &markdown.with_file_name("COMPARISON.md"))?;

            for s in &engines {
                let agg = s.aggregate();
                println!(
                    "[{}] cases {} | recall track/offset/gated {:.1}%/{:.1}%/{:.1}% | fp {:.1} ms | match {:.1} ms | false accepts {}/{}",
                    s.engine,
                    agg.total_cases,
                    agg.recall_track * 100.0,
                    agg.recall_offset * 100.0,
                    agg.recall_gated * 100.0,
                    agg.mean_fingerprint_ms,
                    agg.mean_match_ms,
                    agg.false_accepts,
                    agg.rejection_cases
                );
            }
            println!(
                "reports: {}, {}, COMPARISON.md",
                json.display(),
                markdown.display()
            );
            println!("wall time: {:.1}s", started.elapsed().as_secs_f64());
            Ok(())
        }
    }
}

fn parse_band_list(s: &str) -> Vec<u16> {
    s.split(',')
        .filter_map(|p| p.trim().parse::<u16>().ok())
        .filter(|&b| b > 0 && b.is_power_of_two())
        .collect()
}
