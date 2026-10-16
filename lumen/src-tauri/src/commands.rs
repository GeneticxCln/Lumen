use crate::db::*;
use crate::embed;
use crate::error::{PlayerError, Result};
use crate::extract::{self, ExtractorStatus};
use crate::harvest::{self, HarvestResponse};
use crate::media_tools::{cache_name, prune_cache, run_with_timeout};
use crate::playback::mpv_ipc::MpvIpcBackend;
use crate::playback::{PlaybackBackend, PlayerStateEvent};
use crate::player_prefs;
use crate::playlist::import::{Channel, Playlist, import_m3u_file, import_m3u_url};
use crate::probe::{self, ProbeResponse, ProbeResult, ProbeTarget};
use crate::search::{self, SearchRequest, SearchResponse};
use crate::secrets;
use crate::tmdb;
use base64::Engine;
use chrono::Utc;
use serde::Serialize;
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

// Global playback backend
//
// The cell is locked only long enough to read or replace the handle. Handlers clone the `Arc`
// and release the lock before talking to mpv, so a slow `load` can never block Stop or Pause.
type SharedBackend = Arc<dyn PlaybackBackend>;
type BackendCell = Arc<Mutex<Option<SharedBackend>>>;
static PLAYBACK_BACKEND: OnceLock<BackendCell> = OnceLock::new();
/// Counts started plays, so a subtitle that is still being fetched for one file is dropped if
/// another file has been started since.
static PLAY_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub async fn get_playback_backend() -> Result<BackendCell> {
    let backend = PLAYBACK_BACKEND.get_or_init(|| Arc::new(Mutex::new(None)));
    Ok(backend.clone())
}

pub(crate) async fn ensure_db(app: &AppHandle) -> Result<&'static sqlx::Pool<sqlx::Sqlite>> {
    if let Some(pool) = get_pool() {
        return Ok(pool);
    }
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| PlayerError::Io(e.to_string()))?;
    let _ = std::fs::create_dir_all(&app_dir);
    let _ = init_db(&app_dir).await?;
    get_pool().ok_or_else(|| PlayerError::Sql("Database pool not initialized".into()))
}

/// The running backend, or an error when nothing is playing. Holds the lock only to clone.
pub(crate) async fn current_backend() -> Result<SharedBackend> {
    let cell = get_playback_backend().await?;
    let guard = cell.lock().await;
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| PlayerError::Mpv("No active playback".into()))
}

#[tauri::command]
pub async fn play_url(
    url: String,
    headers: Option<serde_json::Value>,
    channel_id: Option<i64>,
    volume: Option<i64>,
    audio_url: Option<String>,
    app: AppHandle,
) -> Result<()> {
    let result = play_url_inner(url, headers, channel_id, volume, audio_url, app).await;
    // The page shows the error to the user; this line is for anyone reading the log afterwards.
    if let Err(error) = &result {
        eprintln!("lumen: could not start playback: {error}");
    }
    result
}

async fn play_url_inner(
    url: String,
    headers: Option<serde_json::Value>,
    channel_id: Option<i64>,
    volume: Option<i64>,
    audio_url: Option<String>,
    app: AppHandle,
) -> Result<()> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    let hwdec = get_setting(pool, "hwdec")
        .await?
        .unwrap_or_else(|| "auto-safe".to_string());
    let cache = get_setting(pool, "cache")
        .await?
        .unwrap_or_else(|| "yes".to_string());
    let extra_args = get_setting(pool, "extra_mpv_args")
        .await?
        .unwrap_or_default();

    let backend = {
        let backend_cell = get_playback_backend().await?;
        let mut guard = backend_cell.lock().await;
        let running = guard.as_ref().is_some_and(|backend| backend.is_alive());
        if !running {
            // Playing inside the window (experimental): on X11 mpv draws into a native area above
            // the page; on native Wayland libmpv runs here and draws into an OpenGL area. If that
            // cannot be set up, the separate mpv window is used instead.
            let mut args = extra_args.clone();
            embed::set_in_use(false);
            let mut in_window = None;
            if matches!(get_setting(pool, EMBED_PLAYER_SETTING).await, Ok(Some(value)) if value == "true")
            {
                match embed::mode() {
                    Some(embed::Mode::Wid) => match embed::ensure_area(&app).await {
                        Ok(xid) => {
                            // Last, so a user's own arguments cannot point mpv away from the area.
                            args = format!("{args} {}", embed::mpv_args(xid));
                            embed::set_in_use(true);
                        }
                        Err(reason) => eprintln!(
                            "lumen: playing in the window is not available, using the mpv window: {reason}"
                        ),
                    },
                    Some(embed::Mode::Gl) => {
                        match embed::start_gl_engine(&app, &hwdec, &cache, &extra_args).await {
                            Ok(started) => {
                                in_window = Some(started);
                                embed::set_in_use(true);
                            }
                            Err(reason) => eprintln!(
                                "lumen: playing in the window is not available, using the mpv window: {reason}"
                            ),
                        }
                    }
                    None => {}
                }
            }
            *guard = Some(Arc::new(match in_window {
                Some((socket_path, engine)) => {
                    MpvIpcBackend::connect(
                        Some(app.clone()),
                        socket_path,
                        crate::playback::mpv_ipc::Engine::Embedded(engine),
                    )
                    .await?
                }
                None => MpvIpcBackend::new(Some(app.clone()), &hwdec, &cache, &args).await?,
            }));
        }
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| PlayerError::Mpv("mpv did not start".into()))?
    };

    // An entry saved from a page (extract) has no stable stream link: resolve it now. The history
    // and resume identity stays the entry's own URL, which does not change between plays.
    let mut headers = headers;
    let mut audio_url = audio_url;
    let mut site_subtitles: Vec<extract::ExtractedSubtitle> = Vec::new();
    let mut load_url = url.clone();
    if let Some(page) = match channel_id {
        Some(id) => extract_origin_for_channel(pool, id).await?,
        None => None,
    } {
        let allow_lan = allow_lan_sources(pool).await;
        let resolved =
            extract::resolve_stream(&page, app.path().app_data_dir().ok(), allow_lan).await?;
        load_url = resolved.url;
        audio_url = resolved.audio_url;
        site_subtitles = resolved.subtitles;
        headers = Some(serde_json::to_value(resolved.headers)?);
    }

    let hdrs = headers.as_ref().and_then(|v| {
        v.as_object().map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s)))
                .collect::<Vec<_>>()
        })
    });

    // This profile's audio and subtitle preferences, and a fresh subtitle delay for the new file.
    let prefs = player_prefs::load(pool, profile_id)
        .await
        .unwrap_or_default();
    backend.apply_prefs(&prefs, true).await;

    // Volume goes in before the file does, so playback never starts at mpv's default level.
    if let Some(volume) = volume {
        let _ = backend.set_volume(volume).await;
    }
    backend
        .load(&load_url, hdrs.as_deref(), audio_url.as_deref())
        .await?;

    // Check if there is a saved resume position for local files
    if let Ok(Some(resume_pos)) = get_resume_position(pool, profile_id, &url).await
        && resume_pos > 1.0
    {
        let _ = backend.seek(resume_pos).await;
    }

    // A subtitle the user picked for this title before, or one the site offers in their language.
    // It is fetched in the background: playback never waits for it.
    let generation = PLAY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    spawn_subtitle_autoload(
        app.clone(),
        backend.clone(),
        lumen_core::subtitles::service::PlayingItem {
            channel_id,
            source: url.clone(),
            name: None,
        },
        prefs.subtitles.clone(),
        site_subtitles,
        generation,
    );

    // Record history entry for the active profile
    let _ = add_history(pool, profile_id, channel_id, &url, 0.0, 0.0).await;
    if let Some(cid) = channel_id {
        let _ = update_channel_last_played(pool, cid, true, None).await;
    }

    Ok(())
}

#[tauri::command]
pub async fn pause() -> Result<()> {
    let backend = current_backend().await?;
    backend.pause().await
}

#[tauri::command]
pub async fn seek(seconds: f64) -> Result<()> {
    let backend = current_backend().await?;
    backend.seek(seconds).await
}

#[tauri::command]
pub async fn set_volume(vol: i64) -> Result<()> {
    let backend = current_backend().await?;
    backend.set_volume(vol).await
}

#[tauri::command]
pub async fn set_fullscreen(enabled: bool, app: AppHandle) -> Result<()> {
    // Playing inside the window: the window goes fullscreen, and the page re-sizes the video.
    if embed::in_use() {
        let window = app
            .get_webview_window("main")
            .ok_or_else(|| PlayerError::Io("The main window is not available".into()))?;
        return window
            .set_fullscreen(enabled)
            .map_err(|error| PlayerError::Io(error.to_string()));
    }
    let backend = current_backend().await?;
    backend.set_fullscreen(enabled).await
}

/// Whether the video can be played inside the window, and why not when it cannot.
#[tauri::command]
pub fn embedded_player_support() -> embed::Support {
    embed::support()
}

/// Gives the embedded video `height` pixels above the page, or hides it. Returns whether the
/// video is now showing inside the window (false for the separate-window player, where this does
/// nothing), so the page knows which layout it is in.
#[tauri::command]
pub fn set_embedded_video(visible: bool, height: i32, app: AppHandle) -> Result<bool> {
    if !embed::in_use() {
        return Ok(false);
    }
    embed::set_visible(&app, visible, height.clamp(0, 10_000)).map_err(PlayerError::Io)?;
    Ok(visible && height > 0)
}

#[tauri::command]
pub async fn set_player_track(kind: String, id: String) -> Result<()> {
    let backend = current_backend().await?;
    backend.set_track(&kind, &id).await
}

/// The active profile's audio and subtitle preferences.
#[tauri::command]
pub async fn get_player_prefs_cmd(app: AppHandle) -> Result<player_prefs::PlayerPrefs> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    player_prefs::load(pool, profile_id).await
}

/// Saves the preferences for the active profile and applies them to the player that is running,
/// so a size or position change shows at once. Language choices take effect on the next file.
#[tauri::command]
pub async fn set_player_prefs_cmd(
    prefs: player_prefs::PlayerPrefs,
    app: AppHandle,
) -> Result<player_prefs::PlayerPrefs> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    let saved = player_prefs::save(pool, profile_id, &prefs).await?;
    if let Ok(backend) = current_backend().await {
        backend.apply_prefs(&saved, false).await;
    }
    Ok(saved)
}

/// Shifts the current file's subtitles, in seconds (positive shows them later). Returns the
/// delay that was applied after rounding and limiting.
#[tauri::command]
pub async fn set_subtitle_delay_cmd(seconds: f64) -> Result<f64> {
    current_backend().await?.set_subtitle_delay(seconds).await
}

/// Sets the playback speed of the current file; returns the speed applied after limiting.
#[tauri::command]
pub async fn set_speed_cmd(speed: f64) -> Result<f64> {
    current_backend().await?.set_speed(speed).await
}

/// Adds a subtitle file chosen by the user to the current file.
#[tauri::command]
pub async fn add_subtitle_file_cmd(path: String) -> Result<()> {
    current_backend().await?.add_subtitle_file(&path).await
}

/// Adds the stored subtitle for the item that started as `generation`, retrying briefly because
/// mpv only accepts a subtitle once the file has opened. Drops it if another file was started.
async fn add_when_ready(backend: &SharedBackend, path: &Path, generation: u64) {
    for _ in 0..20 {
        if PLAY_GENERATION.load(std::sync::atomic::Ordering::SeqCst) != generation {
            return;
        }
        if backend
            .add_subtitle_file(&path.to_string_lossy())
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn spawn_subtitle_autoload(
    app: AppHandle,
    backend: SharedBackend,
    item: lumen_core::subtitles::service::PlayingItem,
    preference: String,
    site_tracks: Vec<extract::ExtractedSubtitle>,
    generation: u64,
) {
    if preference == "off" {
        return;
    }
    tokio::spawn(async move {
        use lumen_core::subtitles::{extractor, service};
        let Ok(pool) = ensure_db(&app).await else {
            return;
        };
        let wanted: Vec<String> = if preference == "auto" {
            Vec::new()
        } else {
            vec![preference]
        };
        let Ok(key) = service::content_key_for(pool, &item).await else {
            return;
        };
        let mut path = service::chosen(pool, &key, &wanted).await.ok().flatten();
        if path.is_none()
            && !wanted.is_empty()
            && let Some(track) = extractor::pick(&site_tracks, &wanted)
            && let Ok(app_dir) = app.path().app_data_dir()
        {
            let allow_lan = allow_lan_sources(pool).await;
            path = extractor::fetch_and_store(pool, &app_dir, &key, track, allow_lan)
                .await
                .ok();
        }
        if let Some(path) = path {
            add_when_ready(&backend, &path, generation).await;
        }
    });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleServiceState {
    id: &'static str,
    enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleState {
    /// The user has agreed to look subtitles up on the internet.
    online: bool,
    services: Vec<SubtitleServiceState>,
}

/// Whether online subtitle search is on, and which services are switched on.
#[tauri::command]
pub async fn subtitles_state_cmd(app: AppHandle) -> Result<SubtitleState> {
    use lumen_core::subtitles::{gestdown, mirror, service};
    let pool = ensure_db(&app).await?;
    let mut services = Vec::new();
    for id in [mirror::ID, gestdown::ID] {
        let enabled = get_setting(pool, &service::provider_setting(id))
            .await?
            .as_deref()
            != Some("false");
        services.push(SubtitleServiceState { id, enabled });
    }
    Ok(SubtitleState {
        online: service::online_enabled(pool).await?,
        services,
    })
}

/// Turns online subtitle search on or off. The page asks first and says what is sent.
#[tauri::command]
pub async fn subtitles_set_online_cmd(on: bool, app: AppHandle) -> Result<()> {
    let pool = ensure_db(&app).await?;
    lumen_core::subtitles::service::set_online(pool, on).await
}

/// Switches one subtitle service on or off.
#[tauri::command]
pub async fn subtitles_set_service_cmd(id: String, enabled: bool, app: AppHandle) -> Result<()> {
    use lumen_core::subtitles::{gestdown, mirror, service};
    if id != mirror::ID && id != gestdown::ID {
        return Err(PlayerError::InvalidArg(
            "That is not a subtitle service".into(),
        ));
    }
    let pool = ensure_db(&app).await?;
    set_setting(
        pool,
        &service::provider_setting(&id),
        if enabled { "true" } else { "false" },
    )
    .await
}

/// Looks for subtitles for what is playing, in the given languages (best first). Only runs when
/// the user has turned online search on.
#[tauri::command]
pub async fn subtitles_search_cmd(
    item: lumen_core::subtitles::service::PlayingItem,
    languages: Vec<String>,
    app: AppHandle,
) -> Result<lumen_core::subtitles::SearchOutcome> {
    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    lumen_core::subtitles::service::search(pool, &item, &languages, allow_lan).await
}

/// Downloads the subtitle the user picked, stores it, remembers it for the title and shows it.
#[tauri::command]
pub async fn subtitles_apply_cmd(
    item: lumen_core::subtitles::service::PlayingItem,
    candidate: lumen_core::subtitles::SubtitleCandidate,
    app: AppHandle,
) -> Result<()> {
    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| PlayerError::Io(e.to_string()))?;
    let path =
        lumen_core::subtitles::service::apply(pool, &app_dir, &item, &candidate, allow_lan).await?;
    current_backend()
        .await?
        .add_subtitle_file(&path.to_string_lossy())
        .await
}

#[tauri::command]
pub async fn set_player_quality(id: String) -> Result<()> {
    let backend = current_backend().await?;
    backend.set_quality(&id).await
}

#[tauri::command]
pub async fn stop(app: AppHandle) -> Result<()> {
    // Take the handle and release the lock first: Stop must never wait behind a load.
    let taken = {
        let backend_cell = get_playback_backend().await?;
        let mut guard = backend_cell.lock().await;
        guard.take()
    };
    if let Some(backend) = taken {
        // `stop` cancels any load still waiting on this backend before telling mpv to quit.
        let _ = backend.stop().await;
    }
    let _ = app.emit("player://state", PlayerStateEvent::default());
    if embed::in_use() {
        let _ = embed::set_visible(&app, false, 0);
        embed::set_in_use(false);
    }
    Ok(())
}

/// Opt-in: play video inside the Lumen window instead of a separate mpv window (X11 only).
const EMBED_PLAYER_SETTING: &str = "embed_player";

/// The user's opt-in to fetch from their own network (a NAS, Jellyfin on this machine).
const ALLOW_LAN_SETTING: &str = "allow_lan_sources";

async fn allow_lan_sources(pool: &sqlx::Pool<sqlx::Sqlite>) -> bool {
    matches!(get_setting(pool, ALLOW_LAN_SETTING).await, Ok(Some(value)) if value == "true")
}

#[tauri::command]
pub async fn import_playlist_file(path: String, app: AppHandle) -> Result<i64> {
    let (playlist, channels) = import_m3u_file(&path, None).await?;

    let pool = ensure_db(&app).await?;
    import_playlist(pool, &playlist, &channels).await
}

#[tauri::command]
pub async fn import_playlist_url(url: String, name: Option<String>, app: AppHandle) -> Result<i64> {
    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    let (playlist, channels) = import_m3u_url(&url, name, allow_lan).await?;
    import_playlist(pool, &playlist, &channels).await
}

#[tauri::command]
pub async fn delete_playlist_cmd(playlist_id: i64, app: AppHandle) -> Result<()> {
    let pool = ensure_db(&app).await?;
    delete_playlist(pool, playlist_id).await?;
    crate::sync::schedule(pool);
    Ok(())
}

#[derive(Serialize)]
pub struct FallbackSource {
    pub url: String,
    pub headers: Option<serde_json::Value>,
    pub channel_id: Option<i64>,
}

/// The next ranked source for the title that `failed_url` belongs to, if any.
#[tauri::command]
pub async fn next_fallback_source(
    failed_url: String,
    app: AppHandle,
) -> Result<Option<FallbackSource>> {
    let pool = ensure_db(&app).await?;
    let next = next_ranked_source_after(pool, &failed_url).await?;
    Ok(next.map(|(url, headers, channel_id)| FallbackSource {
        url,
        headers: headers.and_then(|raw| serde_json::from_str(&raw).ok()),
        channel_id,
    }))
}

/// Library entries that are new since this profile last used Lumen (empty on the very first
/// visit). The first call of a session also records this visit, so "since last visit" means
/// since the previous session, however many times the page asks.
#[tauri::command]
pub async fn board_new_channel_ids(app: AppHandle) -> Result<Vec<i64>> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    match crate::board::begin_visit(pool, profile_id, Utc::now().timestamp_millis()).await? {
        Some(previous) => crate::board::new_title_channel_ids(pool, previous, 40).await,
        None => Ok(Vec::new()),
    }
}

/// Marks episodes of one season watched or unwatched for the active profile. The mark overrides
/// what history says, and is synced like other profile state.
#[tauri::command]
pub async fn set_episodes_watched_cmd(
    series_key: String,
    season: i64,
    episodes: Vec<i64>,
    watched: bool,
    app: AppHandle,
) -> Result<()> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    crate::series::set_episodes_watched(
        pool,
        profile_id,
        &series_key,
        season,
        &episodes,
        watched,
        Utc::now().timestamp_millis(),
    )
    .await?;
    crate::sync::schedule(pool);
    Ok(())
}

#[tauri::command]
pub async fn list_episode_marks_cmd(app: AppHandle) -> Result<Vec<crate::series::EpisodeMark>> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    crate::series::list_episode_marks(pool, profile_id).await
}

/// Series the active profile follows that have an episode still to air, or one that aired in the
/// last two weeks. With `refresh`, stale schedules are first re-read from TMDB (needs a key);
/// without a key or without a network the stored schedules are returned as they are.
#[tauri::command]
pub async fn upcoming_episodes_cmd(
    refresh: Option<bool>,
    app: AppHandle,
) -> Result<Vec<crate::series::UpcomingSeries>> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    if refresh.unwrap_or(false) {
        // A TMDB failure must not hide what is already known.
        if let Err(error) = tmdb::refresh_airing(pool, profile_id, 12).await {
            eprintln!("lumen: schedule refresh failed: {}", String::from(error));
        }
    }
    let today = Utc::now().date_naive();
    let fmt = |date: chrono::NaiveDate| date.format("%Y-%m-%d").to_string();
    crate::series::list_upcoming(
        pool,
        profile_id,
        &fmt(today),
        &fmt(today - chrono::Days::new(14)),
    )
    .await
}

#[tauri::command]
pub async fn list_playlists_cmd(app: AppHandle) -> Result<Vec<Playlist>> {
    let pool = ensure_db(&app).await?;
    list_playlists(pool).await
}

#[tauri::command]
pub async fn list_channels_cmd(
    playlist_id: i64,
    query: Option<String>,
    group: Option<String>,
    app: AppHandle,
) -> Result<Vec<EnrichedChannel>> {
    let pool = ensure_db(&app).await?;
    list_channels(pool, playlist_id, query.as_deref(), group.as_deref()).await
}

#[tauri::command]
pub async fn search_engine(
    query: String,
    check_availability: Option<bool>,
    preferred_language: Option<String>,
    app: AppHandle,
) -> Result<SearchResponse> {
    let pool = ensure_db(&app).await?;
    let term = contains_pattern(query.trim());
    let rows = sqlx::query(
        "SELECT id, playlist_id, name, url, group_title, tvg_id, language, extra_headers, tmdb_type, release_year \
         FROM channels WHERE name LIKE ? ESCAPE '\\' OR group_title LIKE ? ESCAPE '\\' OR tvg_id LIKE ? ESCAPE '\\' \
         ORDER BY name LIMIT 5000",
    )
    .bind(&term)
    .bind(&term)
    .bind(&term)
    .fetch_all(pool)
    .await
    .map_err(|error| PlayerError::Sql(error.to_string()))?;
    let library = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.get::<i64, _>("id"),
                "playlist_id": row.get::<i64, _>("playlist_id"),
                "name": row.get::<String, _>("name"),
                "url": row.get::<String, _>("url"),
                "group_title": row.get::<Option<String>, _>("group_title"),
                "tvg_id": row.get::<Option<String>, _>("tvg_id"),
                "language": row.get::<Option<String>, _>("language"),
                "extra_headers": row.get::<Option<String>, _>("extra_headers"),
                "tmdb_type": row.get::<Option<String>, _>("tmdb_type"),
                "release_year": row.get::<Option<i64>, _>("release_year"),
            })
        })
        .collect();
    let providers = with_keychain_tokens(load_stored_providers(pool).await?)?;
    let allow_lan = allow_lan_sources(pool).await;
    let request = SearchRequest {
        query,
        library,
        providers,
        check_availability: check_availability.unwrap_or(true),
        preferred_language: preferred_language.filter(|value| !value.trim().is_empty()),
        allow_lan,
    };
    let response = search::execute_search(&request).await?;
    if check_availability.unwrap_or(true) {
        for result in &response.results {
            if let (Some(id), Some(available)) = (
                result.get("channel_id").and_then(|value| value.as_i64()),
                result.get("available").and_then(|value| value.as_bool()),
            ) {
                let status = result.get("status").and_then(|value| value.as_i64());
                let error = if available {
                    None
                } else {
                    Some(
                        status
                            .map(|code| format!("HTTP {code}"))
                            .unwrap_or_else(|| "Unreachable".into()),
                    )
                };
                let _ = update_channel_last_played(pool, id, available, error).await;
            }
        }
    }
    Ok(response)
}

#[tauri::command]
pub async fn get_search_providers(app: AppHandle) -> Result<Vec<serde_json::Value>> {
    let pool = ensure_db(&app).await?;
    load_stored_providers(pool).await
}

/// Reads provider configuration from settings. Tokens never live there: any legacy
/// plaintext token is moved into the OS keychain and removed from the stored copy.
async fn load_stored_providers(pool: &sqlx::Pool<sqlx::Sqlite>) -> Result<Vec<serde_json::Value>> {
    let raw = get_setting(pool, "search_providers")
        .await?
        .unwrap_or_else(|| "[]".into());
    let mut providers: Vec<serde_json::Value> = serde_json::from_str(&raw)?;
    let mut migrated = false;
    for provider in &mut providers {
        let id = secrets::provider_id(provider)?;
        if let Some(token) = secrets::take_token(provider)? {
            if !token.is_empty() {
                secrets::store_token(&id, &token)?;
            }
            migrated = true;
        }
    }
    if migrated {
        set_setting(
            pool,
            "search_providers",
            &serde_json::to_string(&providers)?,
        )
        .await?;
    }
    Ok(providers)
}

/// Adds each provider's keychain token back in, for the search providers in a search request.
fn with_keychain_tokens(mut providers: Vec<serde_json::Value>) -> Result<Vec<serde_json::Value>> {
    for provider in &mut providers {
        let id = secrets::provider_id(provider)?;
        if let Some(token) = secrets::load_token(&id)?
            && let Some(object) = provider.as_object_mut()
        {
            object.insert("token".into(), serde_json::Value::String(token));
        }
    }
    Ok(providers)
}

#[tauri::command]
pub async fn set_search_providers(
    providers: Vec<serde_json::Value>,
    app: AppHandle,
) -> Result<Vec<serde_json::Value>> {
    if providers.len() > 12 {
        return Err(PlayerError::InvalidArg(
            "At most 12 external providers can be enabled".into(),
        ));
    }
    search::validate_search_providers(&providers)?;
    let pool = ensure_db(&app).await?;
    let previous = load_stored_providers(pool).await?;
    let mut stored = Vec::with_capacity(providers.len());
    let mut kept_ids = HashSet::new();
    for mut provider in providers {
        let id = secrets::provider_id(&provider)?;
        // A missing token keeps the saved one, because the UI never receives tokens back.
        // An empty token clears it. A non-empty token replaces it.
        match secrets::take_token(&mut provider)? {
            Some(token) if token.is_empty() => secrets::delete_token(&id)?,
            Some(token) => secrets::store_token(&id, &token)?,
            None => {}
        }
        kept_ids.insert(id);
        stored.push(provider);
    }
    for old in &previous {
        let id = secrets::provider_id(old)?;
        if !kept_ids.contains(&id) {
            secrets::delete_token(&id)?;
        }
    }
    set_setting(pool, "search_providers", &serde_json::to_string(&stored)?).await?;
    Ok(stored)
}

#[tauri::command]
pub async fn extract_url(url: String, app: AppHandle) -> Result<ExtractedEntry> {
    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    let app_dir = app.path().app_data_dir().ok();
    // Fail before saving anything when the page cannot be resolved.
    let extracted = extract::resolve_stream(&url, app_dir, allow_lan).await?;

    // The library entry is the *page*: its stream link expires and often carries a token, so it
    // is resolved again each time the entry plays (see `play_url`), never stored.
    let now = Utc::now().timestamp();
    let playlist = Playlist {
        id: None,
        name: extracted.title.clone(),
        source: "extract".to_string(),
        origin: Some(url.clone()),
        created_at: now,
        updated_at: now,
    };
    let ch = Channel {
        id: None,
        playlist_id: 0,
        name: extracted.title.clone(),
        url: url.clone(),
        group_title: Some("Extracted".to_string()),
        tvg_id: None,
        tvg_logo: None,
        language: None,
        extra_headers: None,
        last_ok: Some(now),
        last_error: None,
        is_live: None,
    };

    // Source and entry are created together; a failed insert leaves no empty source behind.
    let playlist_id = import_playlist(pool, &playlist, &[ch]).await?;
    let channel_id: i64 =
        sqlx::query_scalar("SELECT id FROM channels WHERE playlist_id = ? ORDER BY id LIMIT 1")
            .bind(playlist_id)
            .fetch_one(pool)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;

    // Playback is the caller's job: the UI plays the entry right after this returns, so a
    // failure to play reaches the user instead of being dropped here.
    Ok(ExtractedEntry {
        stream: extracted,
        channel_id,
        page_url: url,
    })
}

/// What `extract_url` returns: the resolved stream (for display) plus the saved library entry to
/// play. Playing the entry by id resolves the page again, or reuses this result for ten minutes.
#[derive(serde::Serialize)]
pub struct ExtractedEntry {
    #[serde(flatten)]
    pub stream: extract::ExtractResponse,
    pub channel_id: i64,
    pub page_url: String,
}

#[tauri::command]
pub async fn extractor_status_cmd(app: AppHandle) -> Result<ExtractorStatus> {
    let app_dir = app.path().app_data_dir().ok();
    extract::extractor_status(app_dir.as_deref())
}

#[tauri::command]
pub async fn update_extractor_cmd(app: AppHandle) -> Result<ExtractorStatus> {
    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| PlayerError::Io(e.to_string()))?;
    extract::update_managed_extractor(&app_dir, None, allow_lan).await
}

#[tauri::command]
pub async fn harvest_page(url: String, app: AppHandle) -> Result<HarvestResponse> {
    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    let harvested = harvest::harvest_streams(&url, allow_lan).await?;

    if !harvested.urls.is_empty() {
        let now = Utc::now().timestamp();
        let host_label = url
            .split('/')
            .nth(2)
            .unwrap_or("Harvested Page")
            .to_string();
        let playlist = Playlist {
            id: None,
            name: format!("Harvested: {}", host_label),
            source: "extract".to_string(),
            origin: Some(url.clone()),
            created_at: now,
            updated_at: now,
        };
        let mut all_channels = Vec::new();
        for (idx, found_url) in harvested.urls.iter().enumerate() {
            // If it's a playlist (.m3u), try importing its channels; otherwise add as direct stream.
            // An HLS manifest found on the page comes back as the single stream it describes.
            if found_url.to_lowercase().ends_with(".m3u")
                && let Ok((_, parsed_chans)) = import_m3u_url(found_url, None, allow_lan).await
            {
                all_channels.extend(parsed_chans);
                continue;
            }
            let stream_name = found_url
                .split('/')
                .next_back()
                .and_then(|s| s.split('?').next())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("Stream #{}", idx + 1));

            all_channels.push(Channel {
                id: None,
                playlist_id: 0,
                name: stream_name,
                url: found_url.clone(),
                group_title: Some("Harvested".to_string()),
                tvg_id: None,
                tvg_logo: None,
                language: None,
                extra_headers: None,
                last_ok: None,
                last_error: None,
                is_live: None,
            });
        }

        // A page can yield URLs that all turn out to be unimportable; an empty source in the
        // sidebar is worse than an honest "nothing found" response.
        import_playlist_if_nonempty(pool, &playlist, &all_channels).await?;
    }

    Ok(harvested)
}

#[tauri::command]
pub async fn probe_urls(ids: Vec<i64>, app: AppHandle) -> Result<ProbeResponse> {
    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    let mut id_and_target: Vec<(i64, ProbeTarget)> = Vec::new();

    for id in ids {
        if let Ok(channel) = get_channel(pool, id).await {
            let headers: HashMap<String, String> = channel
                .extra_headers
                .as_deref()
                .and_then(|json| serde_json::from_str(json).ok())
                .unwrap_or_default();
            id_and_target.push((
                id,
                ProbeTarget {
                    url: channel.url,
                    headers,
                },
            ));
        }
    }

    if id_and_target.is_empty() {
        return Ok(ProbeResponse {
            results: Vec::new(),
        });
    }

    let targets: Vec<ProbeTarget> = id_and_target.iter().map(|(_, t)| t.clone()).collect();
    let outcomes = probe::probe_targets_bounded(
        targets.clone(),
        allow_lan,
        probe::DEFAULT_OVERALL_DEADLINE,
        |idx, res| {
            let _ = app.emit(
                "probe://progress",
                serde_json::json!({
                    "channelId": id_and_target[idx].0,
                    "result": res,
                }),
            );
        },
    )
    .await;

    let mut results = Vec::with_capacity(id_and_target.len());
    for ((channel_id, target), outcome) in id_and_target.into_iter().zip(outcomes) {
        let res = outcome.unwrap_or(ProbeResult {
            url: target.url,
            live: false,
            status: None,
            content_type: None,
        });
        let err_msg = if res.live {
            None
        } else {
            Some(
                res.status
                    .map(|s| format!("HTTP {}", s))
                    .unwrap_or_else(|| "Unreachable".to_string()),
            )
        };
        let _ = update_channel_last_played(pool, channel_id, res.live, err_msg).await;
        results.push(res);
    }

    Ok(ProbeResponse { results })
}

#[tauri::command]
pub async fn toggle_favorite_cmd(channel_id: i64, app: AppHandle) -> Result<bool> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    let favorite = toggle_favorite(pool, profile_id, channel_id).await?;
    crate::sync::schedule(pool);
    Ok(favorite)
}

#[tauri::command]
pub async fn list_favorites_cmd(app: AppHandle) -> Result<Vec<i64>> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    list_favorites(pool, profile_id).await
}

#[tauri::command]
pub async fn get_history_cmd(limit: Option<i64>, app: AppHandle) -> Result<Vec<HistoryEntry>> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    get_history(pool, profile_id, limit.unwrap_or(50)).await
}

#[tauri::command]
pub async fn save_progress_cmd(
    channel_id: Option<i64>,
    path: String,
    position: f64,
    duration: f64,
    flush: Option<bool>,
    app: AppHandle,
) -> Result<()> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    let result = add_history(pool, profile_id, channel_id, &path, position, duration).await;
    // The periodic save during playback is spaced out; the final one (`flush`, when playback
    // stops) goes out at once so the other devices see where you stopped.
    if flush.unwrap_or(false) {
        crate::sync::schedule(pool);
    } else {
        crate::sync::schedule_progress(pool);
    }
    result
}

#[tauri::command]
pub async fn list_profiles_cmd(app: AppHandle) -> Result<Vec<Profile>> {
    let pool = ensure_db(&app).await?;
    list_profiles(pool).await
}

/// The profile the app is acting as, so the frontend knows whose name and PIN state to show.
#[tauri::command]
pub async fn active_profile_cmd(app: AppHandle) -> Result<Profile> {
    let pool = ensure_db(&app).await?;
    get_profile(pool, active_profile_id(pool).await?).await
}

#[tauri::command]
pub async fn create_profile_cmd(
    name: String,
    color: Option<String>,
    app: AppHandle,
) -> Result<Profile> {
    let pool = ensure_db(&app).await?;
    let profile = create_profile(pool, &name, color.as_deref()).await?;
    crate::sync::schedule(pool);
    Ok(profile)
}

#[tauri::command]
pub async fn rename_profile_cmd(
    profile_id: i64,
    name: String,
    current_pin: Option<String>,
    app: AppHandle,
) -> Result<Profile> {
    let pool = ensure_db(&app).await?;
    authorize_profile(pool, profile_id, current_pin.as_deref()).await?;
    let profile = rename_profile(pool, profile_id, &name).await?;
    crate::sync::schedule(pool);
    Ok(profile)
}

#[tauri::command]
pub async fn delete_profile_cmd(
    profile_id: i64,
    current_pin: Option<String>,
    app: AppHandle,
) -> Result<Vec<Profile>> {
    let pool = ensure_db(&app).await?;
    authorize_profile(pool, profile_id, current_pin.as_deref()).await?;
    delete_profile(pool, profile_id).await?;
    crate::sync::schedule(pool);

    // The deleted profile may have been the active one; the next read falls back on its own,
    // so this just reports the surviving profiles.
    list_profiles(pool).await
}

#[tauri::command]
pub async fn set_profile_pin_cmd(
    profile_id: i64,
    pin: Option<String>,
    current_pin: Option<String>,
    app: AppHandle,
) -> Result<Profile> {
    let pool = ensure_db(&app).await?;
    // Setting a PIN on an open profile needs nothing; changing or removing one needs the old PIN.
    authorize_profile(pool, profile_id, current_pin.as_deref()).await?;
    set_profile_pin(pool, profile_id, pin.as_deref()).await
}

/// Check whether a profile is reachable, and make it the active one.
///
/// A profile with a PIN cannot be activated without that PIN, so switching profiles and
/// unlocking a locked profile are the same operation.
#[tauri::command]
pub async fn activate_profile_cmd(
    profile_id: i64,
    pin: Option<String>,
    app: AppHandle,
) -> Result<Profile> {
    let pool = ensure_db(&app).await?;

    authorize_profile(pool, profile_id, pin.as_deref()).await?;

    set_active_profile(pool, profile_id).await?;
    touch_profile(pool, profile_id).await?;

    get_profile(pool, profile_id).await
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/home"))
}

/// The path field is free text, so it accepts the `~` shortcut people type at a shell.
/// `read_dir` needs the expansion; `~` on its own means the home directory.
fn expand_home(path: &str) -> PathBuf {
    let trimmed = path.trim();
    if trimmed == "~" {
        return home_dir();
    }
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        return home_dir().join(rest);
    }
    PathBuf::from(trimmed)
}

/// Longest an ffprobe/ffmpeg run on one file may take before it is stopped.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const FRAME_TIMEOUT: Duration = Duration::from_secs(20);
const FILMSTRIP_TIMEOUT: Duration = Duration::from_secs(30);
/// Cached posters beyond this size are deleted, oldest first.
const THUMBNAIL_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Pruning scans the cache directory, so it runs every few writes rather than on each one.
const PRUNE_EVERY_WRITES: usize = 25;
static CACHE_WRITES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A page of local videos asks for dozens of posters at once. Only a few ffmpeg processes run at a
/// time; the rest wait here instead of starting a process (and a blocking thread) each.
static FFMPEG_SLOTS: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(3));

fn note_cache_write(cache_dir: &Path) {
    if CACHE_WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % PRUNE_EVERY_WRITES
        == PRUNE_EVERY_WRITES - 1
    {
        prune_cache(cache_dir, THUMBNAIL_CACHE_MAX_BYTES);
    }
}

fn generate_local_filmstrip(path: PathBuf, cache_dir: PathBuf) -> Result<Vec<u8>> {
    let source = path
        .canonicalize()
        .map_err(|error| PlayerError::Io(format!("Local video not found: {error}")))?;
    if !source.is_file() || !is_video_path(&source) {
        return Err(PlayerError::InvalidArg(
            "No video preview for this file".into(),
        ));
    }

    let metadata = std::fs::metadata(&source)?;
    let modified = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let key = cache_name(&[
        &source.to_string_lossy(),
        &metadata.len().to_string(),
        &modified.as_nanos().to_string(),
        "filmstrip",
    ]);
    let cached = cache_dir.join(format!("{key}.jpg"));
    if let Ok(bytes) = std::fs::read(&cached)
        && bytes.len() > 4
        && bytes.starts_with(&[0xff, 0xd8])
    {
        return Ok(bytes);
    }

    let mut probe_command = Command::new("ffprobe");
    probe_command
        // Local files only: a crafted "video" must not make ffprobe open other files or URLs.
        .args([
            "-protocol_whitelist",
            "file",
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&source);
    let probe = run_with_timeout(probe_command, PROBE_TIMEOUT)?;
    let duration = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| {
            PlayerError::InvalidArg("Could not determine video duration for preview".into())
        })?;

    std::fs::create_dir_all(&cache_dir)?;
    let temporary = cache_dir.join(format!(".{key}-{}.jpg", std::process::id()));
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    for fraction in [0.12, 0.36, 0.62, 0.88] {
        command.args([
            "-protocol_whitelist",
            "file",
            "-ss",
            &format!("{:.3}", duration * fraction),
            "-i",
        ]);
        command.arg(&source);
    }
    let mut filters: Vec<String> = (0..4)
        .map(|index| format!("[{index}:v:0]scale=240:136:force_original_aspect_ratio=increase,crop=240:136,setsar=1[v{index}]"))
        .collect();
    filters.push("[v0][v1][v2][v3]hstack=inputs=4[out]".into());
    command
        .args([
            "-filter_complex",
            &filters.join(";"),
            "-map",
            "[out]",
            "-frames:v",
            "1",
            "-q:v",
            "6",
            "-threads",
            "1",
            "-an",
            "-sn",
            "-dn",
            "-y",
        ])
        .arg(&temporary);
    let output = run_with_timeout(command, FILMSTRIP_TIMEOUT)?;
    if output.status.success()
        && let Ok(bytes) = std::fs::read(&temporary)
        && bytes.len() > 4
        && bytes.len() <= 2_000_000
        && bytes.starts_with(&[0xff, 0xd8])
    {
        std::fs::rename(&temporary, &cached)?;
        note_cache_write(&cache_dir);
        return Ok(bytes);
    }
    let _ = std::fs::remove_file(&temporary);
    Err(PlayerError::InvalidArg(
        "Could not extract preview frames from this video".into(),
    ))
}

#[tauri::command]
pub async fn local_filmstrip(path: String, app: AppHandle) -> Result<String> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| PlayerError::Io(error.to_string()))?
        .join("thumbnails");
    let _slot = FFMPEG_SLOTS
        .acquire()
        .await
        .map_err(|error| PlayerError::Io(error.to_string()))?;
    let bytes = tokio::task::spawn_blocking(move || {
        generate_local_filmstrip(PathBuf::from(path), cache_dir)
    })
    .await
    .map_err(|error| PlayerError::Io(format!("Filmstrip task failed: {error}")))??;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

const POSTER_FRAME_SETTING: &str = "poster_frame_seconds";
const SEARCH_LANGUAGE_SETTING: &str = "search_preferred_language";
const SEARCH_HIDE_OTHER_LANGUAGES_SETTING: &str = "search_hide_other_languages";
const DEFAULT_POSTER_FRAME_SECONDS: u32 = 2;
/// Frame times offered in Settings. Only these reach ffmpeg, so a hand-edited row cannot seek arbitrarily.
const POSTER_FRAME_CHOICES: &[u32] = &[0, 2, 5, 10, 30, 60];

fn parse_poster_frame_seconds(raw: &str) -> Option<u32> {
    raw.trim()
        .parse::<u32>()
        .ok()
        .filter(|seconds| POSTER_FRAME_CHOICES.contains(seconds))
}

fn poster_frame_seconds_from(stored: Option<String>) -> u32 {
    stored
        .and_then(|raw| parse_poster_frame_seconds(&raw))
        .unwrap_or(DEFAULT_POSTER_FRAME_SECONDS)
}

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "avi", "mov", "webm", "ts", "m2ts"];

fn is_video_path(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();
    VIDEO_EXTENSIONS.contains(&extension.as_str())
}

/// The frame chosen for one file, or None when it follows the global setting.
async fn get_file_poster_frame(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    path: &Path,
) -> Result<Option<u32>> {
    let source = match path.canonicalize() {
        Ok(source) => source,
        Err(_) => return Ok(None),
    };
    let row = sqlx::query("SELECT seconds FROM poster_frames WHERE path = ?")
        .bind(source.to_string_lossy().to_string())
        .fetch_optional(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(row
        .map(|row| row.get::<i64, _>("seconds"))
        .and_then(|seconds| u32::try_from(seconds).ok())
        .filter(|seconds| POSTER_FRAME_CHOICES.contains(seconds)))
}

/// Saves the frame for one file. `None` removes the file's choice so it follows the global setting again.
async fn save_file_poster_frame(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    path: &Path,
    seconds: Option<u32>,
) -> Result<()> {
    let source = path
        .canonicalize()
        .map_err(|error| PlayerError::Io(format!("Local video not found: {error}")))?;
    if !source.is_file() || !is_video_path(&source) {
        return Err(PlayerError::InvalidArg(
            "No video poster for this file".into(),
        ));
    }
    let key = source.to_string_lossy().to_string();
    match seconds {
        None => {
            sqlx::query("DELETE FROM poster_frames WHERE path = ?")
                .bind(key)
                .execute(pool)
                .await
                .map_err(|e| PlayerError::Sql(e.to_string()))?;
        }
        Some(seconds) => {
            if !POSTER_FRAME_CHOICES.contains(&seconds) {
                return Err(PlayerError::InvalidArg(
                    "Poster frame must be one of the offered times".into(),
                ));
            }
            sqlx::query("INSERT INTO poster_frames (path, seconds) VALUES (?, ?) ON CONFLICT(path) DO UPDATE SET seconds = excluded.seconds")
                .bind(key)
                .bind(i64::from(seconds))
                .execute(pool)
                .await
                .map_err(|e| PlayerError::Sql(e.to_string()))?;
        }
    }
    Ok(())
}

/// A file's own frame wins; otherwise the global setting applies.
fn resolve_poster_frame(file_choice: Option<u32>, global_setting: Option<String>) -> u32 {
    file_choice.unwrap_or_else(|| poster_frame_seconds_from(global_setting))
}

fn thumbnail_cache_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app
        .path()
        .app_cache_dir()
        .map_err(|error| PlayerError::Io(error.to_string()))?
        .join("thumbnails"))
}

/// Removes cached posters and filmstrips so they regenerate with the current settings.
fn clear_thumbnail_cache(cache_dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(cache_dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PlayerError::Io(format!(
            "Could not clear the poster cache: {error}"
        ))),
    }
}

fn generate_local_thumbnail(
    path: PathBuf,
    cache_dir: PathBuf,
    frame_seconds: u32,
    fall_back_to_start: bool,
) -> Result<Vec<u8>> {
    let source = path
        .canonicalize()
        .map_err(|error| PlayerError::Io(format!("Local video not found: {error}")))?;
    if !source.is_file() || !is_video_path(&source) {
        return Err(PlayerError::InvalidArg(
            "No video poster for this file".into(),
        ));
    }

    let metadata = std::fs::metadata(&source)?;
    let modified = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let key = cache_name(&[
        &source.to_string_lossy(),
        &metadata.len().to_string(),
        &modified.as_nanos().to_string(),
        &frame_seconds.to_string(),
    ]);
    let cached = cache_dir.join(format!("{key}.jpg"));
    if let Ok(bytes) = std::fs::read(&cached)
        && bytes.len() > 4
        && bytes.starts_with(&[0xff, 0xd8])
    {
        return Ok(bytes);
    }

    std::fs::create_dir_all(&cache_dir)?;
    let temporary = cache_dir.join(format!(".{key}-{}.jpg", std::process::id()));
    let mut last_error = None;
    // The requested frame skips intros; for saved posters, retry at the first frame for clips shorter than that.
    let requested = frame_seconds.to_string();
    let mut seeks = vec![requested.as_str()];
    if fall_back_to_start && frame_seconds != 0 {
        seeks.push("0");
    }
    for seek in seeks {
        let mut command = Command::new("ffmpeg");
        command
            // `-protocol_whitelist file`: a crafted "video" must not make ffmpeg open other files or URLs.
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-protocol_whitelist",
                "file",
                "-ss",
                seek,
                "-i",
            ])
            .arg(&source)
            .args([
                "-map",
                "0:v:0",
                "-frames:v",
                "1",
                "-vf",
                "scale=480:-2:force_original_aspect_ratio=decrease",
                "-q:v",
                "5",
                "-threads",
                "1",
                "-an",
                "-sn",
                "-dn",
                "-y",
            ])
            .arg(&temporary);
        let output = run_with_timeout(command, FRAME_TIMEOUT)?;
        if output.status.success() {
            if let Ok(bytes) = std::fs::read(&temporary)
                && bytes.len() > 4
                && bytes.starts_with(&[0xff, 0xd8])
                && bytes.len() <= 2_000_000
            {
                std::fs::rename(&temporary, &cached)?;
                note_cache_write(&cache_dir);
                return Ok(bytes);
            }
        } else {
            last_error = Some(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        let _ = std::fs::remove_file(&temporary);
    }
    let _ = std::fs::remove_file(&temporary);
    Err(PlayerError::InvalidArg(
        last_error
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "Could not extract a frame from this video".into()),
    ))
}

#[tauri::command]
pub async fn local_thumbnail(path: String, app: AppHandle) -> Result<String> {
    let pool = ensure_db(&app).await?;
    let file_choice = get_file_poster_frame(pool, Path::new(&path)).await?;
    let frame_seconds =
        resolve_poster_frame(file_choice, get_setting(pool, POSTER_FRAME_SETTING).await?);
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| PlayerError::Io(error.to_string()))?
        .join("thumbnails");
    let _slot = FFMPEG_SLOTS
        .acquire()
        .await
        .map_err(|error| PlayerError::Io(error.to_string()))?;
    let bytes = tokio::task::spawn_blocking(move || {
        generate_local_thumbnail(PathBuf::from(path), cache_dir, frame_seconds, true)
    })
    .await
    .map_err(|error| PlayerError::Io(format!("Thumbnail task failed: {error}")))??;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[derive(Debug, Serialize)]
pub struct PosterCandidate {
    pub seconds: u32,
    /// Base64 JPEG, or None when the video ends before this frame time.
    pub image: Option<String>,
}

/// Renders every offered frame time for the Settings picker. The first frame must succeed so a
/// non-video path is reported as an error; later frames past a short clip's end come back without an image.
fn render_poster_candidates(path: PathBuf, cache_dir: PathBuf) -> Result<Vec<PosterCandidate>> {
    let mut candidates = Vec::with_capacity(POSTER_FRAME_CHOICES.len());
    for (index, &seconds) in POSTER_FRAME_CHOICES.iter().enumerate() {
        match generate_local_thumbnail(path.clone(), cache_dir.clone(), seconds, false) {
            Ok(bytes) => candidates.push(PosterCandidate {
                seconds,
                image: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            }),
            Err(error) if index == 0 => return Err(error),
            Err(_) => candidates.push(PosterCandidate {
                seconds,
                image: None,
            }),
        }
    }
    Ok(candidates)
}

#[tauri::command]
pub async fn local_poster_candidates(path: String, app: AppHandle) -> Result<Vec<PosterCandidate>> {
    let cache_dir = thumbnail_cache_dir(&app)?;
    let _slot = FFMPEG_SLOTS
        .acquire()
        .await
        .map_err(|error| PlayerError::Io(error.to_string()))?;
    tokio::task::spawn_blocking(move || render_poster_candidates(expand_home(&path), cache_dir))
        .await
        .map_err(|error| PlayerError::Io(format!("Frame preview task failed: {error}")))?
}

/// Returns the frame a file was given, or null when it follows the global setting.
#[tauri::command]
pub async fn get_local_poster_frame(path: String, app: AppHandle) -> Result<Option<u32>> {
    let pool = ensure_db(&app).await?;
    get_file_poster_frame(pool, &expand_home(&path)).await
}

/// Chooses the poster frame for one file. A null frame clears the choice so the file follows the global setting.
#[tauri::command]
pub async fn set_local_poster_frame(
    path: String,
    seconds: Option<u32>,
    app: AppHandle,
) -> Result<()> {
    let pool = ensure_db(&app).await?;
    save_file_poster_frame(pool, &expand_home(&path), seconds).await
}

#[tauri::command]
pub async fn list_local_dir(path: String, _app: AppHandle) -> Result<Vec<LocalFile>> {
    let target_path = if path.trim().is_empty() {
        home_dir()
    } else {
        expand_home(&path)
    };

    let entries = std::fs::read_dir(&target_path).map_err(|e| PlayerError::Io(e.to_string()))?;

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let entry_path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        if file_name.starts_with('.') {
            continue;
        }
        if entry_path.is_file() {
            if let Some(ext) = entry_path.extension().and_then(|s| s.to_str()) {
                let ext = ext.to_lowercase();
                if matches!(
                    ext.as_str(),
                    "mp4"
                        | "mkv"
                        | "avi"
                        | "mov"
                        | "webm"
                        | "ts"
                        | "m2ts"
                        | "mp3"
                        | "flac"
                        | "wav"
                        | "ogg"
                        | "opus"
                        | "m4a"
                        | "m3u"
                        | "m3u8"
                ) {
                    files.push(LocalFile {
                        name: file_name,
                        path: entry_path.to_string_lossy().to_string(),
                        is_dir: false,
                        size: entry.metadata().ok().map(|m| m.len()),
                    });
                }
            }
        } else if entry_path.is_dir() {
            files.push(LocalFile {
                name: file_name,
                path: entry_path.to_string_lossy().to_string(),
                is_dir: true,
                size: None,
            });
        }
    }

    files.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            b.is_dir.cmp(&a.is_dir)
        } else {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        }
    });

    Ok(files)
}

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<serde_json::Value> {
    let pool = ensure_db(&app).await?;

    let hwdec = get_setting(pool, "hwdec")
        .await?
        .unwrap_or_else(|| "auto-safe".to_string());
    let extra_args = get_setting(pool, "extra_mpv_args")
        .await?
        .unwrap_or_default();
    let cache = get_setting(pool, "cache")
        .await?
        .unwrap_or_else(|| "yes".to_string());
    let poster_frame = poster_frame_seconds_from(get_setting(pool, POSTER_FRAME_SETTING).await?);
    let search_language = get_setting(pool, SEARCH_LANGUAGE_SETTING)
        .await?
        .unwrap_or_default();
    let hide_other_languages = get_setting(pool, SEARCH_HIDE_OTHER_LANGUAGES_SETTING)
        .await?
        .unwrap_or_default();

    Ok(serde_json::json!({
        "hwdec": hwdec,
        "extra_mpv_args": extra_args,
        "cache": cache,
        "poster_frame_seconds": poster_frame.to_string(),
        "search_preferred_language": search_language,
        "search_hide_other_languages": hide_other_languages,
        "allow_lan_sources": get_setting(pool, ALLOW_LAN_SETTING).await?.unwrap_or_default(),
    }))
}

/// Settings the UI may write. The settings table also holds internal state (the active profile,
/// sync cursors); a generic writer must not be able to reach those, or it could switch to a
/// locked profile without its PIN.
const USER_SETTING_KEYS: &[&str] = &[
    "hwdec",
    "cache",
    "extra_mpv_args",
    ALLOW_LAN_SETTING,
    EMBED_PLAYER_SETTING,
    POSTER_FRAME_SETTING,
    SEARCH_LANGUAGE_SETTING,
    SEARCH_HIDE_OTHER_LANGUAGES_SETTING,
];

fn is_user_setting(key: &str) -> bool {
    USER_SETTING_KEYS.contains(&key)
}

#[tauri::command]
pub async fn set_setting_cmd(key: String, value: String, app: AppHandle) -> Result<()> {
    if !is_user_setting(&key) {
        return Err(PlayerError::InvalidArg(format!(
            "\"{key}\" is not a user setting"
        )));
    }
    if key == "search_providers" {
        return Err(PlayerError::InvalidArg(
            "Use set_search_providers to change search providers".into(),
        ));
    }
    if key == tmdb::API_KEY_SETTING {
        return Err(PlayerError::InvalidArg(
            "Use set_tmdb_api_key to change the TMDB API key".into(),
        ));
    }
    let pool = ensure_db(&app).await?;
    if key == POSTER_FRAME_SETTING {
        return set_poster_frame_seconds(pool, &thumbnail_cache_dir(&app)?, &value).await;
    }
    set_setting(pool, &key, &value).await
}

/// Saves the poster frame. Cached posters are cleared only when the frame actually changes.
async fn set_poster_frame_seconds(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    cache_dir: &Path,
    value: &str,
) -> Result<()> {
    let seconds = parse_poster_frame_seconds(value).ok_or_else(|| {
        PlayerError::InvalidArg("Poster frame must be one of the offered times".into())
    })?;
    let previous = poster_frame_seconds_from(get_setting(pool, POSTER_FRAME_SETTING).await?);
    set_setting(pool, POSTER_FRAME_SETTING, &seconds.to_string()).await?;
    if previous != seconds {
        clear_thumbnail_cache(cache_dir)?;
    }
    Ok(())
}

/// Stores the TMDB API key in the OS keychain. A blank key clears it. Any legacy plaintext
/// copy in settings is removed only after the keychain write succeeds.
#[tauri::command]
pub async fn set_tmdb_api_key(key: String, app: AppHandle) -> Result<()> {
    let pool = ensure_db(&app).await?;
    match tmdb::normalize_key(&key) {
        Some(key) => secrets::store_tmdb_key(&key)?,
        None => secrets::delete_tmdb_key()?,
    }
    delete_setting(pool, tmdb::API_KEY_SETTING).await
}

/// Reports whether a TMDB key is available, without ever returning the key itself.
#[tauri::command]
pub async fn tmdb_status(app: AppHandle) -> Result<serde_json::Value> {
    let pool = ensure_db(&app).await?;
    let key = tmdb::api_key(pool).await?;
    let configured = key.is_some();
    let key_source = key.as_ref().map(|(_, source)| *source).unwrap_or("none");
    let (enriched, total) = tmdb::counts(pool).await?;

    Ok(serde_json::json!({
        "configured": configured,
        "key_source": key_source,
        "enriched": enriched,
        "total": total,
        "image_base": tmdb::IMAGE_BASE,
        "poster_size": tmdb::POSTER_SIZE,
        "backdrop_size": tmdb::BACKDROP_SIZE,
    }))
}

/// Enrich a bounded batch, so the UI can show progress across a large library.
#[tauri::command]
pub async fn tmdb_enrich(
    limit: Option<i64>,
    force: Option<bool>,
    app: AppHandle,
) -> Result<serde_json::Value> {
    let pool = ensure_db(&app).await?;
    let outcome = tmdb::enrich(pool, limit.unwrap_or(40), force.unwrap_or(false)).await?;

    Ok(serde_json::json!({
        "enriched": outcome.enriched,
        "matched": outcome.matched,
        "skipped": outcome.skipped,
        "remaining": outcome.remaining,
        "requests": outcome.requests,
    }))
}

/// Trending and top-rated TMDB titles, so Discovery can show more than the matched library.
#[tauri::command]
pub async fn tmdb_browse(app: AppHandle) -> Result<serde_json::Value> {
    let pool = ensure_db(&app).await?;
    let (key, _source) = tmdb::api_key(pool)
        .await?
        .ok_or_else(|| PlayerError::InvalidArg("TMDB API key not configured".into()))?;
    let titles = tmdb::browse(&key).await?;
    Ok(serde_json::json!({ "titles": titles }))
}

#[tauri::command]
pub async fn tmdb_clear_enrichment(app: AppHandle) -> Result<serde_json::Value> {
    let pool = ensure_db(&app).await?;
    let cleared = tmdb::clear_enrichment(pool).await?;
    Ok(serde_json::json!({ "cleared": cleared }))
}

#[cfg(test)]
mod local_path_tests {
    use super::{
        POSTER_FRAME_CHOICES, POSTER_FRAME_SETTING, expand_home, generate_local_filmstrip,
        generate_local_thumbnail, get_file_poster_frame, home_dir, parse_poster_frame_seconds,
        poster_frame_seconds_from, render_poster_candidates, resolve_poster_frame,
        save_file_poster_frame, set_poster_frame_seconds,
    };
    use crate::db::{get_setting, set_setting};
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use std::{fs, thread};

    #[test]
    fn expands_tilde_home_paths_without_rewriting_other_paths() {
        let home = home_dir();
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/Videos"), home.join("Videos"));
        assert_eq!(expand_home("~\\Videos"), home.join("Videos"));
        assert_eq!(expand_home("/tmp/media"), PathBuf::from("/tmp/media"));
        assert_eq!(
            expand_home("~someone/Videos"),
            PathBuf::from("~someone/Videos")
        );
    }

    #[test]
    fn poster_candidates_offer_every_frame_and_omit_frames_past_the_end() {
        use base64::Engine as _;
        let temp =
            std::env::temp_dir().join(format!("lumen-candidate-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).expect("temporary test directory");
        let video = temp.join("clip.mp4");
        let generated = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=640x360:rate=2:duration=8",
                "-c:v",
                "mpeg4",
                "-y",
            ])
            .arg(&video)
            .output()
            .expect("ffmpeg should be installed for the poster candidate integration test");
        assert!(
            generated.status.success(),
            "could not create test video: {}",
            String::from_utf8_lossy(&generated.stderr)
        );

        let cache = temp.join("cache");
        let candidates = render_poster_candidates(video, cache).expect("poster candidates");
        let offered: Vec<u32> = candidates
            .iter()
            .map(|candidate| candidate.seconds)
            .collect();
        assert_eq!(
            offered,
            POSTER_FRAME_CHOICES.to_vec(),
            "every offered frame time is listed in order"
        );

        for candidate in &candidates {
            if candidate.seconds < 8 {
                let encoded = candidate.image.as_ref().unwrap_or_else(|| {
                    panic!("frame {}s is inside the 8s clip", candidate.seconds)
                });
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .expect("base64 image");
                assert!(bytes.starts_with(&[0xff, 0xd8]), "candidate is a JPEG");
            } else {
                assert!(
                    candidate.image.is_none(),
                    "frame {}s is past the 8s clip and has no image",
                    candidate.seconds
                );
            }
        }

        let invalid = temp.join("notes.txt");
        fs::write(&invalid, "not a video").expect("invalid file");
        assert!(
            render_poster_candidates(invalid, temp.join("cache-invalid")).is_err(),
            "non-video files report an error"
        );
    }

    #[test]
    fn generates_and_caches_four_frame_video_filmstrips() {
        let temp =
            std::env::temp_dir().join(format!("lumen-filmstrip-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).expect("temporary test directory");
        let video = temp.join("clip.mp4");
        let generated = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=640x360:rate=2:duration=8",
                "-c:v",
                "mpeg4",
                "-y",
            ])
            .arg(&video)
            .output()
            .expect("ffmpeg should be installed for the filmstrip integration test");
        assert!(
            generated.status.success(),
            "could not create test video: {}",
            String::from_utf8_lossy(&generated.stderr)
        );

        let cache = temp.join("cache");
        let first =
            generate_local_filmstrip(video.clone(), cache.clone()).expect("filmstrip extraction");
        assert!(first.starts_with(&[0xff, 0xd8]), "filmstrip is a JPEG");
        assert!(first.len() <= 2_000_000, "filmstrip stays small");
        let image = temp.join("filmstrip.jpg");
        fs::write(&image, &first).expect("write filmstrip fixture");
        let dimensions = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=s=x:p=0",
            ])
            .arg(&image)
            .output()
            .expect("ffprobe should read filmstrip dimensions");
        assert!(dimensions.status.success());
        assert_eq!(
            String::from_utf8_lossy(&dimensions.stdout).trim(),
            "960x136",
            "contact strip contains four 240x136 frames"
        );
        let cached = fs::read_dir(&cache)
            .expect("cache directory")
            .next()
            .expect("cached strip")
            .expect("cache entry")
            .path();
        let modified = fs::metadata(&cached)
            .expect("cached strip metadata")
            .modified()
            .expect("mtime");
        thread::sleep(Duration::from_millis(20));
        let second = generate_local_filmstrip(video, cache).expect("cached filmstrip");
        assert_eq!(
            first, second,
            "repeated requests return identical cached filmstrip bytes"
        );
        assert_eq!(
            modified,
            fs::metadata(cached)
                .expect("cached strip metadata")
                .modified()
                .expect("mtime"),
            "cache hit does not rewrite the filmstrip"
        );
        fs::remove_dir_all(temp).expect("remove test fixture");
    }

    #[test]
    fn generates_and_caches_jpeg_video_posters() {
        let temp =
            std::env::temp_dir().join(format!("lumen-thumbnail-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).expect("temporary test directory");
        let video = temp.join("clip.mp4");
        let generated = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=640x360:d=1",
                "-frames:v",
                "1",
                "-y",
            ])
            .arg(&video)
            .output()
            .expect("ffmpeg should be installed for the thumbnail integration test");
        assert!(
            generated.status.success(),
            "could not create test video: {}",
            String::from_utf8_lossy(&generated.stderr)
        );
        assert!(video.metadata().expect("video metadata").len() > 0);

        let cache = temp.join("cache");
        let first = generate_local_thumbnail(video.clone(), cache.clone(), 2, true)
            .expect("thumbnail extraction");
        assert!(first.starts_with(&[0xff, 0xd8]), "poster is a JPEG");
        assert!(first.len() <= 2_000_000, "poster stays small");
        let cached = fs::read_dir(&cache)
            .expect("cache directory")
            .next()
            .expect("cached poster")
            .expect("cache entry")
            .path();
        let modified = fs::metadata(&cached)
            .expect("cached poster metadata")
            .modified()
            .expect("mtime");
        thread::sleep(Duration::from_millis(20));
        let second = generate_local_thumbnail(video.clone(), cache.clone(), 2, true)
            .expect("cached thumbnail");
        assert_eq!(
            first, second,
            "repeated requests return identical cached poster bytes"
        );
        assert_eq!(
            modified,
            fs::metadata(cached)
                .expect("cached poster metadata")
                .modified()
                .expect("mtime"),
            "cache hit does not rewrite the poster"
        );

        let invalid = temp.join("not-a-video.txt");
        fs::write(&invalid, "not video").expect("invalid fixture");
        assert!(
            generate_local_thumbnail(invalid, cache.clone(), 2, true).is_err(),
            "non-video files do not produce posters"
        );

        let other_frame = generate_local_thumbnail(video, cache.clone(), 0, true)
            .expect("poster at a different frame time");
        assert!(other_frame.starts_with(&[0xff, 0xd8]), "poster is a JPEG");
        assert_eq!(
            fs::read_dir(&cache).expect("cache directory").count(),
            2,
            "each frame time has its own cached poster"
        );
        fs::remove_dir_all(temp).expect("remove test fixture");
    }

    #[test]
    fn poster_frame_setting_accepts_only_offered_times() {
        assert_eq!(parse_poster_frame_seconds("5"), Some(5));
        assert_eq!(parse_poster_frame_seconds(" 0 "), Some(0));
        assert_eq!(parse_poster_frame_seconds("7"), None);
        assert_eq!(parse_poster_frame_seconds("-1"), None);
        assert_eq!(parse_poster_frame_seconds("2; rm -rf /"), None);
        assert_eq!(poster_frame_seconds_from(None), 2);
        assert_eq!(poster_frame_seconds_from(Some("999".into())), 2);
        assert_eq!(poster_frame_seconds_from(Some("60".into())), 60);
    }

    #[tokio::test]
    async fn changing_poster_frame_setting_clears_thumbnail_cache() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::run_migrations(&pool).await.unwrap();

        let temp = std::env::temp_dir().join(format!(
            "lumen-poster-setting-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let cache = temp.join("thumbnails");
        let poster = cache.join("0000000000000000.jpg");
        let seed_cache = |cache: &Path| {
            fs::create_dir_all(cache).expect("cache directory");
            fs::write(cache.join("0000000000000000.jpg"), [0xff, 0xd8, 0xff, 0xd9])
                .expect("cached poster");
        };

        // Default frame is 2s, so choosing 5s is a change and must drop the old posters.
        seed_cache(&cache);
        set_poster_frame_seconds(&pool, &cache, "5")
            .await
            .expect("changing the frame saves");
        assert_eq!(
            get_setting(&pool, POSTER_FRAME_SETTING)
                .await
                .unwrap()
                .as_deref(),
            Some("5")
        );
        assert!(!cache.exists(), "changing the frame clears cached posters");

        // Saving the same frame again must keep the cache.
        seed_cache(&cache);
        set_poster_frame_seconds(&pool, &cache, "5")
            .await
            .expect("same frame saves");
        assert!(poster.exists(), "an unchanged frame keeps cached posters");

        // A rejected value never reaches storage and leaves the cache alone.
        assert!(set_poster_frame_seconds(&pool, &cache, "7").await.is_err());
        assert!(poster.exists(), "a rejected frame keeps cached posters");
        assert_eq!(
            get_setting(&pool, POSTER_FRAME_SETTING)
                .await
                .unwrap()
                .as_deref(),
            Some("5")
        );

        set_poster_frame_seconds(&pool, &cache, "10")
            .await
            .expect("changing the frame saves");
        assert!(
            !cache.exists(),
            "changing the frame again clears cached posters"
        );
        fs::remove_dir_all(temp).expect("remove test fixture");
    }

    #[tokio::test]
    async fn per_file_poster_frame_overrides_the_global_setting() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::run_migrations(&pool).await.unwrap();

        let temp = std::env::temp_dir().join(format!(
            "lumen-file-poster-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp).expect("fixture directory");
        let clip = temp.join("clip.mp4");
        let other = temp.join("other.mkv");
        write_test_video(&clip).expect("fixture video");
        write_test_video(&other).expect("fixture video");
        let notes = temp.join("notes.txt");
        fs::write(&notes, b"not a video").expect("fixture text");

        set_setting(&pool, POSTER_FRAME_SETTING, "5").await.unwrap();

        // Without a per-file choice, the file follows the global frame.
        assert_eq!(get_file_poster_frame(&pool, &clip).await.unwrap(), None);
        assert_eq!(
            resolve_poster_frame(
                None,
                get_setting(&pool, POSTER_FRAME_SETTING).await.unwrap()
            ),
            5
        );

        // A per-file choice wins for that file only; the global setting stays unchanged.
        save_file_poster_frame(&pool, &clip, Some(30))
            .await
            .expect("per-file frame saves");
        assert_eq!(get_file_poster_frame(&pool, &clip).await.unwrap(), Some(30));
        assert_eq!(
            resolve_poster_frame(
                get_file_poster_frame(&pool, &clip).await.unwrap(),
                get_setting(&pool, POSTER_FRAME_SETTING).await.unwrap()
            ),
            30
        );
        assert_eq!(get_file_poster_frame(&pool, &other).await.unwrap(), None);
        assert_eq!(
            get_setting(&pool, POSTER_FRAME_SETTING)
                .await
                .unwrap()
                .as_deref(),
            Some("5")
        );

        // Rejected frames and non-video files leave the stored choice alone.
        assert!(save_file_poster_frame(&pool, &clip, Some(7)).await.is_err());
        assert!(
            save_file_poster_frame(&pool, &notes, Some(30))
                .await
                .is_err()
        );
        assert_eq!(get_file_poster_frame(&pool, &clip).await.unwrap(), Some(30));

        // Clearing the choice returns the file to the global frame.
        save_file_poster_frame(&pool, &clip, None)
            .await
            .expect("clearing saves");
        assert_eq!(get_file_poster_frame(&pool, &clip).await.unwrap(), None);

        fs::remove_dir_all(temp).expect("remove test fixture");
    }

    /// A per-file choice must change the pixels ffmpeg extracts, not just the stored number, so this
    /// renders real frames and compares them. `testsrc` changes every frame, making the frames distinct.
    #[test]
    fn per_file_poster_frame_changes_the_rendered_image() {
        let temp = std::env::temp_dir().join(format!(
            "lumen-file-render-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&temp).expect("fixture directory");
        let clip = temp.join("clip.mp4");
        write_test_video(&clip).expect("fixture video");
        let cache = temp.join("cache");

        // The global frame renders the start of the video.
        let global = generate_local_thumbnail(clip.clone(), cache.clone(), 0, true)
            .expect("global frame renders");
        assert!(global.starts_with(&[0xff, 0xd8]), "global frame is a JPEG");

        // The per-file choice renders its own frame, and the two cached files differ.
        let per_file = generate_local_thumbnail(clip.clone(), cache.clone(), 5, true)
            .expect("per-file frame renders");
        assert!(
            per_file.starts_with(&[0xff, 0xd8]),
            "per-file frame is a JPEG"
        );
        assert_ne!(
            global, per_file,
            "the chosen frame time extracts different pixels than the global one"
        );
        assert_eq!(
            fs::read_dir(&cache).expect("cache directory").count(),
            2,
            "each frame time keeps its own cached poster"
        );

        fs::remove_dir_all(temp).expect("remove test fixture");
    }

    /// Builds a short video whose contents change over time, so seeking to different frame times
    /// yields genuinely different frames.
    fn write_test_video(path: &Path) -> std::io::Result<()> {
        let generated = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=640x360:rate=2:duration=8",
                "-c:v",
                "mpeg4",
                "-y",
            ])
            .arg(path)
            .output()?;
        assert!(
            generated.status.success(),
            "could not create test video: {}",
            String::from_utf8_lossy(&generated.stderr)
        );
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct LocalFile {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

#[cfg(test)]
mod setting_allowlist_tests {
    use super::is_user_setting;

    #[test]
    fn only_user_facing_settings_are_writable() {
        for key in [
            "hwdec",
            "cache",
            "extra_mpv_args",
            "allow_lan_sources",
            "poster_frame_seconds",
            "search_preferred_language",
            "search_hide_other_languages",
        ] {
            assert!(is_user_setting(key), "{key} is a user setting");
        }
        // Internal state shares the table: writing these would bypass a PIN or corrupt sync.
        for key in [
            "active_profile_id",
            "sync_pull_cursor",
            "sync_seeded",
            "sync_last_synced_at",
            "search_providers",
            "tmdb_api_key",
            // Online subtitle search is switched only through its own command, after the notice.
            "subtitle_online",
            "subtitle_provider_gestdown",
            "",
        ] {
            assert!(
                !is_user_setting(key),
                "{key:?} must not be writable from the UI"
            );
        }
    }
}
