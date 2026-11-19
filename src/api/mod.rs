pub mod subsonic;

use std::collections::HashMap;

use axum::{
    Router,
    extract::{Form, FromRequest, Path, Query, Request, State},
    http::HeaderMap,
    response::Response,
    routing::get,
};

use crate::AppState;

/// axum 0.8 has no `Option<Form<T>>`, so the form body is extracted through
/// this lenient wrapper: only urlencoded POSTs are parsed, anything else
/// (GETs, other content types) yields an empty pair list.
struct MaybeForm(Vec<(String, String)>);

impl<S> FromRequest<S> for MaybeForm
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let is_form = req
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.starts_with("application/x-www-form-urlencoded"))
            .unwrap_or(false);
        if !is_form {
            return Ok(MaybeForm(Vec::new()));
        }
        match Form::<Vec<(String, String)>>::from_request(req, state).await {
            Ok(Form(pairs)) => Ok(MaybeForm(pairs)),
            Err(_) => Ok(MaybeForm(Vec::new())),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        // Playlist mutations arrive as POST on some clients; params travel in
        // the query string either way (an optional form body is merged too).
        .route("/rest/{*method}", get(dispatch).post(dispatch))
        .route("/radio", get(crate::radio::stream_handler))
        .with_state(state)
}

async fn dispatch(
    State(state): State<AppState>,
    Path(method): Path<String>,
    Query(query_pairs): Query<Vec<(String, String)>>,
    headers: HeaderMap,
    MaybeForm(form_pairs): MaybeForm,
) -> Response {
    let mut pairs = query_pairs;
    pairs.extend(form_pairs);
    // Last value wins for scalar lookup; repeated params (songId, songIdToAdd,
    // songIndexToRemove) are preserved in order under `multi`.
    let mut params: HashMap<String, String> = HashMap::new();
    let mut multi: HashMap<String, Vec<String>> = HashMap::new();
    for (k, v) in pairs {
        params.insert(k.clone(), v.clone());
        multi.entry(k).or_default().push(v);
    }
    let method = method.trim_end_matches(".view").to_string();
    // Feishin probes OpenSubsonic extensions without credentials; the answer
    // (none) is not a secret, so it goes before the auth check.
    if method == "getOpenSubsonicExtensions" {
        return subsonic::ok(json!({ "openSubsonicExtensions": [] }));
    }
    if let Err(resp) = subsonic::check_auth(&state, &params) {
        eprintln!("[req] {method} -> auth failed");
        return resp;
    }
    eprintln!("[req] {method}");
    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok());
    match method.as_str() {
        "ping" => subsonic::ping(),
        "getArtists" => subsonic::get_artists(&state),
        "getIndexes" => subsonic::get_indexes(&state),
        "getMusicDirectory" => subsonic::get_music_directory(&state, &params),
        "getArtist" => subsonic::get_artist(&state, &params),
        "getAlbumList2" => subsonic::get_album_list2(&state, &params),
        "getAlbumList" => subsonic::get_album_list(&state, &params),
        "getAlbum" => subsonic::get_album(&state, &params),
        "getSong" => subsonic::get_song(&state, &params),
        "search3" => subsonic::search3(&state, &params),
        "getScanStatus" => subsonic::get_scan_status(&state),
        "scan" | "startScan" => {
            let st = state.clone();
            match tokio::task::spawn_blocking(move || subsonic::start_scan(st)).await {
                Ok(resp) => resp,
                Err(e) => subsonic::error(0, &format!("scan task failed to run: {e}")),
            }
        }
        "getSimilarSongs2" => subsonic::get_similar_songs2(&state, &params),
        "getTopSongs" => subsonic::get_top_songs(&state, &params),
        "getStarred" | "getStarred2" => {
            subsonic::ok(json!({ "starred2": { "artist": [], "album": [], "song": [] } }))
        }
        "star" | "unstar" => subsonic::ok(json!({})),
        "getGenres" => subsonic::get_genres(&state),
        "getSongsByGenre" => subsonic::get_songs_by_genre(&state, &params),
        "getPlaylists" => subsonic::get_playlists(&state),
        "getPlaylist" => subsonic::get_playlist(&state, &params),
        "createPlaylist" => subsonic::create_playlist(&state, &params, &multi),
        "updatePlaylist" => subsonic::update_playlist(&state, &params, &multi),
        "deletePlaylist" => subsonic::delete_playlist(&state, &params),
        "getMusicFolders" => subsonic::ok(json!({
            "musicFolders": { "musicFolder": [{ "id": 0, "name": "Music" }] }
        })),
        "getArtistInfo" | "getArtistInfo2" => subsonic::ok(json!({
            "artistInfo2": { "biography": "", "musicBrainzId": "", "lastFmUrl": "", "album": [] }
        })),
        "getCoverArt" => crate::stream::cover_art(state, &params).await,
        "stream" | "download" => {
            crate::stream::stream(state, &params, range).await
        }
        "scrobble" => subsonic::scrobble(&state, &params, &multi),
        other => {
            eprintln!("[req] {other} -> not implemented");
            subsonic::error(70, &format!("method not implemented: {other}"))
        }
    }
}

use serde_json::json;
