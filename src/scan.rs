use std::path::Path;

use anyhow::{Context, Result};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey};
use rusqlite::Connection;
use walkdir::WalkDir;

use crate::db;

const AUDIO_EXTENSIONS: &[&str] = &["flac", "mp3", "ogg", "opus", "m4a", "aac", "wav"];

pub fn rescan(conn: &mut Connection, music_folder: &Path, owner: &str) -> Result<()> {
    if !music_folder.exists() {
        eprintln!(
            "music folder {} does not exist, starting with an empty library",
            music_folder.display()
        );
        return Ok(());
    }
    db::wipe_library(conn)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut count = 0usize;
    for entry in WalkDir::new(music_folder)
        .follow_links(true)
        .sort_by_file_name()
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("walk error: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        match index_file(conn, path, &ext, now) {
            Ok(()) => count += 1,
            Err(e) => eprintln!("skip {}: {e:#}", path.display()),
        }
    }
    db::recompute_album_stats(conn)?;
    let imported = import_m3u_playlists(conn, music_folder, owner, now)?;
    if imported > 0 {
        eprintln!("m3u import: {imported} playlist(s)");
    }
    db::prune_orphan_playlist_tracks(conn)?;
    eprintln!("library scan complete: {count} tracks");
    Ok(())
}

/// Sidecar playlists: every `.m3u`/`.m3u8` in the library becomes a playlist
/// (id derived from its relative path, so re-imports update in place).
fn import_m3u_playlists(
    conn: &mut Connection,
    music_folder: &Path,
    owner: &str,
    now: i64,
) -> Result<usize> {
    let mut imported = 0usize;
    for entry in WalkDir::new(music_folder)
        .follow_links(true)
        .sort_by_file_name()
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("walk error: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if ext != "m3u" && ext != "m3u8" {
            continue;
        }
        match import_m3u(conn, music_folder, path, owner, now) {
            Ok(()) => imported += 1,
            Err(e) => eprintln!("m3u import failed {}: {e:#}", path.display()),
        }
    }
    Ok(imported)
}

fn import_m3u(
    conn: &mut Connection,
    music_folder: &Path,
    m3u_path: &Path,
    owner: &str,
    now: i64,
) -> Result<()> {
    let text = std::fs::read_to_string(m3u_path).context("read m3u")?;
    let base = m3u_path.parent().unwrap_or(music_folder);
    let mut wanted_paths = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        // m3u entries are usually relative to the file's own directory.
        let p = if l.starts_with('/') {
            std::path::PathBuf::from(l)
        } else {
            normalize_path(&base.join(l))
        };
        wanted_paths.push(p.to_string_lossy().into_owned());
    }
    let known = db::track_ids_for_paths(conn, &wanted_paths)?;
    let by_path: std::collections::HashMap<String, String> = known.into_iter().collect();
    let track_ids: Vec<String> = wanted_paths
        .iter()
        .filter_map(|p| by_path.get(p).cloned())
        .collect();
    let rel = m3u_path.strip_prefix(music_folder).unwrap_or(m3u_path);
    let id = db::stable_id(&format!("playlist:import:{}", rel.display()));
    let name = m3u_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "imported".to_string());
    db::upsert_imported_playlist(conn, &id, &name, owner, now)?;
    db::set_playlist_entries(conn, &id, &track_ids, now)?;
    Ok(())
}

/// Lexical cleanup (`a/./b`, `a/../c`) without touching the filesystem.
fn normalize_path(p: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn index_file(conn: &mut Connection, path: &Path, ext: &str, now: i64) -> Result<()> {
    let tagged = lofty::read_from_path(path).context("read tags")?;
    let props = tagged.properties();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let title = tag_string(tag, |t| t.title()).unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let track_artist =
        tag_string(tag, |t| t.artist()).unwrap_or_else(|| "Unknown Artist".to_string());
    let album_name =
        tag_string(tag, |t| t.album()).unwrap_or_else(|| "Unknown Album".to_string());
    let album_artist = tag
        .and_then(|t| t.get_string(ItemKey::AlbumArtist))
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| track_artist.clone());
    let year = tag.and_then(|t| t.date()).map(|d| d.year as u32);
    let track_number = tag.and_then(|t| t.track());
    let disc_number = tag.and_then(|t| t.disk());
    let genre = tag_string(tag, |t| t.genre());
    let has_art = tag.map(|t| !t.pictures().is_empty()).unwrap_or(false);

    let duration_secs = props.duration().as_secs_f64();
    let size_bytes = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
    let content_type = content_type_for(ext);

    let path_str = path.to_string_lossy().into_owned();
    let tx = conn.transaction()?;
    // Album is grouped under its album artist; the track itself keeps its
    // own artist credit (compilation tracks land in one album this way).
    let album_artist_id = db::upsert_artist(&tx, &album_artist)?;
    let track_artist_id = db::upsert_artist(&tx, &track_artist)?;
    let album_id = db::upsert_album(&tx, &album_name, &album_artist_id, year, now)?;
    db::insert_track(
        &tx,
        &db::TrackInsert {
            path: &path_str,
            title: &title,
            album_id: &album_id,
            artist_id: &track_artist_id,
            track_number,
            disc_number,
            duration_secs,
            size_bytes,
            suffix: ext,
            content_type,
            bit_rate: props.overall_bitrate(),
            sample_rate: props.sample_rate(),
            genre,
            has_art,
        },
    )?;
    tx.commit()?;
    Ok(())
}

fn tag_string<F>(tag: Option<&lofty::tag::Tag>, f: F) -> Option<String>
where
    F: FnOnce(&lofty::tag::Tag) -> Option<std::borrow::Cow<'_, str>>,
{
    tag.and_then(f)
        .map(|s| s.into_owned())
        .filter(|s| !s.trim().is_empty())
}

fn content_type_for(ext: &str) -> &'static str {
    match ext {
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "ogg" | "opus" => "audio/ogg",
        "m4a" | "aac" => "audio/mp4",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}
