//! `stream` serves audio files with correct HTTP Range handling (seeking in
//! clients depends on it); `getCoverArt` extracts embedded artwork, shrinks it
//! to a ~500px thumbnail and caches it on disk. Transcoding is deliberately
//! out of scope — FLAC is served as-is.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use axum::{
    body::Body,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use lofty::file::TaggedFileExt;
use rusqlite::Connection;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{AppState, api::subsonic, db};

pub async fn stream(
    state: AppState,
    params: &HashMap<String, String>,
    range: Option<&str>,
) -> Response {
    let Some(id) = params.get("id") else {
        return subsonic::error(10, "missing required parameter: id");
    };
    let track = match db_get(&state, |c| db::get_track(c, id)) {
        Ok(Some(t)) => t,
        Ok(None) => return subsonic::error(70, &format!("song not found: {id}")),
        Err(e) => return subsonic::error(0, &format!("database error: {e:#}")),
    };
    serve_file(Path::new(&track.path), &track.content_type, range).await
}

pub async fn cover_art(state: AppState, params: &HashMap<String, String>) -> Response {
    let Some(id) = params.get("id") else {
        return subsonic::error(10, "missing required parameter: id");
    };
    if let Some(resp) = serve_cached_art(&state.cache_dir, id).await {
        return resp;
    }
    let source = match db_get(&state, |c| {
        if let Some(t) = db::get_track(c, id)? {
            if t.has_art {
                return Ok(Some(t.path));
            }
            db::album_art_source(c, &t.album_id)
        } else {
            db::album_art_source(c, id)
        }
    }) {
        Ok(Some(p)) => p,
        Ok(None) => return subsonic::error(70, "no cover art"),
        Err(e) => return subsonic::error(0, &format!("database error: {e:#}")),
    };
    match extract_art(Path::new(&source)) {
        Ok(Some(bytes)) => {
            let cache_path = state.cache_dir.join(art_name(id));
            if let Err(e) = tokio::fs::write(&cache_path, &bytes).await {
                eprintln!("cover art cache write {}: {e}", cache_path.display());
            }
            art_response(bytes)
        }
        Ok(None) => subsonic::error(70, "no cover art"),
        Err(e) => subsonic::error(0, &format!("cover art extraction failed: {e:#}")),
    }
}

fn db_get<T>(state: &AppState, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let conn = state
        .db
        .lock()
        .map_err(|e| anyhow::anyhow!("database lock poisoned: {e}"))?;
    f(&conn)
}

pub async fn serve_file(path: &Path, content_type: &str, range: Option<&str>) -> Response {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            return subsonic::error(70, &format!("cannot open {}: {e}", path.display()));
        }
    };
    let len = match file.metadata().await {
        Ok(m) => m.len(),
        Err(e) => return subsonic::error(0, &format!("cannot stat {}: {e}", path.display())),
    };
    let (start, end, partial) = match parse_range(range, len) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return subsonic::error(0, "seek failed");
    }
    let body_len = end - start + 1;
    let stream = ReaderStream::new(file.take(body_len));
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, body_len);
    if partial {
        builder = builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    match builder.body(Body::from_stream(stream)) {
        Ok(resp) => resp.into_response(),
        Err(e) => subsonic::error(0, &format!("stream build failed: {e}")),
    }
}

fn parse_range(header_val: Option<&str>, len: u64) -> Result<(u64, u64, bool), Response> {
    let Some(value) = header_val else {
        return Ok((0, len.saturating_sub(1), false));
    };
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return Ok((0, len.saturating_sub(1), false));
    };
    // Only the first range of a multi-range request is honoured; clients in
    // the Subsonic ecosystem issue single ranges.
    let spec = spec.split(',').next().unwrap_or(spec);
    let (start_s, end_s) = spec.split_once('-').unwrap_or((spec, ""));
    let (start, end) = if start_s.is_empty() {
        // Suffix range: the last N bytes.
        let n: u64 = end_s.parse().unwrap_or(0);
        (len.saturating_sub(n), len.saturating_sub(1))
    } else {
        let start = match start_s.parse::<u64>() {
            Ok(s) => s,
            Err(_) => return Ok((0, len.saturating_sub(1), false)),
        };
        let end = if end_s.is_empty() {
            len.saturating_sub(1)
        } else {
            end_s.parse::<u64>().unwrap_or(u64::MAX).min(len - 1)
        };
        (start, end)
    };
    if start >= len && len > 0 {
        return Err(Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .body(Body::empty())
            .unwrap()
            .into_response());
    }
    Ok((start, end.max(start), true))
}

fn extract_art(path: &Path) -> Result<Option<Vec<u8>>> {
    let tagged = lofty::read_from_path(path).context("read tags")?;
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(None);
    };
    let Some(pic) = tag.pictures().first() else {
        return Ok(None);
    };
    Ok(Some(thumbnail(pic.data())))
}

/// Embedded art is regularly 1400x1400+; clients only need a thumbnail.
/// Undecodable payloads pass through untouched (they are rare and still
/// better than no art at all).
fn thumbnail(data: &[u8]) -> Vec<u8> {
    let Ok(img) = image::load_from_memory(data) else {
        return data.to_vec();
    };
    let thumb = image::imageops::thumbnail(&img, 500, 500);
    let mut out = Vec::new();
    {
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        if enc.encode_image(&thumb).is_err() {
            return data.to_vec();
        }
    }
    if out.is_empty() {
        return data.to_vec();
    }
    out
}

async fn serve_cached_art(cache_dir: &Path, id: &str) -> Option<Response> {
    let prefix = art_name(id);
    let mut entries = tokio::fs::read_dir(cache_dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) {
            continue;
        }
        let bytes = tokio::fs::read(entry.path()).await.ok()?;
        return Some(art_response(bytes));
    }
    None
}

/// Versioned cache name: the "-500" scheme keeps pre-resize full-size files
/// from being served (they just sit there harmlessly).
fn art_name(id: &str) -> String {
    format!("art-{id}-500.")
}

fn art_response(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response()
}
