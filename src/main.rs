mod api;
mod db;
mod radio;
mod scan;
mod stream;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::Connection;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub cache_dir: PathBuf,
    pub music_folder: PathBuf,
    pub user: String,
    pub password: String,
    pub scanning: Arc<std::sync::atomic::AtomicBool>,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let music_folder = PathBuf::from(env_or("SONOTEKA_MUSIC_FOLDER", "./music"));
    let data_folder = PathBuf::from(env_or("SONOTEKA_DATA_FOLDER", "./data"));
    let address = env_or("SONOTEKA_ADDRESS", "0.0.0.0");
    let port: u16 = env_or("SONOTEKA_PORT", "4533")
        .parse()
        .context("SONOTEKA_PORT must be a number")?;
    let user = env_or("SONOTEKA_USER", "admin");
    let password = env_or("SONOTEKA_PASSWORD", "changeme");
    if password == "changeme" {
        eprintln!("warning: SONOTEKA_PASSWORD is not set, using 'changeme'");
    }

    std::fs::create_dir_all(&data_folder)?;
    let cache_dir = data_folder.join("cache");
    std::fs::create_dir_all(&cache_dir)?;
    let db_path = data_folder.join("sonoteka.db");
    let mut conn =
        Connection::open(&db_path).with_context(|| format!("open database {db_path:?}"))?;
    db::init_schema(&conn)?;
    scan::rescan(&mut conn, &music_folder, &user)?;

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        cache_dir,
        music_folder: music_folder.clone(),
        user,
        password,
        scanning: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let addr: SocketAddr = format!("{address}:{port}")
        .parse()
        .with_context(|| "invalid SONOTEKA_ADDRESS or SONOTEKA_PORT")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    eprintln!("sonoteka-rs listening on http://{addr} (user: {})", state.user);
    axum::serve(listener, api::router(state)).await?;
    Ok(())
}
