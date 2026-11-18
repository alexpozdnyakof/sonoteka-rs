use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS artists (
            id   TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS albums (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            artist_id  TEXT NOT NULL REFERENCES artists(id),
            year       INTEGER,
            added_at   INTEGER NOT NULL DEFAULT 0,
            song_count INTEGER NOT NULL DEFAULT 0,
            duration   REAL NOT NULL DEFAULT 0,
            UNIQUE (name, artist_id)
        );

        CREATE TABLE IF NOT EXISTS tracks (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL UNIQUE,
            title        TEXT NOT NULL,
            album_id     TEXT NOT NULL REFERENCES albums(id),
            artist_id    TEXT NOT NULL REFERENCES artists(id),
            track_number INTEGER,
            disc_number  INTEGER,
            duration     REAL NOT NULL DEFAULT 0,
            size         INTEGER NOT NULL DEFAULT 0,
            suffix       TEXT NOT NULL,
            content_type TEXT NOT NULL,
            bit_rate     INTEGER,
            sample_rate  INTEGER,
            genre        TEXT,
            play_count   INTEGER NOT NULL DEFAULT 0,
            played_at    INTEGER,
            has_art      INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS playlists (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            comment    TEXT NOT NULL DEFAULT '',
            owner      TEXT NOT NULL,
            public     INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0,
            changed_at INTEGER NOT NULL DEFAULT 0,
            rules      TEXT
        );

        CREATE TABLE IF NOT EXISTS playlist_tracks (
            playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            track_id    TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            position    INTEGER NOT NULL,
            PRIMARY KEY (playlist_id, position)
        );

        CREATE INDEX IF NOT EXISTS idx_tracks_album  ON tracks(album_id);
        CREATE INDEX IF NOT EXISTS idx_albums_artist ON albums(artist_id);
        ",
    )?;
    migrate(conn)?;
    Ok(())
}

/// Additive migrations for databases created by older builds: tracks.genre
/// (genre feature) and playlists.created_at/changed_at (playlist feature).
fn migrate(conn: &Connection) -> Result<()> {
    let track_cols = table_columns(conn, "tracks")?;
    if !track_cols.iter().any(|c| c == "genre") {
        conn.execute_batch("ALTER TABLE tracks ADD COLUMN genre TEXT;")?;
    }
    let playlist_cols = table_columns(conn, "playlists")?;
    if !playlist_cols.iter().any(|c| c == "created_at") {
        conn.execute_batch("ALTER TABLE playlists ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;")?;
    }
    if !playlist_cols.iter().any(|c| c == "changed_at") {
        conn.execute_batch("ALTER TABLE playlists ADD COLUMN changed_at INTEGER NOT NULL DEFAULT 0;")?;
    }
    if !playlist_cols.iter().any(|c| c == "rules") {
        conn.execute_batch("ALTER TABLE playlists ADD COLUMN rules TEXT;")?;
    }
    let track_cols = table_columns(conn, "tracks")?;
    if !track_cols.iter().any(|c| c == "play_count") {
        conn.execute_batch("ALTER TABLE tracks ADD COLUMN play_count INTEGER NOT NULL DEFAULT 0;")?;
    }
    if !track_cols.iter().any(|c| c == "played_at") {
        conn.execute_batch("ALTER TABLE tracks ADD COLUMN played_at INTEGER;")?;
    }
    Ok(())
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let sql = format!("SELECT name FROM pragma_table_info('{table}')");
    Ok(conn
        .prepare(&sql)?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Full rescan strategy: the library is small and local, so we rebuild
/// artists/albums/tracks from scratch every time instead of doing
/// incremental mtime-based bookkeeping.
pub fn wipe_library(conn: &Connection) -> Result<()> {
    conn.execute_batch("DELETE FROM tracks; DELETE FROM albums; DELETE FROM artists;")?;
    Ok(())
}

pub fn upsert_artist(conn: &Connection, name: &str) -> Result<String> {
    let id = stable_id(&format!("artist:{name}"));
    conn.execute(
        "INSERT OR IGNORE INTO artists (id, name) VALUES (?1, ?2)",
        params![id, name],
    )?;
    Ok(id)
}

pub fn upsert_album(
    conn: &Connection,
    name: &str,
    artist_id: &str,
    year: Option<u32>,
    added_at: i64,
) -> Result<String> {
    let id = stable_id(&format!("album:{artist_id}:{name}"));
    conn.execute(
        "INSERT INTO albums (id, name, artist_id, year, added_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
            year     = COALESCE(albums.year, excluded.year),
            added_at = MIN(albums.added_at, excluded.added_at)",
        params![id, name, artist_id, year.map(|y| y as i64), added_at],
    )?;
    Ok(id)
}

pub struct TrackInsert<'a> {
    pub path: &'a str,
    pub title: &'a str,
    pub album_id: &'a str,
    pub artist_id: &'a str,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub duration_secs: f64,
    pub size_bytes: i64,
    pub suffix: &'a str,
    pub content_type: &'a str,
    pub bit_rate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub genre: Option<String>,
    pub has_art: bool,
}

pub fn insert_track(conn: &Connection, t: &TrackInsert) -> Result<()> {
    let id = stable_id(&format!("track:{}", t.path));
    conn.execute(
        "INSERT OR REPLACE INTO tracks
         (id, path, title, album_id, artist_id, track_number, disc_number, duration, size,
          suffix, content_type, bit_rate, sample_rate, genre, has_art)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            id,
            t.path,
            t.title,
            t.album_id,
            t.artist_id,
            t.track_number.map(|v| v as i64),
            t.disc_number.map(|v| v as i64),
            t.duration_secs,
            t.size_bytes,
            t.suffix,
            t.content_type,
            t.bit_rate.map(|v| v as i64),
            t.sample_rate.map(|v| v as i64),
            t.genre,
            t.has_art as i64,
        ],
    )?;
    Ok(())
}

pub fn recompute_album_stats(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r"UPDATE albums SET
            song_count = (SELECT COUNT(*) FROM tracks WHERE tracks.album_id = albums.id),
            duration   = (SELECT COALESCE(SUM(duration), 0) FROM tracks WHERE tracks.album_id = albums.id);",
    )?;
    Ok(())
}

pub struct ArtistRow {
    pub id: String,
    pub name: String,
    pub album_count: i64,
}

pub fn artists_index(conn: &Connection) -> Result<Vec<ArtistRow>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.name,
                (SELECT COUNT(*) FROM albums al WHERE al.artist_id = a.id) AS album_count
         FROM artists a
         ORDER BY a.name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ArtistRow {
                id: row.get(0)?,
                name: row.get(1)?,
                album_count: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub struct AlbumRow {
    pub id: String,
    pub name: String,
    pub artist_id: String,
    pub artist_name: String,
    pub year: Option<i64>,
    pub song_count: i64,
    pub duration: f64,
    pub added_at: i64,
}

const ALBUM_SELECT: &str =
    "SELECT al.id, al.name, al.artist_id, ar.name, al.year, al.song_count, al.duration, al.added_at
     FROM albums al JOIN artists ar ON ar.id = al.artist_id";

fn album_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AlbumRow> {
    Ok(AlbumRow {
        id: row.get(0)?,
        name: row.get(1)?,
        artist_id: row.get(2)?,
        artist_name: row.get(3)?,
        year: row.get(4)?,
        song_count: row.get(5)?,
        duration: row.get(6)?,
        added_at: row.get(7)?,
    })
}

pub enum AlbumSort {
    Newest,
    Random,
    ByArtist,
    ByName,
}

pub fn list_albums(
    conn: &Connection,
    sort: AlbumSort,
    size: i64,
    offset: i64,
) -> Result<Vec<AlbumRow>> {
    let order = match sort {
        AlbumSort::Newest => "al.added_at DESC, al.name COLLATE NOCASE",
        AlbumSort::Random => "RANDOM()",
        AlbumSort::ByArtist => "ar.name COLLATE NOCASE, al.name COLLATE NOCASE",
        AlbumSort::ByName => "al.name COLLATE NOCASE",
    };
    let sql = format!("{ALBUM_SELECT} ORDER BY {order} LIMIT ?1 OFFSET ?2");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![size, offset], album_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_album(conn: &Connection, id: &str) -> Result<Option<AlbumRow>> {
    let sql = format!("{ALBUM_SELECT} WHERE al.id = ?1");
    Ok(conn.prepare(&sql)?.query_row(params![id], album_from_row).optional()?)
}

pub struct TrackRow {
    pub id: String,
    pub path: String,
    pub title: String,
    pub album_id: String,
    pub album_name: String,
    pub artist_id: String,
    pub artist_name: String,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub duration: f64,
    pub size: i64,
    pub suffix: String,
    pub content_type: String,
    pub bit_rate: Option<i64>,
    pub genre: Option<String>,
    pub play_count: i64,
    /// Selected for SQL-side ordering (top songs, anti-repeat); no wire field.
    #[allow(dead_code)]
    pub played_at: Option<i64>,
    pub has_art: bool,
}

pub(crate) const TRACK_COLS: &str =
    "t.id, t.path, t.title, t.album_id, al.name, t.artist_id, ar.name,
     t.track_number, t.disc_number, t.duration, t.size, t.suffix,
     t.content_type, t.bit_rate, t.genre, t.play_count, t.played_at, t.has_art";

const TRACK_SELECT: &str =
    "SELECT t.id, t.path, t.title, t.album_id, al.name, t.artist_id, ar.name,
            t.track_number, t.disc_number, t.duration, t.size, t.suffix,
            t.content_type, t.bit_rate, t.genre, t.play_count, t.played_at, t.has_art
     FROM tracks t
     JOIN albums al ON al.id = t.album_id
     JOIN artists ar ON ar.id = t.artist_id";

pub(crate) fn track_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackRow> {
    Ok(TrackRow {
        id: row.get(0)?,
        path: row.get(1)?,
        title: row.get(2)?,
        album_id: row.get(3)?,
        album_name: row.get(4)?,
        artist_id: row.get(5)?,
        artist_name: row.get(6)?,
        track_number: row.get(7)?,
        disc_number: row.get(8)?,
        duration: row.get(9)?,
        size: row.get(10)?,
        suffix: row.get(11)?,
        content_type: row.get(12)?,
        bit_rate: row.get(13)?,
        genre: row.get(14)?,
        play_count: row.get(15)?,
        played_at: row.get(16)?,
        has_art: row.get::<_, i64>(17)? != 0,
    })
}

pub fn get_track(conn: &Connection, id: &str) -> Result<Option<TrackRow>> {
    let sql = format!("{TRACK_SELECT} WHERE t.id = ?1");
    Ok(conn.prepare(&sql)?.query_row(params![id], track_from_row).optional()?)
}

pub fn tracks_for_album(conn: &Connection, album_id: &str) -> Result<Vec<TrackRow>> {
    let sql = format!(
        "{TRACK_SELECT} WHERE t.album_id = ?1
         ORDER BY COALESCE(t.disc_number, 1), COALESCE(t.track_number, 0), t.path"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![album_id], track_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Path of a track in the album that carries embedded artwork — the source
/// `getCoverArt` extracts from.
pub fn album_art_source(conn: &Connection, album_id: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare(
            "SELECT path FROM tracks
             WHERE album_id = ?1 AND has_art = 1
             ORDER BY COALESCE(disc_number, 1), COALESCE(track_number, 0)
             LIMIT 1",
        )?
        .query_row(params![album_id], |row| row.get(0))
        .optional()?)
}

pub fn get_artist(conn: &Connection, id: &str) -> Result<Option<ArtistRow>> {
    Ok(conn
        .prepare(
            "SELECT a.id, a.name,
                    (SELECT COUNT(*) FROM albums al WHERE al.artist_id = a.id) AS album_count
             FROM artists a WHERE a.id = ?1",
        )?
        .query_row(params![id], |row| {
            Ok(ArtistRow {
                id: row.get(0)?,
                name: row.get(1)?,
                album_count: row.get(2)?,
            })
        })
        .optional()?)
}

pub fn albums_for_artist(conn: &Connection, artist_id: &str) -> Result<Vec<AlbumRow>> {
    let sql = format!("{ALBUM_SELECT} WHERE al.artist_id = ?1 ORDER BY al.name COLLATE NOCASE");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![artist_id], album_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn search_artists(
    conn: &Connection,
    query: &str,
    size: i64,
    offset: i64,
) -> Result<Vec<ArtistRow>> {
    let like = format!("%{query}%");
    let mut stmt = conn.prepare(
        "SELECT a.id, a.name,
                (SELECT COUNT(*) FROM albums al WHERE al.artist_id = a.id) AS album_count
         FROM artists a
         WHERE a.name LIKE ?1 
         ORDER BY a.name COLLATE NOCASE
         LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt
        .query_map(params![like, size, offset], |row| {
            Ok(ArtistRow {
                id: row.get(0)?,
                name: row.get(1)?,
                album_count: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn search_albums(conn: &Connection, query: &str, size: i64, offset: i64) -> Result<Vec<AlbumRow>> {
    let like = format!("%{query}%");
    let sql = format!(
        "{ALBUM_SELECT}
         WHERE al.name LIKE ?1  OR ar.name LIKE ?1 
         ORDER BY al.name COLLATE NOCASE
         LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![like, size, offset], album_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn search_tracks(conn: &Connection, query: &str, size: i64, offset: i64) -> Result<Vec<TrackRow>> {
    let like = format!("%{query}%");
    let sql = format!(
        "{TRACK_SELECT}
         WHERE t.title LIKE ?1  OR al.name LIKE ?1  OR ar.name LIKE ?1 
         ORDER BY ar.name COLLATE NOCASE, al.name COLLATE NOCASE, t.path
         LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![like, size, offset], track_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn track_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))?)
}

/// "Top" = most played, ties broken by most recently played. Tracks never
/// scrobbled keep play_count 0 and sink below everything played.
pub fn songs_by_artist(conn: &Connection, artist_id: &str, limit: i64) -> Result<Vec<TrackRow>> {
    let sql = format!(
        "{TRACK_SELECT} WHERE t.artist_id = ?1
         ORDER BY t.play_count DESC, COALESCE(t.played_at, 0) DESC,
                  al.name COLLATE NOCASE, t.path LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![artist_id, limit], track_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Scrobble bookkeeping: bump the counter, stamp the last-played time.
/// Returns false when the id is not a known track.
pub fn record_play(conn: &Connection, id: &str, played_at: i64) -> Result<bool> {
    let affected = conn.execute(
        "UPDATE tracks SET play_count = play_count + 1, played_at = ?2 WHERE id = ?1",
        params![id, played_at],
    )?;
    Ok(affected > 0)
}

pub struct GenreRow {
    pub name: String,
    pub song_count: i64,
    pub album_count: i64,
}

/// Genres with per-genre track/album tallies, most-played-ish order (by size).
pub fn list_genres(conn: &Connection) -> Result<Vec<GenreRow>> {
    let mut stmt = conn.prepare(
        "SELECT genre,
                COUNT(*) AS song_count,
                COUNT(DISTINCT album_id) AS album_count
         FROM tracks
         WHERE genre IS NOT NULL AND genre != ''
         GROUP BY genre
         ORDER BY song_count DESC, genre COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(GenreRow {
                name: row.get(0)?,
                song_count: row.get(1)?,
                album_count: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn songs_by_genre(
    conn: &Connection,
    genre: &str,
    size: i64,
    offset: i64,
) -> Result<Vec<TrackRow>> {
    let sql = format!(
        "{TRACK_SELECT} WHERE t.genre = ?1
         ORDER BY ar.name COLLATE NOCASE, al.name COLLATE NOCASE, t.path
         LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![genre, size, offset], track_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Albums that have at least one track of the genre.
pub fn albums_by_genre(
    conn: &Connection,
    genre: &str,
    size: i64,
    offset: i64,
) -> Result<Vec<AlbumRow>> {
    let sql = format!(
        "{ALBUM_SELECT}
         WHERE EXISTS (SELECT 1 FROM tracks t
                       WHERE t.album_id = al.id AND t.genre = ?1)
         ORDER BY ar.name COLLATE NOCASE, al.name COLLATE NOCASE
         LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![genre, size, offset], album_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn random_songs(
    conn: &Connection,
    limit: i64,
    artist_id: Option<&str>,
) -> Result<Vec<TrackRow>> {
    let rows = match artist_id {
        Some(aid) => {
            let sql = format!("{TRACK_SELECT} WHERE t.artist_id = ?1 ORDER BY RANDOM() LIMIT ?2");
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map(params![aid, limit], track_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
        None => {
            let sql = format!("{TRACK_SELECT} ORDER BY RANDOM() LIMIT ?1");
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map(params![limit], track_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
    };
    Ok(rows)
}

/// Stable string IDs (md5 of a domain-specific key), same approach as Navidrome.
pub fn stable_id(key: &str) -> String {
    use md5::{Digest, Md5};
    hex::encode(Md5::digest(key.as_bytes()))
}

pub struct PlaylistRow {
    pub id: String,
    pub name: String,
    pub comment: String,
    pub owner: String,
    pub public: bool,
    pub created_at: i64,
    pub changed_at: i64,
    pub song_count: i64,
    pub duration: f64,
}

const PLAYLIST_SELECT: &str =
    "SELECT p.id, p.name, p.comment, p.owner, p.public, p.created_at, p.changed_at,
            COUNT(pt.track_id) AS song_count,
            COALESCE(SUM(t.duration), 0) AS duration
     FROM playlists p
     LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
     LEFT JOIN tracks t ON t.id = pt.track_id";

fn playlist_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlaylistRow> {
    Ok(PlaylistRow {
        id: row.get(0)?,
        name: row.get(1)?,
        comment: row.get(2)?,
        owner: row.get(3)?,
        public: row.get::<_, i64>(4)? != 0,
        created_at: row.get(5)?,
        changed_at: row.get(6)?,
        song_count: row.get(7)?,
        duration: row.get(8)?,
    })
}

pub fn list_playlists(conn: &Connection) -> Result<Vec<PlaylistRow>> {
    let sql = format!("{PLAYLIST_SELECT} GROUP BY p.id ORDER BY p.name COLLATE NOCASE");
    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt
        .query_map([], playlist_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn playlist_by_id(conn: &Connection, id: &str) -> Result<Option<PlaylistRow>> {
    let sql = format!("{PLAYLIST_SELECT} WHERE p.id = ?1 GROUP BY p.id");
    Ok(conn
        .prepare(&sql)?
        .query_row(params![id], playlist_from_row)
        .optional()?)
}

pub fn create_playlist(
    conn: &Connection,
    name: &str,
    owner: &str,
    now: i64,
) -> Result<PlaylistRow> {
    let id = stable_id(&format!("playlist:{name}:{now}"));
    conn.execute(
        "INSERT INTO playlists (id, name, owner, created_at, changed_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![id, name, owner, now],
    )?;
    Ok(playlist_by_id(conn, &id)?.expect("playlist just inserted"))
}

/// Applies whichever of name/comment/public is provided; None leaves the
/// field untouched. Bumps changed_at.
pub fn update_playlist_meta(
    conn: &Connection,
    id: &str,
    name: Option<&str>,
    comment: Option<&str>,
    public: Option<bool>,
    now: i64,
) -> Result<bool> {
    let affected = conn.execute(
        "UPDATE playlists SET
            name      = COALESCE(?2, name),
            comment   = COALESCE(?3, comment),
            public    = COALESCE(?4, public),
            changed_at = ?5
         WHERE id = ?1",
        params![
            id,
            name,
            comment,
            public.map(|p| p as i64),
            now,
        ],
    )?;
    Ok(affected > 0)
}

/// Full replacement of a playlist's contents (dense positions from 0).
pub fn set_playlist_entries(
    conn: &Connection,
    playlist_id: &str,
    track_ids: &[String],
    now: i64,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM playlist_tracks WHERE playlist_id = ?1",
        params![playlist_id],
    )?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position)
             VALUES (?1, ?2, ?3)",
        )?;
        for (pos, track_id) in track_ids.iter().enumerate() {
            stmt.execute(params![playlist_id, track_id, pos as i64])?;
        }
    }
    tx.execute(
        "UPDATE playlists SET changed_at = ?2 WHERE id = ?1",
        params![playlist_id, now],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn delete_playlist(conn: &Connection, id: &str) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM playlist_tracks WHERE playlist_id = ?1",
        params![id],
    )?;
    tx.execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
    tx.commit()?;
    Ok(())
}

pub fn playlist_entries(conn: &Connection, playlist_id: &str) -> Result<Vec<TrackRow>> {
    let sql = format!(
        "SELECT {TRACK_COLS}
         FROM playlist_tracks pt
         JOIN tracks t ON t.id = pt.track_id
         JOIN albums al ON al.id = t.album_id
         JOIN artists ar ON ar.id = t.artist_id
         WHERE pt.playlist_id = ?1
         ORDER BY pt.position"
    );
    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt
        .query_map(params![playlist_id], track_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Drops playlist entries pointing at tracks that no longer exist (removed
/// files stay removed; rescan re-inserts the same stable ids, so surviving
/// entries keep working).
pub fn prune_orphan_playlist_tracks(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM playlist_tracks WHERE track_id NOT IN (SELECT id FROM tracks);",
    )?;
    Ok(())
}

/// Input ids in order, minus the ones that are not in the library.
pub fn existing_track_ids(conn: &Connection, ids: &[String]) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT id FROM tracks WHERE id IN ({placeholders})");
    let found: std::collections::HashSet<String> = conn
        .prepare(&sql)?
        .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(ids
        .iter()
        .filter(|id| found.contains(*id))
        .cloned()
        .collect())
}

/// (path, track id) pairs for the paths present in the library — the m3u
/// importer resolves entries through this.
pub fn track_ids_for_paths(conn: &Connection, paths: &[String]) -> Result<Vec<(String, String)>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", paths.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT path, id FROM tracks WHERE path IN ({placeholders})");
    Ok(conn
        .prepare(&sql)?
        .query_map(rusqlite::params_from_iter(paths.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Rules for smart playlists (JSON, see radio::Rules). Empty string clears.
pub fn set_playlist_rules(
    conn: &Connection,
    id: &str,
    rules: Option<&str>,
    now: i64,
) -> Result<bool> {
    let affected = conn.execute(
        "UPDATE playlists SET rules = ?2, changed_at = ?3 WHERE id = ?1",
        params![id, rules, now],
    )?;
    Ok(affected > 0)
}

/// (id, raw rules) for every playlist carrying rules — the regen sweep.
pub fn smart_playlists(conn: &Connection) -> Result<Vec<(String, String)>> {
    Ok(conn
        .prepare("SELECT id, rules FROM playlists WHERE rules IS NOT NULL AND rules != ''")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Idempotent insert for imported playlists: created once, name refreshed on
/// re-import, owner/comment kept as-is.
pub fn upsert_imported_playlist(
    conn: &Connection,
    id: &str,
    name: &str,
    owner: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO playlists (id, name, owner, created_at, changed_at)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET
            name       = excluded.name,
            changed_at = excluded.changed_at",
        params![id, name, owner, now],
    )?;
    Ok(())
}
