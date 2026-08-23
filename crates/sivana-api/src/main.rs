//! Sivana recognition API (PLAN §37-§39, Phase 5).
//!
//! Routes:
//! ```text
//! POST /v1/sessions              -> { session_id }
//! WS   /v1/identify/{session}    <- SFP1 binary batches
//!                                 -> JSON state events (listening/candidate/matched/no_match)
//! GET  /v1/recordings/{id}       -> catalog metadata
//! GET  /v1/health                -> liveness + catalog version
//! ```
//!
//! Security posture (§39): sessions are capped, batches are size-limited,
//! and only the trailing fingerprint window is kept per session. The
//! server never sees raw audio.

mod recognition;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{
        Path as AxumPath, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    routing::{get, post},
};
use sivana_core::RecordingId;
use sivana_index::manifest::Catalog;
use sivana_match::{InMemoryIndex, MatchParams, QueryFp};
use tokio::sync::Mutex;
use tower_http::services::ServeDir;

/// Max bytes of one fingerprint batch (a 10 s window is ~2 KB; generous).
const MAX_BATCH_BYTES: usize = 256 * 1024;
/// Idle sessions are dropped after this long.
const SESSION_TTL: Duration = Duration::from_secs(120);

#[derive(Clone)]
struct AppState {
    index: Arc<InMemoryIndex>,
    params: Arc<MatchParams>,
    sessions: Arc<Mutex<HashMap<u64, recognition::RecognitionSession>>>,
    next_session: Arc<std::sync::atomic::AtomicU64>,
    titles: Arc<HashMap<u32, RecordingMeta>>,
    catalog_version: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct RecordingMeta {
    title: String,
    artist: String,
}

fn build_index_from_catalog(
    dir: Option<&PathBuf>,
) -> (Arc<InMemoryIndex>, HashMap<u32, RecordingMeta>, u64) {
    let mut index = InMemoryIndex::new();
    let mut titles = HashMap::new();
    let mut version = 0u64;
    if let Some(dir) = dir {
        match Catalog::open(dir) {
            Ok(cat) => {
                version = cat.manifest.catalog_version;
                // Metadata file sits next to the manifest (Phase 6 formalizes it).
                let meta_path = dir.join("recordings.json");
                if let Ok(bytes) = std::fs::read(&meta_path) {
                    if let Ok(map) =
                        serde_json::from_slice::<HashMap<String, RecordingMeta>>(&bytes)
                    {
                        for (k, v) in map {
                            if let Ok(id) = k.parse::<u32>() {
                                titles.insert(id, v);
                            }
                        }
                    }
                }
                // Materialize postings into the matcher index from the
                // per-recording SFP1 sidecars written by ingestion
                // (Phase 6); the .siv segments remain the serving format.
                let sidecar = dir.join("fingerprints");
                if sidecar.is_dir() {
                    for entry in std::fs::read_dir(&sidecar).into_iter().flatten() {
                        let p = entry.ok().map(|e| e.path());
                        if let Some(p) = p {
                            if let Ok(bytes) = std::fs::read(&p) {
                                if let Some((_, fps)) = decode_sfp_batch(&bytes) {
                                    index.add_recording(
                                        RecordingId::new(
                                            p.file_stem()
                                                .and_then(|s| s.to_str())
                                                .and_then(|s| s.parse::<u32>().ok())
                                                .unwrap_or(0),
                                        ),
                                        &fps.iter().map(|&(h, t)| (h, t)).collect::<Vec<_>>(),
                                    );
                                }
                            }
                        }
                    }
                }
                index.finalize();
            }
            Err(e) => {
                eprintln!(
                    "catalog open failed ({}): {e}; starting empty",
                    dir.display()
                );
            }
        }
    }
    (Arc::new(index), titles, version)
}

/// Decode an SFP1 batch: returns (sample_rate_hz, fingerprints).
pub fn decode_sfp_batch(bytes: &[u8]) -> Option<(u32, Vec<(u32, u32)>)> {
    if bytes.len() < 16 || &bytes[..4] != b"SFP1" {
        return None;
    }
    let sample_rate = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
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
    Some((sample_rate, out))
}

async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "catalog_version": state.catalog_version,
        "engine": "landmark-v2",
    }))
}

async fn create_session(State(state): State<AppState>) -> Json<serde_json::Value> {
    let id = state
        .next_session
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Json(serde_json::json!({ "session_id": id }))
}

async fn get_recording(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u32>,
) -> Json<serde_json::Value> {
    match state.titles.get(&id) {
        Some(m) => Json(serde_json::json!({
            "recording_id": id,
            "title": m.title,
            "artist": m.artist,
        })),
        None => Json(serde_json::json!({ "recording_id": id, "title": null })),
    }
}

async fn ws_identify(
    State(state): State<AppState>,
    AxumPath(session_id): AxumPath<u64>,
    upgrade: WebSocketUpgrade,
) -> axum::response::Response {
    upgrade.on_upgrade(move |socket| handle_ws(socket, state, session_id))
}

async fn handle_ws(mut socket: WebSocket, state: AppState, session_id: u64) {
    // Register the session on first batch (client tells us its sample rate).
    send_event(
        &mut socket,
        serde_json::json!({ "event": "listening", "detail": "waiting for audio" }),
    )
    .await;

    while let Some(Ok(msg)) = socket.recv().await {
        let bytes = match msg {
            Message::Binary(b) => b,
            Message::Close(_) => break,
            _ => continue,
        };
        if bytes.len() > MAX_BATCH_BYTES {
            send_event(
                &mut socket,
                serde_json::json!({ "event": "error", "detail": "batch too large" }),
            )
            .await;
            break;
        }
        let Some((sample_rate, fps)) = decode_sfp_batch(&bytes) else {
            send_event(
                &mut socket,
                serde_json::json!({ "event": "error", "detail": "malformed batch" }),
            )
            .await;
            continue;
        };

        let mut sessions = state.sessions.lock().await;
        // Expire stale sessions opportunistically.
        sessions.retain(|_, s| s.capture_seconds() < SESSION_TTL.as_secs_f32() * 60.0);

        let hop = 1024; // V2 default geometry; client sample rate scales fps_rate
        let session = sessions
            .entry(session_id)
            .or_insert_with(|| recognition::RecognitionSession::new(sample_rate, hop));
        let new_state = session.ingest(
            fps.into_iter()
                .map(|(h, t)| QueryFp {
                    hash: h,
                    anchor_time: t,
                })
                .collect(),
            &state.index,
            &state.params,
        );
        let capture = session.capture_seconds();
        let outcome = session.outcome.clone();

        let event = match (new_state, outcome) {
            (recognition::RecognitionState::ConfidentMatch, Some(o)) => {
                let rec_id = o.recording.as_u32();
                let title = state.titles.get(&rec_id);
                serde_json::json!({
                    "event": "matched",
                    "recording_id": rec_id,
                    "title": title.map(|t| t.title.clone()),
                    "artist": title.map(|t| t.artist.clone()),
                    "offset_frames": o.offset_frames,
                    "inliers": o.inliers,
                    "concentration": o.offset_concentration,
                    "margin": o.margin_over_next,
                    "capture_seconds": capture,
                })
            }
            (recognition::RecognitionState::Candidate, _) => {
                serde_json::json!({ "event": "candidate", "capture_seconds": capture })
            }
            (recognition::RecognitionState::NoMatch, _) => {
                serde_json::json!({ "event": "no_match", "capture_seconds": capture })
            }
            _ => serde_json::json!({ "event": "listening", "capture_seconds": capture }),
        };
        drop(sessions);
        send_event(&mut socket, event).await;
        if new_state == recognition::RecognitionState::ConfidentMatch
            || new_state == recognition::RecognitionState::NoMatch
        {
            let _ = socket.send(Message::Close(None)).await;
            break;
        }
    }
    state.sessions.lock().await.remove(&session_id);
}

async fn send_event(socket: &mut WebSocket, event: serde_json::Value) {
    let _ = socket.send(Message::Text(event.to_string())).await;
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let catalog = args
        .iter()
        .position(|a| a == "--catalog")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let web_dir = args
        .iter()
        .position(|a| a == "--web")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("apps/web"));

    let started = Instant::now();
    let (index, titles, version) = build_index_from_catalog(catalog.as_ref());
    println!(
        "catalog v{version}: {} hashes indexed in {:.2}s",
        index.len(),
        started.elapsed().as_secs_f64()
    );

    let state = AppState {
        index,
        params: Arc::new(MatchParams::default()),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        next_session: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        titles: Arc::new(titles),
        catalog_version: version,
    };

    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/sessions", post(create_session))
        .route("/v1/identify/:session_id", get(ws_identify))
        .route("/v1/recordings/:id", get(get_recording))
        .fallback_service(ServeDir::new(&web_dir).append_index_html_on_directories(true))
        .with_state(state);

    let addr = std::env::var("SIVANA_ADDR").unwrap_or_else(|_| "127.0.0.1:8077".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!(
        "sivana-api listening on http://{addr} (serving {})",
        web_dir.display()
    );
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use sivana_audio::fixtures;
    use sivana_landmark::LandmarkV2Config;
    use sivana_wasm::FingerprintEngine;

    /// End-to-end: fixture -> local engine -> SFP1 wire bytes -> decode ->
    /// streaming session -> confident match on the right recording.
    #[test]
    fn full_pipeline_finds_the_right_recording() {
        // Two "songs" in the catalog.
        let cfg = LandmarkV2Config::default();
        let mut idx = InMemoryIndex::new();
        for rec in 0..2u32 {
            let song = fixtures::synth_song(1000 + rec as u64, 15.0, 22_050);
            let fps = sivana_landmark::fingerprint(&song, 22_050, &cfg);
            idx.add_recording(
                RecordingId::new(rec),
                &fps.iter()
                    .map(|f| (f.hash, f.anchor_time))
                    .collect::<Vec<_>>(),
            );
        }
        idx.finalize();

        // A degraded excerpt of song 0 plays locally and gets fingerprinted
        // exactly like the browser would: chunked push -> SFP1 -> decode.
        let song = fixtures::synth_song(1000, 15.0, 22_050);
        let excerpt = fixtures::excerpt(&song, 22_050, 4.0, 6.0);
        let noisy = sivana_bench::degradations::Degradation::WhiteNoise { snr_db: 10.0 }
            .apply(&excerpt, 22_050, 1);
        let mut engine = FingerprintEngine::new(22_050, cfg.clone());
        let mut session = recognition::RecognitionSession::new(22_050, cfg.hop);

        let params = MatchParams::default();
        let mut final_state = recognition::RecognitionState::Listening;
        for piece in noisy.chunks(22050 / 4) {
            engine.process(piece);
            let mut batch = Vec::new();
            engine.take_batch(&mut batch);
            if batch.len() <= 16 {
                continue;
            }
            let Some((_, fps)) = decode_sfp_batch(&batch) else {
                panic!("server failed to decode its own engine's batch");
            };
            final_state = session.ingest(
                fps.into_iter()
                    .map(|(h, t)| QueryFp {
                        hash: h,
                        anchor_time: t,
                    })
                    .collect(),
                &idx,
                &params,
            );
            if final_state == recognition::RecognitionState::ConfidentMatch {
                break;
            }
        }

        assert_eq!(
            final_state,
            recognition::RecognitionState::ConfidentMatch,
            "streaming session should reach a confident match"
        );
        let o = session.outcome.unwrap();
        assert_eq!(o.recording.as_u32(), 0, "right recording identified");
    }

    #[test]
    fn malformed_batches_are_rejected() {
        assert!(decode_sfp_batch(b"").is_none());
        assert!(decode_sfp_batch(b"SFP2xxxx").is_none());
        assert!(decode_sfp_batch(&[0u8; 15]).is_none());
        // Count field disagrees with length.
        let mut b = vec![b'S', b'F', b'P', b'1', 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0];
        b.extend_from_slice(&[0u8; 8]);
        assert!(decode_sfp_batch(&b).is_none());
    }
}
