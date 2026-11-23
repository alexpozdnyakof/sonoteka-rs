//! Rule-based track selection shared by smart playlists and the radio
//! stream: a small JSON rule set → ordered track list.
//!
//! Rules JSON (all fields optional):
//! ```json
//! {
//!   "genres": ["Techno", "Ambient"],
//!   "artistIds": ["<artist id>", ...],
//!   "min_year": 1990, "max_year": 2005,
//!   "limit": 50,
//!   "order": "random" | "newest" | "top",
//!   "exclude_recent_days": 7
//! }
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, ToSql};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::broadcast;

use crate::{AppState, db};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Order {
    #[default]
    Random,
    Newest,
    Top,
}

#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub genres: Vec<String>,
    pub artist_ids: Vec<String>,
    pub min_year: Option<i64>,
    pub max_year: Option<i64>,
    pub limit: i64,
    pub order: Order,
    /// Anti-repeat: skip tracks played within the last N days.
    pub exclude_recent_days: Option<i64>,
}

impl Rules {
    pub fn parse(raw: &str) -> Result<Rules> {
        let v: Value = serde_json::from_str(raw).context("invalid rules JSON")?;
        let obj = v.as_object().context("rules must be a JSON object")?;
        let str_vec = |key: &str| -> Vec<String> {
            obj.get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default()
        };
        let order = match obj.get("order").and_then(Value::as_str) {
            Some("newest") => Order::Newest,
            Some("top") => Order::Top,
            _ => Order::Random,
        };
        Ok(Rules {
            genres: str_vec("genres"),
            artist_ids: str_vec("artistIds"),
            min_year: obj.get("min_year").and_then(Value::as_i64),
            max_year: obj.get("max_year").and_then(Value::as_i64),
            limit: obj
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(50)
                .clamp(1, 500),
            order,
            exclude_recent_days: obj.get("exclude_recent_days").and_then(Value::as_i64),
        })
    }
}

/// Candidates for the rules, ordered and capped. Genre matching is
/// case-insensitive (tags carry `Techno`/`techno` variants); ids match exact.
pub fn tracks_by_rules(conn: &Connection, rules: &Rules, now: i64) -> Result<Vec<db::TrackRow>> {
    let mut wheres = vec!["1=1".to_string()];
    let mut args: Vec<Box<dyn ToSql>> = Vec::new();
    let push_in = |wheres: &mut Vec<String>, args: &mut Vec<Box<dyn ToSql>>, col: &str, values: &[String]| {
        if values.is_empty() {
            return;
        }
        let marks = std::iter::repeat_n("?", values.len())
            .collect::<Vec<_>>()
            .join(",");
        wheres.push(format!("{col} IN ({marks})"));
        for v in values {
            args.push(Box::new(v.clone()));
        }
    };
    push_in(
        &mut wheres,
        &mut args,
        "LOWER(t.genre)",
        &rules.genres.iter().map(|g| g.to_lowercase()).collect::<Vec<_>>(),
    );
    push_in(&mut wheres, &mut args, "t.artist_id", &rules.artist_ids);
    if let Some(y) = rules.min_year {
        wheres.push("al.year >= ?".to_string());
        args.push(Box::new(y));
    }
    if let Some(y) = rules.max_year {
        wheres.push("al.year <= ?".to_string());
        args.push(Box::new(y));
    }
    if let Some(days) = rules.exclude_recent_days {
        wheres.push("(t.played_at IS NULL OR t.played_at < ?)".to_string());
        args.push(Box::new(now - days * 86_400));
    }
    let order = match rules.order {
        Order::Random => "RANDOM()",
        Order::Newest => "al.added_at DESC, RANDOM()",
        Order::Top => "t.play_count DESC, RANDOM()",
    };
    let sql = format!(
        "SELECT {} FROM tracks t
         JOIN albums al ON al.id = t.album_id
         JOIN artists ar ON ar.id = t.artist_id
         WHERE {} ORDER BY {} LIMIT ?",
        db::TRACK_COLS,
        wheres.join(" AND "),
        order
    );
    args.push(Box::new(rules.limit));
    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt
        .query_map(rusqlite::params_from_iter(args.iter()), db::track_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Rebuilds one smart playlist's entries from its rules column.
pub fn regenerate(conn: &Connection, playlist_id: &str, raw: &str, now: i64) -> Result<()> {
    let rules = Rules::parse(raw)?;
    let tracks = tracks_by_rules(conn, &rules, now)?;
    let ids = tracks.iter().map(|t| t.id.clone()).collect::<Vec<_>>();
    db::set_playlist_entries(conn, playlist_id, &ids, now)
}

// ---------------------------------------------------------------------------
// Live radio: one shared queue broadcast to every listener as a continuous
// Opus-in-Ogg stream. ffmpeg transcodes track by track (gapless across
// tracks is a phase-4 item); ICY metadata is interleaved only for clients
// that ask for it (`Icy-MetaData: 1`), so browsers get a pure Ogg stream.
// ---------------------------------------------------------------------------

/// Audio bytes between ICY metadata blocks.
pub const METAINT: usize = 16 * 1024;

#[derive(Clone, Default)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
}

#[derive(Clone)]
pub struct RadioState {
    pub tx: broadcast::Sender<Vec<u8>>,
    pub now: Arc<StdMutex<NowPlaying>>,
    pub listeners: Arc<AtomicUsize>,
    pub engine_on: Arc<AtomicBool>,
}

impl RadioState {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1024);
        RadioState {
            tx,
            now: Arc::new(StdMutex::new(NowPlaying::default())),
            listeners: Arc::new(AtomicUsize::new(0)),
            engine_on: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Decrements the listener counter when a stream ends (client disconnect).
struct ListenerGuard(Arc<AtomicUsize>);

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Shared state for the HTTP handler: joins the broadcast, starts the engine
/// when the first listener arrives. Query params seed the queue rules the
/// first time (`?genres=Techno,Ambient&order=top`); later listeners join the
/// running queue.
pub async fn stream_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let mut rx = state.radio.tx.subscribe();
    state.radio.listeners.fetch_add(1, Ordering::SeqCst);
    spawn_engine(&state, &params);

    let icy = headers
        .get("Icy-MetaData")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    let now = state.radio.now.clone();
    let listeners = state.radio.listeners.clone();

    let body = Body::from_stream(async_stream::stream! {
        let _guard = ListenerGuard(listeners);
        let mut pending: Vec<u8> = Vec::with_capacity(METAINT + 256);
        loop {
            match rx.recv().await {
                Ok(chunk) => {
                    pending.extend_from_slice(&chunk);
                    let mut out = Vec::new();
                    while pending.len() >= METAINT {
                        let frame: Vec<u8> = pending.drain(..METAINT).collect();
                        out.extend_from_slice(&frame);
                        if icy {
                            out.extend(meta_block(&now));
                        }
                    }
                    if !out.is_empty() {
                        yield Ok::<_, std::convert::Infallible>(out);
                    }
                }
                // Too slow: drop what we missed, keep the stream live.
                Err(broadcast::error::RecvError::Lagged(_)) => pending.clear(),
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, "audio/ogg")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("icy-name", HeaderValue::from_static("sonoteka"))
        .header("icy-genre", HeaderValue::from_static("various"));
    if icy {
        builder = builder.header("icy-metaint", METAINT.to_string());
    }
    match builder.body(body) {
        Ok(resp) => resp.into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("radio stream build failed: {e}"),
        )
            .into_response(),
    }
}

/// One engine at a time; concurrent requests just join the broadcast.
fn spawn_engine(state: &AppState, params: &HashMap<String, String>) {
    if state.radio.engine_on.swap(true, Ordering::SeqCst) {
        return;
    }
    let state = state.clone();
    let rules = rules_from_params(params);
    tokio::spawn(async move {
        run_radio(&state, rules).await;
        state.radio.engine_on.store(false, Ordering::SeqCst);
    });
}

fn rules_from_params(params: &HashMap<String, String>) -> Rules {
    let genres = params
        .get("genres")
        .map(|g| {
            g.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let order = match params.get("order").map(String::as_str) {
        Some("newest") => Order::Newest,
        Some("top") => Order::Top,
        _ => Order::Random,
    };
    Rules {
        genres,
        limit: params
            .get("limit")
            .and_then(|v| v.parse().ok())
            .unwrap_or(100)
            .clamp(1, 500),
        order,
        exclude_recent_days: params
            .get("exclude_recent_days")
            .and_then(|v| v.parse().ok())
            .or(Some(1)),
        ..Rules::default()
    }
}

async fn run_radio(state: &AppState, rules: Rules) {
    loop {
        if state.radio.listeners.load(Ordering::Relaxed) == 0 {
            return;
        }
        let now = now_secs();
        let batch = {
            let conn = match state.db.lock() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[radio] db lock poisoned: {e}");
                    return;
                }
            };
            match tracks_by_rules(&conn, &rules, now) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[radio] rules query failed: {e:#}");
                    Vec::new()
                }
            }
        };
        if batch.is_empty() {
            eprintln!("[radio] no tracks match, retrying in 10s");
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }
        eprintln!(
            "[radio] batch: {} track(s), {:.0} min",
            batch.len(),
            batch.iter().map(|t| t.duration).sum::<f64>() / 60.0,
        );
        if let Err(e) = pump_batch(state, &batch).await {
            eprintln!("[radio] batch failed: {e:#}");
        }
    }
}

/// One continuous ffmpeg process per batch: the concat demuxer stitches the
/// queue into a single encode, so track transitions are sample-continuous
/// (Opus has no encoder priming, Ogg just keeps paging). NowPlaying is
/// driven by a timer over the tag durations — drift of a few tens of ms
/// per boundary is fine for display.
async fn pump_batch(state: &AppState, batch: &[db::TrackRow]) -> Result<()> {
    let list_path = write_concat_list(batch)?;
    let meta = tokio::spawn(now_playing_timer(
        state.radio.now.clone(),
        batch
            .iter()
            .map(|t| (t.artist_name.clone(), t.title.clone(), t.duration))
            .collect(),
    ));
    let result = pump_concat(state, &list_path).await;
    meta.abort();
    let _ = std::fs::remove_file(&list_path);
    result
}

async fn now_playing_timer(
    slot: Arc<StdMutex<NowPlaying>>,
    tracks: Vec<(String, String, f64)>,
) {
    for (artist, title, duration) in tracks {
        if let Ok(mut np) = slot.lock() {
            np.artist = artist.clone();
            np.title = title.clone();
        }
        eprintln!("[radio] {artist} — {title}");
        tokio::time::sleep(std::time::Duration::from_secs_f64(duration.max(0.0))).await;
    }
}

/// ffconcat list v1.0; single quotes in paths are escaped for the demuxer.
fn write_concat_list(batch: &[db::TrackRow]) -> Result<std::path::PathBuf> {
    use std::fmt::Write as _;
    let mut text = String::from("ffconcat version 1.0\n");
    for t in batch {
        let escaped = t.path.replace('\'', "'\\''");
        let _ = writeln!(text, "file '{escaped}'");
    }
    let path = std::env::temp_dir().join(format!(
        "sonoteka-radio-{}-{now_secs}.txt",
        std::process::id(),
        now_secs = now_secs()
    ));
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Transcode the whole concat list as one Opus-in-Ogg stream and broadcast
/// ffmpeg's stdout.
async fn pump_concat(state: &AppState, list_path: &std::path::Path) -> Result<()> {
    let ffmpeg = std::env::var("SONOTEKA_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let mut child = tokio::process::Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-re",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            &list_path.to_string_lossy(),
            "-vn",
            "-c:a",
            "libopus",
            "-b:a",
            "96k",
            "-f",
            "ogg",
            "pipe:1",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("spawn ffmpeg")?;
    let mut stdout = child.stdout.take().context("ffmpeg stdout")?;
    let mut buf = vec![0u8; 16 * 1024];
    let mut listener_gone = false;
    loop {
        let n = stdout.read(&mut buf).await.context("read ffmpeg stdout")?;
        if n == 0 {
            break;
        }
        if state.radio.tx.send(buf[..n].to_vec()).is_err() {
            // Nobody is listening; kill early, the engine loop will exit.
            let _ = child.kill().await;
            listener_gone = true;
            break;
        }
    }
    let status = child.wait().await.context("ffmpeg wait")?;
    if listener_gone {
        return Ok(());
    }
    if !status.success() {
        anyhow::bail!("ffmpeg exited with {status}");
    }
    Ok(())
}

/// ICY metadata block: 1-byte length (in 16-byte units) + payload padded to
/// a 16-byte boundary.
fn meta_block(now: &Arc<StdMutex<NowPlaying>>) -> Vec<u8> {
    let np = now
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| NowPlaying::default());
    let clean = |s: &str| s.replace('\'', "");
    let payload = format!("StreamTitle='{} - {}';", clean(&np.artist), clean(&np.title));
    let mut block = payload.into_bytes();
    let units = block.len() / 16 + 1;
    block.resize(units * 16, 0);
    let mut out = vec![units as u8];
    out.extend_from_slice(&block);
    out
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
