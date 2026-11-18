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

use anyhow::{Context, Result};
use rusqlite::{Connection, ToSql};
use serde_json::Value;

use crate::db;

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
