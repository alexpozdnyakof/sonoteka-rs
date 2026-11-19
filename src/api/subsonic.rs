use std::collections::HashMap;

use axum::{
    Json,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::{AppState, db, radio};

pub const API_VERSION: &str = "1.16.1";
pub const IGNORED_ARTICLES: &str = "The El La Los Las Le Les Os As O A";

const ARTICLES: &[&str] = &[
    "the ", "el ", "la ", "los ", "las ", "le ", "les ", "os ", "as ", "o ", "a ", "an ",
];

/// Subsonic auth: `u` + token pair `t`/`s` where t = md5(password + salt),
/// or the legacy `p` = hex(password). Single user, credentials from env.
pub fn check_auth(state: &AppState, params: &HashMap<String, String>) -> Result<(), Response> {
    let username = params
        .get("u")
        .ok_or_else(|| error(10, "missing required parameter: u"))?;
    if username != &state.user {
        return Err(error(40, "wrong username or password"));
    }
    match (params.get("t"), params.get("s")) {
        (Some(t), Some(s)) => {
            // Subsonic uses md5(password + salt); OpenSubsonic clients
            // (Feishin among them) may use sha256(password + salt).
            use md5::{Digest, Md5};
            use sha2::Sha256;
            let payload = format!("{}{}", state.password, s);
            let ok = hex::encode(Md5::digest(payload.as_bytes())).eq_ignore_ascii_case(t)
                || hex::encode(Sha256::digest(payload.as_bytes())).eq_ignore_ascii_case(t);
            if ok {
                Ok(())
            } else {
                Err(error(40, "wrong username or password"))
            }
        }
        _ => match params.get("p") {
            Some(p) => {
                let expected = hex::encode(state.password.as_bytes());
                if expected.eq_ignore_ascii_case(p) {
                    Ok(())
                } else {
                    Err(error(40, "wrong username or password"))
                }
            }
            None => Err(error(10, "missing authentication parameters (t/s or p)")),
        },
    }
}

pub fn ping() -> Response {
    ok(json!({}))
}

pub fn get_artists(state: &AppState) -> Response {
    let mut index = artists_buckets(state);
    if let Value::Object(ref mut m) = index {
        m.insert("ignoredArticles".to_string(), json!(IGNORED_ARTICLES));
    }
    ok(json!({ "artists": index }))
}

/// Legacy folder-browse entry points. Navidrome simulates the directory
/// tree from tags (`/Artist/Album/...`); we mirror that: `getIndexes`
/// lists artists, `getMusicDirectory` resolves an artist id to album
/// directories and an album id to songs.
pub fn get_indexes(state: &AppState) -> Response {
    let mut index = artists_buckets(state);
    if let Value::Object(ref mut m) = index {
        m.insert("ignoredArticles".to_string(), json!(IGNORED_ARTICLES));
        m.insert("lastModified".to_string(), json!(0));
        m.insert("child".to_string(), json!([]));
    }
    ok(json!({ "indexes": index }))
}

pub fn get_music_directory(state: &AppState, params: &HashMap<String, String>) -> Response {
    let id = match params.get("id") {
        Some(i) => i,
        None => return error(10, "missing required parameter: id"),
    };
    // Album id → songs; artist id → album directories.
    let album = with_db(state, |c| db::get_album(c, id));
    match album {
        Ok(Some(album)) => {
            let tracks = match with_db(state, |c| db::tracks_for_album(c, id)) {
                Ok(t) => t,
                Err(e) => return error(0, &format!("database error: {e:#}")),
            };
            let mut dir = album_dir_json(&album);
            if let Value::Object(ref mut m) = dir {
                m.insert(
                    "child".to_string(),
                    json!(tracks.iter().map(child_json).collect::<Vec<_>>()),
                );
            }
            return ok(json!({ "directory": dir }));
        }
        Ok(None) => {}
        Err(e) => return error(0, &format!("database error: {e:#}")),
    }
    let artist = match with_db(state, |c| db::get_artist(c, id)) {
        Ok(a) => a,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    match artist {
        Some(artist) => {
            let albums = match with_db(state, |c| db::albums_for_artist(c, id)) {
                Ok(a) => a,
                Err(e) => return error(0, &format!("database error: {e:#}")),
            };
            let mut dir = json!({
                "id": artist.id,
                "name": artist.name,
                "child": [],
            });
            if let Value::Object(ref mut m) = dir {
                m.insert(
                    "child".to_string(),
                    json!(albums.iter().map(album_dir_json).collect::<Vec<_>>()),
                );
            }
            ok(json!({ "directory": dir }))
        }
        None => error(70, &format!("directory not found: {id}")),
    }
}

pub fn get_artist(state: &AppState, params: &HashMap<String, String>) -> Response {
    let id = match params.get("id") {
        Some(i) => i,
        None => return error(10, "missing required parameter: id"),
    };
    let (artist, albums) = {
        let artist = match with_db(state, |c| db::get_artist(c, id)) {
            Ok(Some(a)) => a,
            Ok(None) => return error(70, &format!("artist not found: {id}")),
            Err(e) => return error(0, &format!("database error: {e:#}")),
        };
        let albums = match with_db(state, |c| db::albums_for_artist(c, id)) {
            Ok(a) => a,
            Err(e) => return error(0, &format!("database error: {e:#}")),
        };
        (artist, albums)
    };
    ok(json!({
        "artist": {
            "id": artist.id,
            "name": artist.name,
            "albumCount": artist.album_count,
            "coverArt": artist.id,
            "album": albums.iter().map(album_json).collect::<Vec<_>>(),
        }
    }))
}

pub fn get_album_list2(state: &AppState, params: &HashMap<String, String>) -> Response {
    album_list_response(state, params, "albumList2")
}

/// Legacy v1 variant of getAlbumList2 — same data, different response key.
pub fn get_album_list(state: &AppState, params: &HashMap<String, String>) -> Response {
    album_list_response(state, params, "albumList")
}

fn album_list_response(
    state: &AppState,
    params: &HashMap<String, String>,
    key: &str,
) -> Response {
    let size = params
        .get("size")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(10);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if params.get("type").map(String::as_str) == Some("byGenre") {
        let genre = match params.get("genre") {
            Some(g) if !g.trim().is_empty() => g.clone(),
            _ => return error(10, "missing required parameter: genre"),
        };
        let albums = match with_db(state, |c| db::albums_by_genre(c, &genre, size, offset)) {
            Ok(a) => a,
            Err(e) => return error(0, &format!("database error: {e:#}")),
        };
        return ok(json!({ key: { "album": albums.iter().map(album_json).collect::<Vec<_>>() } }));
    }
    let sort = match params.get("type").map(String::as_str) {
        Some("newest") | Some("recent") | Some("frequent") => db::AlbumSort::Newest,
        Some("random") => db::AlbumSort::Random,
        Some("alphabeticalByArtist") | Some("byArtist") | Some("starred") => {
            db::AlbumSort::ByArtist
        }
        _ => db::AlbumSort::ByName,
    };
    let size = params
        .get("size")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(10);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let albums = match with_db(state, |c| db::list_albums(c, sort, size, offset)) {
        Ok(a) => a,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    let list: Vec<Value> = albums.iter().map(album_json).collect();
    ok(json!({ key: { "album": list } }))
}

pub fn get_genres(state: &AppState) -> Response {
    let genres = match with_db(state, db::list_genres) {
        Ok(g) => g,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    ok(json!({
        "genres": {
            "genre": genres
                .iter()
                .map(|g| json!({
                    "value": g.name,
                    "songCount": g.song_count,
                    "albumCount": g.album_count,
                }))
                .collect::<Vec<_>>(),
        }
    }))
}

pub fn get_songs_by_genre(state: &AppState, params: &HashMap<String, String>) -> Response {
    let genre = match params.get("genre") {
        Some(g) if !g.trim().is_empty() => g.clone(),
        _ => return error(10, "missing required parameter: genre"),
    };
    let count = params
        .get("count")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(10);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let songs = match with_db(state, |c| db::songs_by_genre(c, &genre, count, offset)) {
        Ok(s) => s,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    ok(json!({
        "songsByGenre": { "song": songs.iter().map(child_json).collect::<Vec<_>>() }
    }))
}

pub fn get_playlists(state: &AppState) -> Response {
    let playlists = match with_db(state, db::list_playlists) {
        Ok(p) => p,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    ok(json!({
        "playlists": {
            "playlist": playlists.iter().map(|p| playlist_json(p, None)).collect::<Vec<_>>(),
        }
    }))
}

pub fn get_playlist(state: &AppState, params: &HashMap<String, String>) -> Response {
    let id = match params.get("id") {
        Some(i) => i,
        None => return error(10, "missing required parameter: id"),
    };
    let (playlist, entries) = {
        let playlist = match with_db(state, |c| db::playlist_by_id(c, id)) {
            Ok(Some(p)) => p,
            Ok(None) => return error(70, &format!("playlist not found: {id}")),
            Err(e) => return error(0, &format!("database error: {e:#}")),
        };
        let entries = match with_db(state, |c| db::playlist_entries(c, id)) {
            Ok(t) => t,
            Err(e) => return error(0, &format!("database error: {e:#}")),
        };
        (playlist, entries)
    };
    ok(json!({ "playlist": playlist_json(&playlist, Some(&entries)) }))
}

/// Subsonic packs repeated params as `songId=a&songId=b`; those arrive in
/// `multi` (the last-value-wins `params` map can't hold them).
pub fn create_playlist(
    state: &AppState,
    params: &HashMap<String, String>,
    multi: &HashMap<String, Vec<String>>,
) -> Response {
    let requested: Vec<String> = multi
        .get("songId")
        .map(|v| v.iter().filter(|s| !s.is_empty()).cloned().collect())
        .unwrap_or_default();
    let song_ids = match with_db(state, |c| db::existing_track_ids(c, &requested)) {
        Ok(ids) => ids,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    let now = now_secs();
    let playlist_id = match params.get("playlistId") {
        // createPlaylist with playlistId overwrites that playlist's contents.
        Some(pid) => {
            let exists = match with_db(state, |c| db::playlist_by_id(c, pid)) {
                Ok(p) => p,
                Err(e) => return error(0, &format!("database error: {e:#}")),
            };
            if exists.is_none() {
                return error(70, &format!("playlist not found: {pid}"));
            }
            if let Err(e) = with_db(state, |c| db::set_playlist_entries(c, pid, &song_ids, now)) {
                return error(0, &format!("database error: {e:#}"));
            }
            pid.clone()
        }
        None => {
            let name = match params.get("name").filter(|n| !n.trim().is_empty()) {
                Some(n) => n.clone(),
                None => return error(10, "missing required parameter: name"),
            };
            let playlist = match with_db(state, |c| db::create_playlist(c, &name, &state.user, now))
            {
                Ok(p) => p,
                Err(e) => return error(0, &format!("database error: {e:#}")),
            };
            if let Some(raw) = params.get("rules").filter(|r| !r.trim().is_empty()) {
                if let Err(e) = radio::Rules::parse(raw) {
                    return error(10, &format!("invalid rules: {e:#}"));
                }
                let result = with_db(state, |c| {
                    db::set_playlist_rules(c, &playlist.id, Some(raw), now)?;
                    radio::regenerate(c, &playlist.id, raw, now)
                });
                if let Err(e) = result {
                    return error(0, &format!("database error: {e:#}"));
                }
            } else if let Err(e) =
                with_db(state, |c| db::set_playlist_entries(c, &playlist.id, &song_ids, now))
            {
                return error(0, &format!("database error: {e:#}"));
            }
            playlist.id
        }
    };
    respond_playlist(state, &playlist_id)
}

pub fn update_playlist(
    state: &AppState,
    params: &HashMap<String, String>,
    multi: &HashMap<String, Vec<String>>,
) -> Response {
    let id = match params.get("playlistId") {
        Some(i) => i,
        None => return error(10, "missing required parameter: playlistId"),
    };
    let name = params.get("name").filter(|n| !n.trim().is_empty());
    let comment = params.get("comment").map(String::as_str);
    let public = params.get("public").and_then(|v| parse_bool(v));
    let remove_indexes: Vec<i64> = multi
        .get("songIndexToRemove")
        .map(|v| v.iter().filter_map(|s| s.parse().ok()).collect())
        .unwrap_or_default();
    let add_requested: Vec<String> = multi
        .get("songIdToAdd")
        .map(|v| v.iter().filter(|s| !s.is_empty()).cloned().collect())
        .unwrap_or_default();
    let add_ids = match with_db(state, |c| db::existing_track_ids(c, &add_requested)) {
        Ok(ids) => ids,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    let exists = match with_db(state, |c| db::playlist_by_id(c, id)) {
        Ok(p) => p,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    if exists.is_none() {
        return error(70, &format!("playlist not found: {id}"));
    }
    let now = now_secs();
    let entries_changed = !remove_indexes.is_empty() || !add_ids.is_empty();
    if entries_changed {
        let result = with_db(state, |c| {
            let mut entries = db::playlist_entries(c, id)?;
            for idx in remove_indexes {
                if idx >= 0 && (idx as usize) < entries.len() {
                    entries.remove(idx as usize);
                }
            }
            let mut ids: Vec<String> = entries.iter().map(|t| t.id.clone()).collect();
            ids.extend(add_ids.iter().cloned());
            db::set_playlist_entries(c, id, &ids, now)
        });
        if let Err(e) = result {
            return error(0, &format!("database error: {e:#}"));
        }
    }
    if name.is_some() || comment.is_some() || public.is_some() {
        let result = with_db(state, |c| {
            db::update_playlist_meta(c, id, name.map(String::as_str), comment, public, now)
        });
        match result {
            Ok(true) => {}
            Ok(false) => return error(70, &format!("playlist not found: {id}")),
            Err(e) => return error(0, &format!("database error: {e:#}")),
        }
    }
    // `rules` present + non-empty → save and apply; present + empty → clear.
    if let Some(raw) = params.get("rules") {
        let rules = if raw.trim().is_empty() { None } else { Some(raw) };
        if let Some(raw) = rules
            && let Err(e) = radio::Rules::parse(raw)
        {
            return error(10, &format!("invalid rules: {e:#}"));
        }
        let result = with_db(state, |c| {
            db::set_playlist_rules(c, id, rules.map(String::as_str), now)?;
            if let Some(raw) = rules {
                radio::regenerate(c, id, raw, now)?;
            }
            Ok::<_, anyhow::Error>(())
        });
        if let Err(e) = result {
            return error(0, &format!("database error: {e:#}"));
        }
    }
    ok(json!({}))
}

/// Scrobble submissions write play_count/played_at; `submission=false`
/// ("now playing") pings are acknowledged but not counted.
pub fn scrobble(
    state: &AppState,
    params: &HashMap<String, String>,
    multi: &HashMap<String, Vec<String>>,
) -> Response {
    let ids: Vec<&str> = multi
        .get("id")
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return error(10, "missing required parameter: id");
    }
    let count_plays = params
        .get("submission")
        .and_then(|v| parse_bool(v))
        .unwrap_or(true);
    let played_at = params
        .get("time")
        .and_then(|v| v.parse::<i64>().ok())
        .map(|ms| ms / 1000)
        .unwrap_or_else(now_secs);
    if count_plays {
        for id in &ids {
            match with_db(state, |c| db::record_play(c, id, played_at)) {
                Ok(true) => {}
                Ok(false) => return error(70, &format!("song not found: {id}")),
                Err(e) => return error(0, &format!("database error: {e:#}")),
            }
        }
    }
    ok(json!({}))
}

pub fn delete_playlist(state: &AppState, params: &HashMap<String, String>) -> Response {
    let id = match params.get("id") {
        Some(i) => i,
        None => return error(10, "missing required parameter: id"),
    };
    let exists = match with_db(state, |c| db::playlist_by_id(c, id)) {
        Ok(p) => p,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    if exists.is_none() {
        return error(70, &format!("playlist not found: {id}"));
    }
    if let Err(e) = with_db(state, |c| db::delete_playlist(c, id)) {
        return error(0, &format!("database error: {e:#}"));
    }
    ok(json!({}))
}

fn respond_playlist(state: &AppState, id: &str) -> Response {
    let (playlist, entries) = {
        let playlist = match with_db(state, |c| db::playlist_by_id(c, id)) {
            Ok(Some(p)) => p,
            Ok(None) => return error(70, &format!("playlist not found: {id}")),
            Err(e) => return error(0, &format!("database error: {e:#}")),
        };
        let entries = match with_db(state, |c| db::playlist_entries(c, id)) {
            Ok(t) => t,
            Err(e) => return error(0, &format!("database error: {e:#}")),
        };
        (playlist, entries)
    };
    ok(json!({ "playlist": playlist_json(&playlist, Some(&entries)) }))
}

fn playlist_json(p: &db::PlaylistRow, entries: Option<&[db::TrackRow]>) -> Value {
    let mut v = json!({
        "id": p.id,
        "name": p.name,
        "comment": p.comment,
        "owner": p.owner,
        "public": p.public,
        "songCount": p.song_count,
        "duration": p.duration.round() as i64,
        "created": format_unix(p.created_at),
        "changed": format_unix(p.changed_at),
    });
    if let (Value::Object(m), Some(tracks)) = (&mut v, entries) {
        if let Some(first) = tracks.first() {
            m.insert("coverArt".to_string(), json!(first.album_id));
        }
        m.insert(
            "entry".to_string(),
            json!(tracks.iter().map(child_json).collect::<Vec<_>>()),
        );
    }
    v
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn get_album(state: &AppState, params: &HashMap<String, String>) -> Response {
    let id = match params.get("id") {
        Some(i) => i,
        None => return error(10, "missing required parameter: id"),
    };
    let (album, tracks) = {
        let album = match with_db(state, |c| db::get_album(c, id)) {
            Ok(Some(a)) => a,
            Ok(None) => return error(70, &format!("album not found: {id}")),
            Err(e) => return error(0, &format!("database error: {e:#}")),
        };
        let tracks = match with_db(state, |c| db::tracks_for_album(c, id)) {
            Ok(t) => t,
            Err(e) => return error(0, &format!("database error: {e:#}")),
        };
        (album, tracks)
    };
    let mut album_value = album_json(&album);
    if let Value::Object(ref mut map) = album_value {
        map.insert(
            "song".to_string(),
            json!(tracks.iter().map(child_json).collect::<Vec<_>>()),
        );
    }
    ok(json!({ "album": album_value }))
}

pub fn get_song(state: &AppState, params: &HashMap<String, String>) -> Response {
    let id = match params.get("id") {
        Some(i) => i,
        None => return error(10, "missing required parameter: id"),
    };
    let track = match with_db(state, |c| db::get_track(c, id)) {
        Ok(Some(t)) => t,
        Ok(None) => return error(70, &format!("song not found: {id}")),
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    ok(json!({ "song": child_json(&track) }))
}

pub fn search3(state: &AppState, params: &HashMap<String, String>) -> Response {
    let query = params
        .get("query")
        .or_else(|| params.get("any"))
        .cloned()
        .unwrap_or_default();
    if query.trim().is_empty() {
        return ok(json!({ "searchResult3": { "artist": [], "album": [], "song": [] } }));
    }
    let count = |key: &str| {
        params
            .get(key)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(20)
    };
    let offset = |key: &str| {
        params
            .get(key)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };
    let artists = match with_db(state, |c| {
        db::search_artists(c, &query, count("artistCount"), offset("artistOffset"))
    }) {
        Ok(a) => a,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    let albums = match with_db(state, |c| {
        db::search_albums(c, &query, count("albumCount"), offset("albumOffset"))
    }) {
        Ok(a) => a,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    let songs = match with_db(state, |c| {
        db::search_tracks(c, &query, count("songCount"), offset("songOffset"))
    }) {
        Ok(t) => t,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    let artist_json = |a: &db::ArtistRow| {
        json!({ "id": a.id, "name": a.name, "albumCount": a.album_count, "coverArt": a.id })
    };
    ok(json!({
        "searchResult3": {
            "artist": artists.iter().map(artist_json).collect::<Vec<_>>(),
            "album": albums.iter().map(album_json).collect::<Vec<_>>(),
            "song": songs.iter().map(child_json).collect::<Vec<_>>(),
        }
    }))
}

fn with_db<T>(
    state: &AppState,
    f: impl FnOnce(&rusqlite::Connection) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let conn = state
        .db
        .lock()
        .map_err(|e| anyhow::anyhow!("database lock poisoned: {e}"))?;
    f(&conn)
}

fn artists_buckets(state: &AppState) -> Value {
    let artists = match with_db(state, db::artists_index) {
        Ok(a) => a,
        Err(e) => return json!({ "error": format!("database error: {e:#}") }),
    };
    let mut buckets: Vec<(String, Vec<Value>)> = Vec::new();
    for a in artists {
        let entry = json!({ "id": a.id, "name": a.name, "albumCount": a.album_count });
        let letter = index_letter(&a.name);
        match buckets.iter_mut().find(|(l, _)| *l == letter) {
            Some((_, items)) => items.push(entry),
            None => buckets.push((letter, vec![entry])),
        }
    }
    let index: Vec<Value> = buckets
        .into_iter()
        .map(|(name, artist)| json!({ "name": name, "artist": artist }))
        .collect();
    json!({ "index": index })
}

fn album_json(a: &db::AlbumRow) -> Value {
    json!({
        "id": a.id,
        "name": a.name,
        "artist": a.artist_name,
        "artistId": a.artist_id,
        "coverArt": a.id,
        "songCount": a.song_count,
        "duration": a.duration.round() as i64,
        "year": a.year,
        "created": format_unix(a.added_at),
    })
}

/// Legacy directory-listing shape of an album (isDir=true child entry).
fn album_dir_json(a: &db::AlbumRow) -> Value {
    json!({
        "id": a.id,
        "parent": a.artist_id,
        "isDir": true,
        "title": a.name,
        "name": a.name,
        "album": a.name,
        "artist": a.artist_name,
        "coverArt": a.id,
        "songCount": a.song_count,
        "duration": a.duration.round() as i64,
        "year": a.year,
        "created": format_unix(a.added_at),
    })
}

fn child_json(t: &db::TrackRow) -> Value {
    json!({
        "id": t.id,
        "parent": t.album_id,
        "isDir": false,
        "title": t.title,
        "album": t.album_name,
        "artist": t.artist_name,
        "artistId": t.artist_id,
        "track": t.track_number,
        "discNumber": t.disc_number,
        "coverArt": t.album_id,
        "size": t.size,
        "contentType": t.content_type,
        "suffix": t.suffix,
        "duration": t.duration.round() as i64,
        "bitRate": t.bit_rate,
        "genre": t.genre,
        "playCount": t.play_count,
        "path": t.path,
        "type": "music",
    })
}

fn format_unix(ts: i64) -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::from_unix_timestamp(ts)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_default()
}

fn index_letter(name: &str) -> String {
    let lower = name.trim_start().to_lowercase();
    let stripped = ARTICLES
        .iter()
        .fold(lower.as_str(), |s, art| s.strip_prefix(art).unwrap_or(s));
    match stripped.chars().next() {
        Some(c) if c.is_alphabetic() => c.to_uppercase().collect(),
        _ => "#".to_string(),
    }
}

pub fn get_scan_status(state: &AppState) -> Response {
    let count = match with_db(state, db::track_count) {
        Ok(n) => n,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    let scanning = state.scanning.load(std::sync::atomic::Ordering::Relaxed);
    ok(json!({ "scanStatus": { "scanning": scanning, "count": count } }))
}

/// Manual rescan without a process restart; runs off the async runtime so
/// the event loop stays responsive. Concurrent calls collapse into the
/// in-flight scan (the flag flip acts as a cheap lock).
pub fn start_scan(state: AppState) -> Response {
    if state
        .scanning
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return get_scan_status(&state);
    }
    let result = {
        let mut conn = match state.db.lock() {
            Ok(c) => c,
            Err(e) => {
                state
                    .scanning
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                return error(0, &format!("database lock poisoned: {e}"));
            }
        };
        crate::scan::rescan(&mut conn, &state.music_folder, &state.user)
    };
    state
        .scanning
        .store(false, std::sync::atomic::Ordering::SeqCst);
    match result {
        Ok(()) => get_scan_status(&state),
        Err(e) => error(0, &format!("scan failed: {e:#}")),
    }
}

/// Personal-server honesty: without an MBID/similarity backend, "similar"
/// is random tracks, preferring the seed song's artist.
pub fn get_similar_songs2(state: &AppState, params: &HashMap<String, String>) -> Response {
    let id = params.get("id").cloned().unwrap_or_default();
    let count = params
        .get("count")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(50)
        .min(500);
    let artist = with_db(state, |c| db::get_track(c, &id))
        .ok()
        .flatten()
        .map(|t| t.artist_id);
    let songs = match with_db(state, |c| db::random_songs(c, count, artist.as_deref())) {
        Ok(s) => s,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    ok(json!({ "similarSongs2": { "song": songs.iter().map(child_json).collect::<Vec<_>>() } }))
}

/// Top = most scrobbled (play_count, freshest first as tiebreak).
pub fn get_top_songs(state: &AppState, params: &HashMap<String, String>) -> Response {
    let artist_id = match params.get("id").or_else(|| params.get("artistId")) {
        Some(i) => i.clone(),
        None => return error(10, "missing required parameter: id"),
    };
    let count = params
        .get("count")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(10)
        .min(100);
    let songs = match with_db(state, |c| db::songs_by_artist(c, &artist_id, count)) {
        Ok(s) => s,
        Err(e) => return error(0, &format!("database error: {e:#}")),
    };
    ok(json!({ "topSongs": { "song": songs.iter().map(child_json).collect::<Vec<_>>() } }))
}

pub(crate) fn ok(body: Value) -> Response {
    let mut root = json!({ "status": "ok", "version": API_VERSION });
    if let (Value::Object(map), Value::Object(extra)) = (&mut root, body) {
        map.extend(extra);
    }
    Json(json!({ "subsonic-response": root })).into_response()
}

pub fn error(code: i32, message: &str) -> Response {
    Json(json!({
        "subsonic-response": {
            "status": "failed",
            "version": API_VERSION,
            "error": { "code": code, "message": message }
        }
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn track() -> db::TrackRow {
        db::TrackRow {
            id: "track-1".into(),
            path: "/music/Artist/Album/03 Song.flac".into(),
            title: "Song".into(),
            album_id: "album-1".into(),
            album_name: "Album".into(),
            artist_id: "artist-1".into(),
            artist_name: "Artist".into(),
            track_number: Some(3),
            disc_number: Some(1),
            duration: 245.4,
            size: 18_000_000,
            suffix: "flac".into(),
            content_type: "audio/flac".into(),
            bit_rate: Some(900),
            genre: Some("Techno".into()),
            play_count: 7,
            played_at: Some(1_700_000_000),
            has_art: true,
        }
    }

    fn album() -> db::AlbumRow {
        db::AlbumRow {
            id: "album-1".into(),
            name: "Album".into(),
            artist_id: "artist-1".into(),
            artist_name: "Artist".into(),
            year: Some(1998),
            song_count: 12,
            duration: 3_300.6,
            added_at: 1_700_000_000,
        }
    }

    fn playlist() -> db::PlaylistRow {
        db::PlaylistRow {
            id: "playlist-1".into(),
            name: "Mix".into(),
            comment: "test".into(),
            owner: "admin".into(),
            public: true,
            created_at: 1_700_000_000,
            changed_at: 1_700_000_100,
            song_count: 1,
            duration: 245.4,
        }
    }

    fn snap(v: &Value) -> String {
        serde_json::to_string_pretty(v).unwrap()
    }

    // These pin the wire shapes Subsonic clients consume: a renamed field,
    // a dropped key or an int/string flip fails the snapshot.

    #[test]
    fn child_entry_shape() {
        insta::assert_snapshot!(snap(&child_json(&track())));
    }

    #[test]
    fn album_shape() {
        insta::assert_snapshot!(snap(&album_json(&album())));
    }

    #[test]
    fn legacy_album_directory_shape() {
        insta::assert_snapshot!(snap(&album_dir_json(&album())));
    }

    #[test]
    fn playlist_with_entries_shape() {
        let entries = vec![track()];
        insta::assert_snapshot!(snap(&playlist_json(&playlist(), Some(&entries))));
    }

    #[test]
    fn ok_envelope_wraps_body() {
        let resp = ok(json!({ "ping": {} }));
        let body = resp_to_json(resp);
        insta::assert_snapshot!(snap(&body));
    }

    #[test]
    fn error_envelope_shape() {
        let resp = error(70, "song not found: x");
        let body = resp_to_json(resp);
        insta::assert_snapshot!(snap(&body));
    }

    fn resp_to_json(resp: Response) -> Value {
        use http_body_util::BodyExt;
        let bytes = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async { resp.into_body().collect().await.unwrap().to_bytes() });
        serde_json::from_slice(&bytes).unwrap()
    }
}
