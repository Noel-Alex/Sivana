//! Load generator (PLAN §53, §88): drives a running sivana-api with
//! realistic streaming recognition sessions and reports latency
//! percentiles for the full session lifecycle.
//!
//! Usage:
//!   cargo run -p sivana-bench --release -- loadgen --url ws://127.0.0.1:8077 \
//!     --concurrency 8 --sessions 100

use std::time::{Duration, Instant};
macro_rules! trace { ($($a:tt)*) => { if std::env::var("LOADGEN_TRACE").is_ok() { eprintln!("[lg] {}", format!($($a)*)); } }; }

use sivana_audio::fixtures;
use sivana_landmark::LandmarkV2Config;
use sivana_wasm::FingerprintEngine;

pub struct LoadReport {
    pub sessions_ok: usize,
    pub sessions_failed: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub total_seconds: f64,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len());
    sorted[idx - 1]
}

fn run_one_session(
    url: &str,
    session_id: u64,
    pcm: &[f32],
    engine_cfg: &LandmarkV2Config,
) -> Result<f64, String> {
    use tungstenite::Message;
    let started = Instant::now();
    let (mut ws, _resp) = tungstenite::client::connect(format!("{url}/v1/identify/{session_id}"))
        .map_err(|e| {
        trace!("connect failed: {e}");
        e.to_string()
    })?;
    trace!("connected");

    let mut engine = FingerprintEngine::new(22_050, engine_cfg.clone());
    let chunk = 22_050usize / 4; // 250 ms of audio per batch
    for piece in pcm.chunks(chunk) {
        engine.process(piece);
        let mut batch = Vec::new();
        engine.take_batch(&mut batch);
        if batch.len() > 16 {
            trace!("sent batch of {} fps", batch.len() / 8 - 2);
            ws.send(Message::Binary(batch)).map_err(|e| e.to_string())?;
            ws.flush().map_err(|e| e.to_string())?;
        }
        // Real clients stream in real time; pacing keeps the server's
        // capture clock meaningful and exercises the streaming path.
        std::thread::sleep(Duration::from_millis(250));
    }
    engine.finish();
    let mut batch = Vec::new();
    engine.take_batch(&mut batch);
    if batch.len() > 16 {
        ws.send(Message::Binary(batch)).map_err(|e| e.to_string())?;
        ws.flush().map_err(|e| e.to_string())?;
    }

    // Block until a terminal event or close.
    trace!("streaming done; waiting terminal");
    loop {
        match ws.read() {
            Ok(Message::Text(txt)) => {
                trace!("text: {}", &txt[..txt.len().min(80)]);
                if txt.contains("\"matched\"") || txt.contains("\"no_match\"") {
                    break;
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("ws read: {e}")),
        }
    }
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

/// Run `n_sessions` recognition sessions against `url` with `concurrency`
/// worker threads; each session replays a fixture excerpt end-to-end and
/// records the wall time until the terminal server event.
pub fn run_load(
    url: &str,
    n_sessions: usize,
    concurrency: usize,
    seed: u64,
) -> Result<LoadReport, String> {
    let cfg = LandmarkV2Config::default();
    // A small pool of pre-generated excerpts shared by all workers.
    let excerpts: Vec<Vec<f32>> = (0..concurrency.max(1))
        .map(|i| {
            let song = fixtures::synth_song(seed + i as u64, 10.0, 22_050);
            fixtures::excerpt(&song, 22_050, 1.0, 6.0)
        })
        .collect();

    let started = Instant::now();
    let workers = concurrency.max(1);
    let per_worker = n_sessions.div_ceil(workers);
    let results: Vec<Result<f64, String>> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for w in 0..workers {
            let url = url.to_string();
            let pcm = excerpts[w % excerpts.len()].clone();
            let cfg = cfg.clone();
            handles.push(scope.spawn(move || {
                (0..per_worker)
                    .map(|i| {
                        // Unique session ids so concurrent workers do not
                        // share recognition state on the server.
                        let sid = (w as u64) * 1_000_000 + i as u64;
                        run_one_session(&url, sid, &pcm, &cfg)
                    })
                    .collect::<Vec<_>>()
            }));
        }
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    let total_seconds = started.elapsed().as_secs_f64();

    let mut latencies: Vec<f64> = results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .copied()
        .collect();
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(LoadReport {
        sessions_ok: latencies.len(),
        sessions_failed: results.len() - latencies.len(),
        p50_ms: percentile(&latencies, 0.50),
        p95_ms: percentile(&latencies, 0.95),
        p99_ms: percentile(&latencies, 0.99),
        total_seconds,
    })
}
