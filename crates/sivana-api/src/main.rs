//! Sivana recognition API (PLAN §37-§39, Phase 5).
//!
//! Routes:
//! ```text
//! POST /v1/sessions              -> { session_id }
//! WS   /v1/identify/{session}    <- SFP1 binary batches
//!                                 -> JSON state events (listening/candidate/matched/no_match)
//! GET  /v1/metadata/youtube     -> no-key YouTube Music metadata lookup
//! GET  /v1/recordings           -> catalog listing
//! POST /v1/recordings           -> multipart audio ingestion + metadata
//! GET  /v1/recordings/{id}       -> catalog metadata
//! GET  /v1/health                -> liveness + catalog version
//! ```
//!
//! Security posture (§39): at most `MAX_CONCURRENT_SESSIONS` (= 32)
//! recognition sessions run concurrently, and session creation is
//! fixed-window rate limited per client IP. Raw `/v1/identify/{session}`
//! websocket connects are not rate limited (the load generator and the
//! extension open them directly with self-assigned ids); each is still
//! bounded by the session cap plus the batch-size and capture-timeout
//! limits. Batches are size-limited, and only the trailing fingerprint
//! window is kept per session. Uploads (`POST /v1/recordings`) are gated by an
//! optional shared secret: start the server with `--ingest-token <secret>`
//! and present it as `Authorization: Bearer <token>` or a `token` multipart
//! field. The server never sees raw audio.

mod recognition;

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{
        ConnectInfo, DefaultBodyLimit, Multipart, Path as AxumPath, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sivana_core::RecordingId;
use sivana_index::manifest::Catalog;
use sivana_ingest::RecordingMetadata;
use sivana_match::{InMemoryIndex, MatchParams, QueryFp};
use tokio::sync::Mutex;
use tower_http::services::ServeDir;

/// Max bytes of one fingerprint batch (a 10 s window is ~2 KB; generous).
const MAX_BATCH_BYTES: usize = 256 * 1024;
/// Idle sessions are dropped after this long.
const SESSION_TTL: Duration = Duration::from_secs(120);
/// Uploads are local admin operations, but still need a bounded body.
const MAX_UPLOAD_BYTES: usize = 160 * 1024 * 1024;
/// Ceiling on live recognition sessions (§39 abuse resistance). Sessions
/// live in `AppState::sessions` until TTL eviction, so the map size is the
/// active count.
const MAX_CONCURRENT_SESSIONS: usize = 32;
/// Session creations allowed per client IP per fixed window. Generous on
/// purpose: a human starting one capture every couple of seconds never
/// sees this.
const SESSIONS_PER_WINDOW: u32 = 30;
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Rate-limit table hygiene: drop IPs idle longer than this, and hard-cap
/// the table so a spoofed-source flood cannot grow it without bound.
const RATE_TABLE_IDLE_TTL: Duration = Duration::from_secs(600);
const RATE_TABLE_MAX_ENTRIES: usize = 4096;

/// Fixed-window per-IP rate limiter (§39). No new dependencies: one
/// std-Mutex'd map of IP -> (window start, hits in this window).
///
/// Bounded state: entries idle for `RATE_TABLE_IDLE_TTL` are dropped on
/// every check, and once the table reaches `RATE_TABLE_MAX_ENTRIES` the
/// oldest window is recycled before inserting an unseen IP. A spoofed-
/// source flood therefore cannot grow memory without limit.
#[derive(Default)]
struct RateLimiter {
    /// ip -> (window start, hits in this window)
    windows: StdMutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl RateLimiter {
    /// Production entry point: server-wide budget per IP.
    fn check(&self, ip: IpAddr, now: Instant) -> bool {
        self.check_with(ip, now, SESSIONS_PER_WINDOW, RATE_WINDOW)
    }

    /// Injectible variant so tests can exercise window rollover with tiny
    /// budgets/windows instead of waiting real time.
    fn check_with(&self, ip: IpAddr, now: Instant, limit: u32, window: Duration) -> bool {
        let mut windows = self.windows.lock().expect("rate limiter lock poisoned");
        windows.retain(|_, (window_start, _)| {
            now.checked_duration_since(*window_start)
                .is_some_and(|age| age < RATE_TABLE_IDLE_TTL)
        });
        if windows.len() >= RATE_TABLE_MAX_ENTRIES && !windows.contains_key(&ip) {
            let oldest = windows
                .iter()
                .min_by_key(|(_, (window_start, _))| *window_start)
                .map(|(ip, _)| *ip);
            if let Some(oldest) = oldest {
                windows.remove(&oldest);
            }
        }
        match windows.get_mut(&ip) {
            Some((window_start, hits)) => {
                if now.duration_since(*window_start) >= window {
                    *window_start = now;
                    *hits = 1;
                    true
                } else {
                    *hits += 1;
                    *hits <= limit
                }
            }
            None => {
                windows.insert(ip, (now, 1));
                true
            }
        }
    }
}

/// Everything a query needs from the catalog, swapped atomically when
/// the manifest changes (Phase 10: live catalog updates without restart).
struct Bundle {
    index: Arc<InMemoryIndex>,
    titles: Arc<HashMap<u32, RecordingMeta>>,
    catalog_version: u64,
}

#[derive(Clone)]
struct AppState {
    bundle: Arc<std::sync::RwLock<Arc<Bundle>>>,
    /// Catalog directory watched for manifest swaps (None = static empty).
    catalog_dir: Option<PathBuf>,
    params: Arc<MatchParams>,
    sessions: Arc<Mutex<HashMap<u64, recognition::RecognitionSession>>>,
    ingest_lock: Arc<Mutex<()>>,
    /// Shared secret for POST /v1/recordings (None = endpoint open).
    ingest_token: Option<Arc<str>>,
    /// Fixed-window limiter on session creation, keyed by client IP.
    session_rate: Arc<RateLimiter>,
    next_session: Arc<std::sync::atomic::AtomicU64>,
    next_upload: Arc<std::sync::atomic::AtomicU64>,
    http_client: reqwest::Client,
}

impl AppState {
    fn snapshot(&self) -> Arc<Bundle> {
        self.bundle.read().expect("bundle lock poisoned").clone()
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct RecordingMeta {
    title: String,
    artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artwork_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_provider: Option<String>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn too_many_requests(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: message.into(),
        }
    }
}

/// Shared-secret gate for `POST /v1/recordings`. `configured` is the
/// `--ingest-token` value; `provided` is whatever the caller presented
/// (Bearer credential or `token` form field). No token configured means
/// the endpoint stays open for local development.
///
/// Plain equality on purpose: this is a localhost convenience gate, not a
/// password hash, and a timing side channel against another process on
/// your own machine is out of threat model.
fn ingest_authorized(configured: Option<&str>, provided: Option<&str>) -> Result<(), ApiError> {
    match configured {
        None => Ok(()),
        Some(expected) => match provided {
            Some(got) if got == expected => Ok(()),
            _ => Err(ApiError::unauthorized(
                "This server requires an ingest token. Present the server's --ingest-token \
                 secret as `Authorization: Bearer <token>` or a `token` form field.",
            )),
        },
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

#[derive(Clone, serde::Serialize)]
struct LinkMetadata {
    title: String,
    artist: String,
    artwork_url: String,
    source_url: String,
    provider: String,
}

#[derive(serde::Deserialize)]
struct MetadataQuery {
    url: String,
}

#[derive(serde::Deserialize)]
struct YoutubeOembed {
    title: String,
    author_name: String,
    thumbnail_url: String,
}

#[derive(serde::Deserialize)]
struct ItunesSearchResponse {
    results: Vec<ItunesTrack>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItunesTrack {
    track_name: String,
    artist_name: String,
    artwork_url100: String,
}

fn validate_youtube_music_url(raw: &str) -> Result<reqwest::Url, ApiError> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|_| ApiError::bad_request("Enter a valid YouTube Music link."))?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let supported = matches!(
        host.as_str(),
        "music.youtube.com" | "www.youtube.com" | "youtube.com" | "youtu.be"
    );
    if url.scheme() != "https" || !supported {
        return Err(ApiError::bad_request(
            "Use a YouTube Music track link (music.youtube.com).",
        ));
    }
    let has_video = if host == "youtu.be" {
        url.path_segments()
            .and_then(|mut parts| parts.next())
            .is_some_and(|id| !id.is_empty())
    } else {
        url.query_pairs()
            .any(|(key, value)| key == "v" && !value.is_empty())
    };
    if !has_video {
        return Err(ApiError::bad_request(
            "That link does not contain a YouTube track id.",
        ));
    }
    Ok(url)
}

fn clean_youtube_title(raw: &str, artist: &str) -> String {
    let mut title = raw.trim().to_string();
    let prefix = format!("{} - ", artist.trim());
    if title
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(&prefix))
    {
        title = title[prefix.len()..].trim().to_string();
    }
    loop {
        let Some(open) = title.rfind('(') else {
            break;
        };
        if !title.ends_with(')') {
            break;
        }
        let qualifier = title[open + 1..title.len() - 1].to_ascii_lowercase();
        let presentation_only = [
            "official",
            "video",
            "audio",
            "lyric",
            "visualizer",
            "remaster",
            "4k",
        ]
        .iter()
        .any(|marker| qualifier.contains(marker));
        if !presentation_only {
            break;
        }
        title.truncate(open);
        title = title.trim().to_string();
    }
    if title.is_empty() {
        raw.trim().to_string()
    } else {
        title
    }
}

fn clean_youtube_artist(raw: &str) -> String {
    raw.trim()
        .strip_suffix(" - Topic")
        .unwrap_or(raw.trim())
        .trim()
        .to_string()
}

fn metadata_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn select_square_artwork(
    response: ItunesSearchResponse,
    title: &str,
    artist: &str,
) -> Option<String> {
    let title_key = metadata_key(title);
    let artist_key = metadata_key(artist);
    let exact = response.results.into_iter().find(|track| {
        metadata_key(&track.track_name) == title_key
            && metadata_key(&track.artist_name) == artist_key
    })?;
    Some(exact.artwork_url100.replace("100x100bb", "600x600bb"))
}

async fn fetch_square_artwork(
    client: &reqwest::Client,
    title: &str,
    artist: &str,
) -> Option<String> {
    let response = client
        .get("https://itunes.apple.com/search")
        .query(&[
            ("term", format!("{title} {artist}")),
            ("entity", "song".to_string()),
            ("limit", "10".to_string()),
        ])
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<ItunesSearchResponse>()
        .await
        .ok()?;
    select_square_artwork(response, title, artist)
}

async fn fetch_youtube_metadata(
    client: &reqwest::Client,
    raw_url: &str,
) -> Result<LinkMetadata, ApiError> {
    let source_url = validate_youtube_music_url(raw_url)?;
    let response = client
        .get("https://www.youtube.com/oembed")
        .query(&[("url", source_url.as_str()), ("format", "json")])
        .send()
        .await
        .map_err(|_| ApiError::bad_request("YouTube Music metadata could not be reached."))?;
    if !response.status().is_success() {
        return Err(ApiError::bad_request(
            "YouTube Music could not find metadata for that link.",
        ));
    }
    let metadata = response
        .json::<YoutubeOembed>()
        .await
        .map_err(|_| ApiError::bad_request("YouTube Music returned incomplete metadata."))?;
    let title = clean_youtube_title(&metadata.title, &metadata.author_name);
    let artist = clean_youtube_artist(&metadata.author_name);
    let artwork_url = fetch_square_artwork(client, &title, &artist)
        .await
        .unwrap_or(metadata.thumbnail_url);
    Ok(LinkMetadata {
        title,
        artist,
        artwork_url,
        source_url: source_url.to_string(),
        provider: "YouTube Music".into(),
    })
}

fn build_bundle(dir: Option<&PathBuf>) -> Bundle {
    let mut index = InMemoryIndex::new();
    let mut titles = HashMap::new();
    let mut version = 0u64;
    if let Some(dir) = dir {
        if dir.join(sivana_index::manifest::MANIFEST_FILE).is_file() {
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
                }
                Err(e) => {
                    eprintln!(
                        "catalog open failed ({}): {e}; starting empty",
                        dir.display()
                    );
                }
            }
        }
    }
    // An empty catalog is still a valid matcher state for local development
    // and must reject queries cleanly instead of panicking on the first batch.
    index.finalize();
    Bundle {
        index: Arc::new(index),
        titles: Arc::new(titles),
        catalog_version: version,
    }
}

/// Background task: watch the catalog manifest and atomically swap the
/// serving bundle whenever it changes (§21 atomic swaps; §88 matcher
/// nodes stay up across catalog updates).
async fn watch_catalog(state: AppState) {
    let Some(dir) = state.catalog_dir.clone() else {
        return;
    };
    let manifest_path = dir.join(sivana_index::manifest::MANIFEST_FILE);
    let mut last = std::fs::metadata(&manifest_path)
        .map(|m| (m.len(), m.modified().ok()))
        .ok()
        .unwrap_or((0, None));
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        tick.tick().await;
        let cur = std::fs::metadata(&manifest_path)
            .map(|m| (m.len(), m.modified().ok()))
            .ok()
            .unwrap_or((0, None));
        if cur != last {
            last = cur;
            println!("manifest changed; reloading catalog...");
            let bundle = build_bundle(Some(&dir));
            *state.bundle.write().expect("bundle lock poisoned") = Arc::new(bundle);
            println!("catalog v{} is now live", state.snapshot().catalog_version);
        }
    }
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
        "catalog_version": state.snapshot().catalog_version,
        "engine": "landmark-v2",
    }))
}

async fn create_session(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
) -> Result<Json<serde_json::Value>, ApiError> {
    create_session_for_ip(state, peer.ip()).await
}

/// Session creation minus socket plumbing, so tests can drive it with
/// synthetic peer addresses.
///
/// Abuse resistance (§39): a fixed-window per-IP budget on session
/// creation. Raw websocket identifies bypass this (the load generator and
/// extension open them with self-assigned ids), but every live session
/// still counts against `MAX_CONCURRENT_SESSIONS`.
async fn create_session_for_ip(
    state: AppState,
    ip: IpAddr,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !state.session_rate.check(ip, Instant::now()) {
        return Err(ApiError::too_many_requests(
            "Too many sessions started from this address. Wait a moment and try again.",
        ));
    }
    let active = state.sessions.lock().await.len();
    if active >= MAX_CONCURRENT_SESSIONS {
        return Err(ApiError::too_many_requests(format!(
            "Server is at its concurrent-session limit ({MAX_CONCURRENT_SESSIONS}); \
             idle sessions expire within {}s.",
            SESSION_TTL.as_secs()
        )));
    }
    let id = state
        .next_session
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(Json(serde_json::json!({ "session_id": id })))
}

async fn get_recording(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<u32>,
) -> Json<serde_json::Value> {
    let titles = state.snapshot().titles.clone();
    match titles.get(&id) {
        Some(m) => Json(serde_json::json!({
            "recording_id": id,
            "title": m.title,
            "artist": m.artist,
            "artwork_url": m.artwork_url,
            "source_url": m.source_url,
            "source_provider": m.source_provider,
        })),
        None => Json(serde_json::json!({ "recording_id": id, "title": null })),
    }
}

async fn list_recordings(State(state): State<AppState>) -> Json<serde_json::Value> {
    let bundle = state.snapshot();
    let mut recordings = bundle
        .titles
        .iter()
        .map(|(&id, meta)| {
            serde_json::json!({
                "recording_id": id,
                "title": meta.title,
                "artist": meta.artist,
                "artwork_url": meta.artwork_url,
                "source_url": meta.source_url,
                "source_provider": meta.source_provider,
            })
        })
        .collect::<Vec<_>>();
    recordings.sort_by_key(|entry| {
        entry
            .get("recording_id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    });
    Json(serde_json::json!({
        "catalog_version": bundle.catalog_version,
        "recordings": recordings,
    }))
}

async fn youtube_metadata(
    State(state): State<AppState>,
    Query(query): Query<MetadataQuery>,
) -> Result<Json<LinkMetadata>, ApiError> {
    fetch_youtube_metadata(&state.http_client, &query.url)
        .await
        .map(Json)
}

async fn add_recording(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Gate 1: Authorization header (curl / scripts / extension). A wrong
    // header credential fails immediately; no header at all stays open
    // until after the multipart parse because browsers post the secret as
    // a form field instead.
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    let expected = state.ingest_token.as_deref();
    let mut needs_form_token = false;
    match (expected, bearer) {
        (None, _) | (Some(_), Some(_)) => {
            ingest_authorized(expected, bearer)?;
        }
        (Some(_), None) => needs_form_token = true,
    }

    let mut source_url = None;
    let mut audio = None;
    let mut form_token: Option<String> = None;
    let mut original_name = String::from("audio file");

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::bad_request("The upload form could not be read."))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "token" => {
                form_token = Some(
                    field
                        .text()
                        .await
                        .map_err(|_| ApiError::bad_request("The ingest token is invalid."))?,
                );
            }
            "source_url" => {
                let text = field
                    .text()
                    .await
                    .map_err(|_| ApiError::bad_request("The YouTube Music link is invalid."))?;
                if text.len() > 2_048 {
                    return Err(ApiError::bad_request("The YouTube Music link is too long."));
                }
                source_url = Some(text);
            }
            "audio" => {
                if let Some(file_name) = field.file_name() {
                    original_name = FsPath::new(file_name)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("audio file")
                        .to_string();
                }
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|_| ApiError::bad_request("The audio file could not be read."))?;
                if bytes.is_empty() {
                    return Err(ApiError::bad_request("Choose a non-empty audio file."));
                }
                audio = Some(bytes);
            }
            _ => {}
        }
    }

    // Gate 2: `token` form field (the browser uploader posts multipart and
    // cannot set headers from its plain FormData submission). Checked
    // before any other input validation so unauthenticated callers learn
    // about the gate first.
    if needs_form_token {
        ingest_authorized(expected, form_token.as_deref())?;
    }

    let source_url = source_url
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("Paste a YouTube Music link first."))?;
    let audio = audio.ok_or_else(|| ApiError::bad_request("Drop an audio file first."))?;
    let metadata = fetch_youtube_metadata(&state.http_client, &source_url).await?;
    let catalog_dir = state
        .catalog_dir
        .clone()
        .ok_or_else(|| ApiError::internal("This server has no writable catalog configured."))?;

    let upload_dir = catalog_dir.join(".uploads");
    tokio::fs::create_dir_all(&upload_dir)
        .await
        .map_err(|e| ApiError::internal(format!("Could not prepare the upload: {e}")))?;
    let serial = state
        .next_upload
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let temp_path = upload_dir.join(format!("{stamp}-{serial}.audio"));
    tokio::fs::write(&temp_path, &audio)
        .await
        .map_err(|e| ApiError::internal(format!("Could not stage the audio file: {e}")))?;
    drop(audio);

    let recording_metadata = RecordingMetadata {
        title: metadata.title.clone(),
        artist: metadata.artist.clone(),
        artwork_url: Some(metadata.artwork_url.clone()),
        source_url: Some(metadata.source_url.clone()),
        source_provider: Some(metadata.provider.clone()),
    };

    // Catalog mutations are serialized so concurrent local uploads cannot race
    // on sources.json, recordings.json, or the manifest swap.
    let _ingest_guard = state.ingest_lock.lock().await;
    let ingest_dir = catalog_dir.clone();
    let ingest_path = temp_path.clone();
    let ingest_result = tokio::task::spawn_blocking(move || {
        sivana_ingest::add_files_with_metadata(
            &ingest_dir,
            &[(ingest_path, Some(recording_metadata))],
            1,
        )
    })
    .await
    .map_err(|e| ApiError::internal(format!("Fingerprint worker stopped unexpectedly: {e}")))?;
    let _ = tokio::fs::remove_file(&temp_path).await;
    let stats = ingest_result.map_err(ApiError::internal)?;
    if let Some((_, error)) = stats.failed.first() {
        return Err(ApiError::bad_request(format!(
            "{original_name} could not be fingerprinted: {error}"
        )));
    }
    let (recording_id, duplicate) = if let Some(&id) = stats.added.first() {
        (id, false)
    } else if let Some(&id) = stats.existing.first() {
        (id, true)
    } else {
        return Err(ApiError::internal(
            "The fingerprint finished without creating a catalog record.",
        ));
    };

    // Make the new track queryable immediately. The watcher remains the
    // recovery path for catalog changes made by the CLI.
    let fresh = Arc::new(build_bundle(Some(&catalog_dir)));
    let catalog_version = fresh.catalog_version;
    *state.bundle.write().expect("bundle lock poisoned") = fresh;

    Ok(Json(serde_json::json!({
        "recording_id": recording_id,
        "duplicate": duplicate,
        "catalog_version": catalog_version,
        "postings": stats.postings,
        "title": metadata.title,
        "artist": metadata.artist,
        "artwork_url": metadata.artwork_url,
        "source_url": metadata.source_url,
        "provider": metadata.provider,
    })))
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
    let connected_at = Instant::now();

    loop {
        // Eager timeout tick: evaluate the session clock even when the
        // client goes quiet, so every session terminates (Phase 10 fix
        // surfaced by the load generator).
        let next = tokio::time::timeout(Duration::from_millis(250), socket.recv()).await;
        let msg = match next {
            Err(_elapsed) => {
                let mut sessions = state.sessions.lock().await;
                let capture = if let Some(s) = sessions.get_mut(&session_id) {
                    if s.poll_timeout() == recognition::RecognitionState::NoMatch {
                        Some(s.capture_seconds())
                    } else {
                        None
                    }
                } else if connected_at.elapsed().as_secs_f32() > recognition::MAX_CAPTURE_SECONDS {
                    Some(connected_at.elapsed().as_secs_f32())
                } else {
                    None
                };
                drop(sessions);
                if let Some(capture) = capture {
                    send_event(
                        &mut socket,
                        serde_json::json!({ "event": "no_match", "capture_seconds": capture }),
                    )
                    .await;
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
                continue;
            }
            Ok(None) => break,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(m))) => m,
        };
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
        sessions.retain(|_, s| s.capture_seconds() < SESSION_TTL.as_secs_f32());

        let bundle = state.snapshot();
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
            &bundle.index,
            &state.params,
        );
        let capture = session.capture_seconds();
        let outcome = session.outcome.clone();

        let event = match (new_state, outcome) {
            (recognition::RecognitionState::ConfidentMatch, Some(o)) => {
                let rec_id = o.recording.as_u32();
                let title = bundle.titles.get(&rec_id);
                serde_json::json!({
                    "event": "matched",
                    "recording_id": rec_id,
                    "title": title.map(|t| t.title.clone()),
                    "artist": title.map(|t| t.artist.clone()),
                    "artwork_url": title.and_then(|t| t.artwork_url.clone()),
                    "source_url": title.and_then(|t| t.source_url.clone()),
                    "source_provider": title.and_then(|t| t.source_provider.clone()),
                    "offset_frames": o.offset_frames,
                    "inliers": o.inliers,
                    "concentration": o.offset_concentration,
                    "margin": o.margin_over_next,
                    "capture_seconds": capture,
                })
            }
            (recognition::RecognitionState::Candidate, Some(o)) => {
                serde_json::json!({
                    "event": "candidate",
                    "recording_id": o.recording.as_u32(),
                    "inliers": o.inliers,
                    "concentration": o.offset_concentration,
                    "margin": o.margin_over_next,
                    "capture_seconds": capture,
                })
            }
            (recognition::RecognitionState::NoMatch, Some(o)) => {
                serde_json::json!({
                    "event": "no_match",
                    "recording_id": o.recording.as_u32(),
                    "inliers": o.inliers,
                    "concentration": o.offset_concentration,
                    "margin": o.margin_over_next,
                    "capture_seconds": capture,
                })
            }
            (recognition::RecognitionState::Listening, Some(o)) => {
                serde_json::json!({
                    "event": "listening",
                    "recording_id": o.recording.as_u32(),
                    "inliers": o.inliers,
                    "concentration": o.offset_concentration,
                    "margin": o.margin_over_next,
                    "capture_seconds": capture,
                })
            }
            (recognition::RecognitionState::Candidate, None) => {
                serde_json::json!({ "event": "candidate", "capture_seconds": capture })
            }
            (recognition::RecognitionState::NoMatch, None) => {
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

/// Local UI and engine assets must never go stale independently. A cached
/// stylesheet paired with new result markup breaks the layout; a cached WASM
/// binary paired with a current catalog produces zero hash collisions.
async fn no_cache_engine_assets(
    uri: axum::http::Uri,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut res = next.run(req).await;
    let path = uri.path();
    let is_ui_asset =
        path == "/" || path.ends_with(".html") || path.ends_with(".css") || path.ends_with(".js");
    if is_ui_asset || path.starts_with("/wasm/") || path.ends_with(".wasm") {
        res.headers_mut().insert(
            "Cache-Control",
            axum::http::HeaderValue::from_static("no-store, max-age=0"),
        );
    }
    res
}

/// Minimal permissive CORS for localhost use. The Chrome extension's
/// offscreen document runs on a chrome-extension:// origin, so its session
/// POST is cross-origin and the browser preflights/blocks it unless the
/// server answers with CORS headers. Dev-server posture: allow everything.
async fn cors_allow_local(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let has_origin = req.headers().contains_key(axum::http::header::ORIGIN);
    if req.method() == axum::http::Method::OPTIONS {
        let mut res = axum::http::Response::new(axum::body::Body::empty());
        *res.status_mut() = axum::http::StatusCode::NO_CONTENT;
        let h = res.headers_mut();
        h.insert("Access-Control-Allow-Origin", axum::http::HeaderValue::from_static("*"));
        h.insert(
            "Access-Control-Allow-Methods",
            axum::http::HeaderValue::from_static("GET,POST"),
        );
        h.insert(
            "Access-Control-Allow-Headers",
            axum::http::HeaderValue::from_static("content-type"),
        );
        return res;
    }
    let mut res = next.run(req).await;
    if has_origin {
        res.headers_mut().insert(
            "Access-Control-Allow-Origin",
            axum::http::HeaderValue::from_static("*"),
        );
    }
    res
}

/// Route table shared by `main` and the integration-style tests, which
/// drive real requests through it with `tower::Service::call`.
fn build_router(state: AppState, web_dir: &FsPath) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/sessions", post(create_session))
        .route("/v1/identify/:session_id", get(ws_identify))
        .route("/v1/metadata/youtube", get(youtube_metadata))
        .route("/v1/recordings", get(list_recordings).post(add_recording))
        .route("/v1/recordings/:id", get(get_recording))
        .fallback_service(ServeDir::new(web_dir).append_index_html_on_directories(true))
        .layer(axum::middleware::from_fn(no_cache_engine_assets))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
        // Outermost: CORS must wrap every route (and preflights) so the
        // extension's offscreen document can create sessions.
        .layer(axum::middleware::from_fn(cors_allow_local))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let catalog = args
        .iter()
        .position(|a| a == "--catalog")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from("catalog")));
    let web_dir = args
        .iter()
        .position(|a| a == "--web")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("apps/web"));
    let ingest_token = args
        .iter()
        .position(|a| a == "--ingest-token")
        .and_then(|i| args.get(i + 1))
        .filter(|t| !t.is_empty())
        .cloned();

    if let Some(dir) = &catalog {
        std::fs::create_dir_all(dir).expect("create catalog directory");
    }
    let started = Instant::now();
    let bundle = Arc::new(build_bundle(catalog.as_ref()));
    println!(
        "catalog v{}: {} hashes indexed in {:.2}s",
        bundle.catalog_version,
        bundle.index.len(),
        started.elapsed().as_secs_f64()
    );

    let state = AppState {
        bundle: Arc::new(std::sync::RwLock::new(bundle)),
        catalog_dir: catalog.clone(),
        params: Arc::new(MatchParams::default()),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        ingest_lock: Arc::new(Mutex::new(())),
        ingest_token: ingest_token.as_deref().map(Arc::from),
        session_rate: Arc::new(RateLimiter::default()),
        next_session: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        next_upload: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        http_client: reqwest::Client::builder()
            .user_agent("Sivana/0.1 local catalog")
            .build()
            .expect("build metadata client"),
    };
    tokio::spawn(watch_catalog(state.clone()));

    let app = build_router(state, &web_dir);

    let addr = std::env::var("SIVANA_ADDR").unwrap_or_else(|_| "127.0.0.1:8077".into());
    if ingest_token.is_none() {
        // §39 posture line: make the open endpoint visible, not silent.
        println!("ingest endpoint open (no --ingest-token set)");
    }
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!(
        "sivana-api listening on http://{addr} (serving {})",
        web_dir.display()
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use axum::response::Response;
    use sivana_audio::fixtures;
    use sivana_landmark::LandmarkV2Config;
    use sivana_wasm::FingerprintEngine;

    const TEST_BOUNDARY: &str = "sivana-test-boundary";

    fn test_state(ingest_token: Option<&str>) -> AppState {
        AppState {
            bundle: Arc::new(std::sync::RwLock::new(Arc::new(build_bundle(None)))),
            catalog_dir: None,
            params: Arc::new(MatchParams::default()),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            ingest_lock: Arc::new(Mutex::new(())),
            ingest_token: ingest_token.map(Arc::from),
            session_rate: Arc::new(RateLimiter::default()),
            next_session: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            next_upload: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            http_client: reqwest::Client::new(),
        }
    }

    /// Minimal multipart/form-data encoder for the uploader contract.
    fn multipart_body(fields: &[(&str, &str)]) -> Body {
        let mut body = String::new();
        for (name, value) in fields {
            body.push_str(&format!(
                "--{TEST_BOUNDARY}\r\ncontent-disposition: form-data; name=\"{name}\"\r\n\r\n"
            ));
            body.push_str(value);
            body.push_str("\r\n");
        }
        body.push_str(&format!("--{TEST_BOUNDARY}--\r\n"));
        Body::from(body)
    }

    /// Drive a real request through the production route table, tagged
    /// with a synthetic peer address so ConnectInfo resolves like it does
    /// behind `into_make_service_with_connect_info`.
    async fn post(
        state: AppState,
        uri: &str,
        headers: &[(&str, &str)],
        body: Body,
        ip: IpAddr,
    ) -> Response {
        let app = build_router(state, std::path::Path::new("apps/web"));
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("content-type", format!("multipart/form-data; boundary={TEST_BOUNDARY}"));
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let mut req = req.body(body).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(std::net::SocketAddr::new(ip, 40000)));
        tower::ServiceExt::oneshot(app, req)
            .await
            .expect("router service ready")
    }

    fn status_of(response: Response) -> StatusCode {
        response.status()
    }

    /// Auth gate: configured token rejects missing/wrong credentials.
    #[test]
    fn ingest_token_is_required_when_configured() {
        assert!(ingest_authorized(Some("secret"), None).is_err());
        assert!(ingest_authorized(Some("secret"), Some("wrong")).is_err());
        assert_eq!(
            ingest_authorized(Some("secret"), None).unwrap_err().status,
            StatusCode::UNAUTHORIZED
        );
        assert!(ingest_authorized(Some("secret"), Some("secret")).is_ok());
        // No token configured => open, whatever the caller presents.
        assert!(ingest_authorized(None, None).is_ok());
        assert!(ingest_authorized(None, Some("anything")).is_ok());
    }

    /// Router-level: no token configured keeps the endpoint open (the
    /// default local-dev posture).
    #[tokio::test]
    async fn upload_is_open_without_a_configured_token() {
        let state = test_state(None);
        let resp = post(
            state,
            "/v1/recordings",
            &[],
            multipart_body(&[("source_url", "not even a url")]),
            IpAddr::from([127, 0, 0, 1]),
        )
        .await;
        // Anything but an auth failure proves the gate stayed open; the
        // request then dies later on input validation.
        assert_ne!(status_of(resp), StatusCode::UNAUTHORIZED);
    }

    /// Router-level: with a token configured, missing AND wrong
    /// credentials are rejected before any validation work.
    #[tokio::test]
    async fn upload_rejects_missing_and_wrong_tokens() {
        let state = test_state(Some("hunter2"));
        let resp = post(
            state.clone(),
            "/v1/recordings",
            &[],
            multipart_body(&[]),
            IpAddr::from([127, 0, 0, 1]),
        )
        .await;
        assert_eq!(status_of(resp), StatusCode::UNAUTHORIZED);

        let resp = post(
            state,
            "/v1/recordings",
            &[("authorization", "Bearer wrong-password")],
            multipart_body(&[]),
            IpAddr::from([127, 0, 0, 2]),
        )
        .await;
        assert_eq!(status_of(resp), StatusCode::UNAUTHORIZED);
    }

    /// Router-level: the documented Bearer credential passes the gate.
    /// The request then dies on ordinary validation (400, no audio field)
    /// rather than on auth (401), proving authorization happened.
    #[tokio::test]
    async fn upload_accepts_bearer_header() {
        let state = test_state(Some("hunter2"));
        let resp = post(
            state,
            "/v1/recordings",
            &[("authorization", "Bearer hunter2")],
            multipart_body(&[("source_url", "definitely not a url")]),
            IpAddr::from([10, 0, 0, 1]),
        )
        .await;
        assert_eq!(status_of(resp), StatusCode::BAD_REQUEST);
    }

    /// Router-level: the browser uploader posts multipart FormData, which
    /// cannot carry custom headers, so the `token` field must also work.
    #[tokio::test]
    async fn upload_accepts_token_form_field() {
        let state = test_state(Some("hunter2"));
        let resp = post(
            state,
            "/v1/recordings",
            &[],
            multipart_body(&[
                ("token", "hunter2"),
                ("source_url", "definitely not a url"),
            ]),
            IpAddr::from([10, 0, 0, 2]),
        )
        .await;
        assert_eq!(status_of(resp), StatusCode::BAD_REQUEST);

        // A wrong field value still fails closed.
        let state = test_state(Some("hunter2"));
        let resp = post(
            state,
            "/v1/recordings",
            &[],
            multipart_body(&[("token", "nope")]),
            IpAddr::from([10, 0, 0, 3]),
        )
        .await;
        assert_eq!(status_of(resp), StatusCode::UNAUTHORIZED);
    }

    /// Session cap: creation succeeds under the ceiling and 429s once the
    /// live-session count reaches MAX_CONCURRENT_SESSIONS.
    #[tokio::test]
    async fn session_creation_rejects_at_the_concurrent_cap() {
        let state = test_state(None);
        let ip = IpAddr::from([192, 168, 0, 1]);

        // Under the cap: ids flow.
        let resp = create_session_for_ip(state.clone(), ip).await.unwrap();
        let id = resp
            .0
            .get("session_id")
            .and_then(serde_json::Value::as_u64)
            .expect("session id in response");
        assert!(id >= 1);

        // Fill the map to exactly the cap (sessions are TTL-evicted in
        // production, so map size == active count).
        for i in 0..MAX_CONCURRENT_SESSIONS {
            state.sessions.lock().await.insert(i as u64, recognition::RecognitionSession::new(22_050, 1024));
        }
        let err = create_session_for_ip(state.clone(), ip).await.unwrap_err();
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);

        // A different IP hits the same global ceiling.
        let err = create_session_for_ip(state, IpAddr::from([192, 168, 0, 2]))
            .await
            .unwrap_err();
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    }

    /// Rate limiter: budget exhausts within a window and refills at the
    /// next window boundary. Uses injected Instants, so it sleeps nowhere.
    #[test]
    fn rate_limiter_enforces_fixed_window() {
        let limiter = RateLimiter::default();
        let ip = IpAddr::from([203, 0, 113, 7]);
        let start = Instant::now();
        let limit = 3;
        let window = Duration::from_secs(60);

        for _ in 0..limit {
            assert!(limiter.check_with(ip, start, limit, window));
        }
        assert!(!limiter.check_with(ip, start + Duration::from_secs(1), limit, window));
        assert!(!limiter.check_with(ip, start + Duration::from_secs(59), limit, window));
        // Window rolls over at >= window elapsed since its start.
        assert!(limiter.check_with(ip, start + window, limit, window));
        // ...and the fresh window has its own full budget again: two more
        // hits land inside it, the fourth is refused.
        assert!(limiter.check_with(ip, start + window + Duration::from_secs(1), limit, window));
        assert!(limiter.check_with(ip, start + window + Duration::from_secs(2), limit, window));
        assert!(!limiter.check_with(ip, start + window + Duration::from_secs(3), limit, window));
    }

    /// Rate limiter: budgets are per IP, not shared globally.
    #[test]
    fn rate_limiter_tracks_ips_independently() {
        let limiter = RateLimiter::default();
        let a = IpAddr::from([203, 0, 113, 8]);
        let b = IpAddr::from([203, 0, 113, 9]);
        let now = Instant::now();
        for _ in 0..SESSIONS_PER_WINDOW {
            assert!(limiter.check(a, now));
        }
        assert!(!limiter.check(a, now));
        // A different address still has its own untouched budget.
        assert!(limiter.check(b, now));
    }

    /// Rate limiter hygiene: the IP table cannot grow without bound even
    /// when every hit comes from a unique address.
    #[test]
    fn rate_limiter_table_stays_bounded() {
        let limiter = RateLimiter::default();
        let now = Instant::now();
        // Unique addresses across two octets, far beyond the table cap; the
        // recycle path must keep the map pinned at the ceiling instead of
        // ballooning. limit=1 so each address is one insert.
        for i in 0..(RATE_TABLE_MAX_ENTRIES + 512) {
            let hi = ((i / 256) % 256) as u8;
            let lo = (i % 256) as u8;
            let ip = IpAddr::from([198, 51, hi, lo]);
            assert!(
                limiter.check_with(ip, now + Duration::from_millis(i as u64), 1, RATE_WINDOW),
                "limit=1 with a fresh IP is always admitted"
            );
        }
        let windows = limiter.windows.lock().unwrap();
        assert!(windows.len() <= RATE_TABLE_MAX_ENTRIES);
    }

    /// Idle entries are evicted from the rate table after the TTL, freeing
    /// space (and memory) on long-lived servers.
    #[test]
    fn rate_limiter_evicts_idle_entries() {
        let limiter = RateLimiter::default();
        let ip = IpAddr::from([192, 0, 2, 55]);
        let now = Instant::now();
        assert!(limiter.check(ip, now));
        assert_eq!(limiter.windows.lock().unwrap().len(), 1);
        // A much later visit drops the stale entry rather than keeping it.
        let later = now + RATE_TABLE_IDLE_TTL + Duration::from_secs(1);
        assert!(limiter.check(ip, later));
        assert_eq!(limiter.windows.lock().unwrap().len(), 1);
    }

    /// Session creation beyond the per-IP fixed-window budget 429s.
    #[tokio::test]
    async fn session_creation_is_rate_limited_per_ip() {
        let state = test_state(None);
        let ip = IpAddr::from([192, 168, 0, 9]);
        // Drain the real production budget for this IP.
        let now = Instant::now();
        for _ in 0..SESSIONS_PER_WINDOW {
            assert!(state.session_rate.check(ip, now));
        }
        let err = create_session_for_ip(state.clone(), ip).await.unwrap_err();
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        // Another IP is unaffected by the first one's exhaustion.
        let resp = create_session_for_ip(state, IpAddr::from([192, 168, 0, 10]))
            .await
            .unwrap();
        assert!(resp.0.get("session_id").is_some());
    }

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
        let excerpt = fixtures::excerpt(&song, 22_050, 2.5, 10.0);
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

    #[test]
    fn empty_catalog_is_a_finalized_matcher() {
        let bundle = build_bundle(None);
        let outcomes = bundle.index.query(&[], &MatchParams::default());
        assert!(outcomes.is_empty());
    }

    #[test]
    fn youtube_music_links_are_constrained_to_video_urls() {
        assert!(
            validate_youtube_music_url("https://music.youtube.com/watch?v=dQw4w9WgXcQ").is_ok()
        );
        assert!(validate_youtube_music_url("https://youtu.be/dQw4w9WgXcQ").is_ok());
        assert!(validate_youtube_music_url("http://music.youtube.com/watch?v=x").is_err());
        assert!(validate_youtube_music_url("https://example.com/watch?v=x").is_err());
        assert!(validate_youtube_music_url("https://music.youtube.com/playlist?list=x").is_err());
    }

    #[test]
    fn youtube_presentation_suffixes_do_not_become_track_titles() {
        assert_eq!(
            clean_youtube_title(
                "Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)",
                "Rick Astley"
            ),
            "Never Gonna Give You Up"
        );
        assert_eq!(
            clean_youtube_title("Song Name (feat. Guest)", "Artist"),
            "Song Name (feat. Guest)"
        );
    }

    #[test]
    fn youtube_topic_suffix_is_removed_from_artist_credit() {
        assert_eq!(clean_youtube_artist("Toby Fox - Topic"), "Toby Fox");
        assert_eq!(clean_youtube_artist("Toby Fox"), "Toby Fox");
    }

    #[test]
    fn square_artwork_requires_an_exact_track_and_artist() {
        let response = ItunesSearchResponse {
            results: vec![
                ItunesTrack {
                    track_name: "MEGALOVANIA (Remix)".into(),
                    artist_name: "Toby Fox".into(),
                    artwork_url100: "https://example.test/remix/100x100bb.jpg".into(),
                },
                ItunesTrack {
                    track_name: "Megalovania".into(),
                    artist_name: "Toby Fox".into(),
                    artwork_url100: "https://example.test/original/100x100bb.jpg".into(),
                },
            ],
        };
        assert_eq!(
            select_square_artwork(response, "MEGALOVANIA", "Toby Fox").as_deref(),
            Some("https://example.test/original/600x600bb.jpg")
        );
    }
}
