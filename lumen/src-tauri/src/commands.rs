use crate::db::*;
use crate::error::{PlayerError, Result};
use crate::playback::mpv_ipc::MpvIpcBackend;
use crate::playback::{PlaybackBackend, PlayerStateEvent};
use crate::playlist::import::{import_m3u_file, import_m3u_url, Channel, Playlist};
use crate::sidecar::*;
use crate::tmdb;
use base64::Engine;
use chrono::Utc;
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

// Global playback backend
static PLAYBACK_BACKEND: OnceLock<Arc<Mutex<Option<MpvIpcBackend>>>> = OnceLock::new();

pub async fn get_playback_backend() -> Result<Arc<Mutex<Option<MpvIpcBackend>>>> {
    let backend = PLAYBACK_BACKEND.get_or_init(|| Arc::new(Mutex::new(None)));
    Ok(backend.clone())
}

async fn ensure_db(app: &AppHandle) -> Result<&'static sqlx::Pool<sqlx::Sqlite>> {
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

async fn ensure_sidecar_handle(app: &AppHandle) -> Result<Arc<Mutex<SidecarManager>>> {
    if let Some(sc) = get_sidecar() {
        return Ok(sc);
    }
    let binaries_dir = app
        .path()
        .app_data_dir()
        .map(|d| d.join("binaries"))
        .unwrap_or_else(|_| PathBuf::from("binaries"));
    init_sidecar(binaries_dir).await?;
    get_sidecar().ok_or_else(|| PlayerError::Sidecar("Sidecar not initialized".into()))
}

#[tauri::command]
pub async fn play_url(
    url: String,
    headers: Option<serde_json::Value>,
    channel_id: Option<i64>,
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

    let backend_cell = get_playback_backend().await?;
    let mut backend_guard = backend_cell.lock().await;

    let need_spawn = match backend_guard.as_mut() {
        Some(b) => !b.is_alive().await,
        None => true,
    };

    if need_spawn {
        *backend_guard = Some(
            MpvIpcBackend::new(Some(app.clone()), &hwdec, &cache, &extra_args).await?,
        );
    }

    let backend = backend_guard.as_mut().unwrap();
    let hdrs = headers.as_ref().and_then(|v| {
        v.as_object().map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s)))
                .collect::<Vec<_>>()
        })
    });

    backend.load(&url, hdrs.as_deref()).await?;

    // Check if there is a saved resume position for local files
    if let Ok(Some(resume_pos)) = get_resume_position(pool, profile_id, &url).await {
        if resume_pos > 1.0 {
            let _ = backend.seek(resume_pos).await;
        }
    }

    // Record history entry for the active profile
    let _ = add_history(pool, profile_id, channel_id, &url, 0.0, 0.0).await;
    if let Some(cid) = channel_id {
        let _ = update_channel_last_played(pool, cid, true, None).await;
    }

    Ok(())
}

#[tauri::command]
pub async fn pause() -> Result<()> {
    let backend_cell = get_playback_backend().await?;
    let backend_guard = backend_cell.lock().await;

    if let Some(backend) = backend_guard.as_ref() {
        backend.pause().await
    } else {
        Err(PlayerError::Mpv("No active playback".into()))
    }
}

#[tauri::command]
pub async fn seek(seconds: f64) -> Result<()> {
    let backend_cell = get_playback_backend().await?;
    let backend_guard = backend_cell.lock().await;

    if let Some(backend) = backend_guard.as_ref() {
        backend.seek(seconds).await
    } else {
        Err(PlayerError::Mpv("No active playback".into()))
    }
}

#[tauri::command]
pub async fn set_volume(vol: i64) -> Result<()> {
    let backend_cell = get_playback_backend().await?;
    let backend_guard = backend_cell.lock().await;

    if let Some(backend) = backend_guard.as_ref() {
        backend.set_volume(vol).await
    } else {
        Err(PlayerError::Mpv("No active playback".into()))
    }
}

#[tauri::command]
pub async fn stop(app: AppHandle) -> Result<()> {
    let backend_cell = get_playback_backend().await?;
    let mut backend_guard = backend_cell.lock().await;

    if let Some(backend) = backend_guard.take() {
        let _ = backend.stop().await;
    }
    let _ = app.emit("player://state", PlayerStateEvent::default());
    Ok(())
}

#[tauri::command]
pub async fn import_playlist_file(path: String, app: AppHandle) -> Result<i64> {
    let (playlist, channels) = import_m3u_file(&path, None).await?;

    let pool = ensure_db(&app).await?;
    import_playlist(pool, &playlist, &channels).await
}

#[tauri::command]
pub async fn import_playlist_url(url: String, name: Option<String>, app: AppHandle) -> Result<i64> {
    let (playlist, channels) = import_m3u_url(&url, name).await?;

    let pool = ensure_db(&app).await?;
    import_playlist(pool, &playlist, &channels).await
}

#[tauri::command]
pub async fn delete_playlist_cmd(playlist_id: i64, app: AppHandle) -> Result<()> {
    let pool = ensure_db(&app).await?;
    delete_playlist(pool, playlist_id).await
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
    providers: Vec<serde_json::Value>,
    check_availability: Option<bool>,
    app: AppHandle,
) -> Result<SearchResponse> {
    let pool = ensure_db(&app).await?;
    let term = format!("%{}%", query.trim());
    let rows = sqlx::query(
        "SELECT id, playlist_id, name, url, group_title, tvg_id, extra_headers, tmdb_type, release_year \
         FROM channels WHERE name LIKE ? OR group_title LIKE ? OR tvg_id LIKE ? \
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
                "extra_headers": row.get::<Option<String>, _>("extra_headers"),
                "tmdb_type": row.get::<Option<String>, _>("tmdb_type"),
                "release_year": row.get::<Option<i64>, _>("release_year"),
            })
        })
        .collect();
    let sidecar = ensure_sidecar_handle(&app).await?;
    let request = SearchRequest {
        query,
        library,
        providers,
        check_availability: check_availability.unwrap_or(true),
    };
    let mut manager = sidecar.lock().await;
    manager.search(&request).await
}

#[tauri::command]
pub async fn get_setting_value(key: String, app: AppHandle) -> Result<Option<String>> {
    let pool = ensure_db(&app).await?;
    get_setting(pool, &key).await
}

#[tauri::command]
pub async fn set_search_providers(
    providers: Vec<serde_json::Value>,
    app: AppHandle,
) -> Result<Vec<serde_json::Value>> {
    if providers.len() > 12 {
        return Err(PlayerError::InvalidArg("At most 12 external providers can be enabled".into()));
    }
    let pool = ensure_db(&app).await?;
    let raw = serde_json::to_string(&providers)?;
    set_setting(pool, "search_providers", &raw).await?;
    Ok(providers
        .into_iter()
        .map(|mut provider| {
            if let Some(object) = provider.as_object_mut() {
                object.remove("token");
            }
            provider
        })
        .collect())
}

#[tauri::command]
pub async fn extract_url(url: String, app: AppHandle) -> Result<ExtractResponse> {
    let sidecar = ensure_sidecar_handle(&app).await?;
    let extracted = {
        let mut sidecar_guard = sidecar.lock().await;
        sidecar_guard.extract(&url).await?
    };

    // Per PLAN.md §8: "Rust saves as a 1-item playlist, then play_url"
    let pool = ensure_db(&app).await?;
    let now = Utc::now().timestamp();
    let playlist = Playlist {
        id: None,
        name: extracted.title.clone(),
        source: "extract".to_string(),
        origin: Some(url.clone()),
        created_at: now,
        updated_at: now,
    };
    let extra_headers = if extracted.headers.is_empty() {
        None
    } else {
        serde_json::to_string(&extracted.headers).ok()
    };

    let ch = Channel {
        id: None,
        playlist_id: 0,
        name: extracted.title.clone(),
        url: extracted.url.clone(),
        group_title: Some("Extracted".to_string()),
        tvg_id: None,
        tvg_logo: None,
        extra_headers,
        last_ok: Some(now),
        last_error: None,
    };

    // Source and entry are created together; a failed insert leaves no empty source behind.
    import_playlist(pool, &playlist, &[ch]).await?;

    // Immediately start playback
    let hdr_val = serde_json::to_value(&extracted.headers).ok();
    let _ = play_url(extracted.url.clone(), hdr_val, None, app).await;

    Ok(extracted)
}

#[tauri::command]
pub async fn harvest_page(url: String, app: AppHandle) -> Result<HarvestResponse> {
    let sidecar = ensure_sidecar_handle(&app).await?;
    let harvested = {
        let mut sidecar_guard = sidecar.lock().await;
        sidecar_guard.harvest(&url).await?
    };

    if !harvested.urls.is_empty() {
        let pool = ensure_db(&app).await?;
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
            if found_url.to_lowercase().ends_with(".m3u") {
                if let Ok((_, parsed_chans)) = import_m3u_url(found_url, None).await {
                    all_channels.extend(parsed_chans);
                    continue;
                }
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
                extra_headers: None,
                last_ok: None,
                last_error: None,
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
    let mut id_to_url: Vec<(i64, String)> = Vec::new();

    for id in ids {
        if let Ok(channel) = get_channel(pool, id).await {
            id_to_url.push((id, channel.url));
        }
    }

    if id_to_url.is_empty() {
        return Ok(ProbeResponse {
            results: Vec::new(),
        });
    }

    let urls: Vec<String> = id_to_url.iter().map(|(_, u)| u.clone()).collect();

    let sidecar = ensure_sidecar_handle(&app).await?;
    let response = {
        let mut sidecar_guard = sidecar.lock().await;
        sidecar_guard.probe(urls).await?
    };

    // Map URL results back to channel IDs and persist last_ok / last_error in SQLite
    let mut url_map: HashMap<String, &ProbeResult> = HashMap::new();
    for res in &response.results {
        url_map.insert(res.url.clone(), res);
    }

    for (channel_id, u) in id_to_url {
        if let Some(res) = url_map.get(&u) {
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
        }
    }

    Ok(response)
}

#[tauri::command]
pub async fn toggle_favorite_cmd(channel_id: i64, app: AppHandle) -> Result<bool> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    toggle_favorite(pool, profile_id, channel_id).await
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
    app: AppHandle,
) -> Result<()> {
    let pool = ensure_db(&app).await?;
    let profile_id = active_profile_id(pool).await?;
    add_history(pool, profile_id, channel_id, &path, position, duration).await
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
    create_profile(pool, &name, color.as_deref()).await
}

#[tauri::command]
pub async fn rename_profile_cmd(
    profile_id: i64,
    name: String,
    app: AppHandle,
) -> Result<Profile> {
    let pool = ensure_db(&app).await?;
    rename_profile(pool, profile_id, &name).await
}

#[tauri::command]
pub async fn delete_profile_cmd(profile_id: i64, app: AppHandle) -> Result<Vec<Profile>> {
    let pool = ensure_db(&app).await?;
    delete_profile(pool, profile_id).await?;

    // The deleted profile may have been the active one; the next read falls back on its own,
    // so this just reports the surviving profiles.
    list_profiles(pool).await
}

#[tauri::command]
pub async fn set_profile_pin_cmd(
    profile_id: i64,
    pin: Option<String>,
    app: AppHandle,
) -> Result<Profile> {
    let pool = ensure_db(&app).await?;
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

    if !verify_profile_pin(pool, profile_id, pin.as_deref().unwrap_or_default()).await? {
        return Err(PlayerError::InvalidArg(
            "That PIN is not correct".to_string(),
        ));
    }

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
    if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    PathBuf::from(trimmed)
}

fn generate_local_thumbnail(path: PathBuf, cache_dir: PathBuf) -> Result<Vec<u8>> {
    const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "avi", "mov", "webm", "ts", "m2ts"];
    let source = path
        .canonicalize()
        .map_err(|error| PlayerError::Io(format!("Local video not found: {error}")))?;
    let extension = source.extension().and_then(|value| value.to_str()).unwrap_or("").to_lowercase();
    if !source.is_file() || !VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        return Err(PlayerError::InvalidArg("No video poster for this file".into()));
    }

    let metadata = std::fs::metadata(&source)?;
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default();
    let fingerprint = (source.to_string_lossy().to_string(), metadata.len(), modified.as_nanos());
    let mut hasher = DefaultHasher::new();
    fingerprint.hash(&mut hasher);
    let cached = cache_dir.join(format!("{:016x}.jpg", hasher.finish()));
    if let Ok(bytes) = std::fs::read(&cached) {
        if bytes.len() > 4 && bytes.starts_with(&[0xff, 0xd8]) {
            return Ok(bytes);
        }
    }

    std::fs::create_dir_all(&cache_dir)?;
    let temporary = cache_dir.join(format!(".{:016x}-{}.jpg", hasher.finish(), std::process::id()));
    let mut last_error = None;
    for seek in ["2", "0"] {
        let output = Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-ss", seek, "-i"])
            .arg(&source)
            .args(["-map", "0:v:0", "-frames:v", "1", "-vf", "scale=480:-2:force_original_aspect_ratio=decrease", "-q:v", "5", "-threads", "1", "-an", "-sn", "-dn", "-y"])
            .arg(&temporary)
            .output()
            .map_err(|error| PlayerError::Io(format!("Could not start ffmpeg: {error}")))?;
        if output.status.success() {
            if let Ok(bytes) = std::fs::read(&temporary) {
                if bytes.len() > 4 && bytes.starts_with(&[0xff, 0xd8]) && bytes.len() <= 2_000_000 {
                    std::fs::rename(&temporary, &cached)?;
                    return Ok(bytes);
                }
            }
        } else {
            last_error = Some(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        let _ = std::fs::remove_file(&temporary);
    }
    let _ = std::fs::remove_file(&temporary);
    Err(PlayerError::InvalidArg(last_error.filter(|message| !message.is_empty()).unwrap_or_else(|| "Could not extract a frame from this video".into())))
}

#[tauri::command]
pub async fn local_thumbnail(path: String, app: AppHandle) -> Result<String> {
    let cache_dir = app.path().app_cache_dir().map_err(|error| PlayerError::Io(error.to_string()))?.join("thumbnails");
    let bytes = tokio::task::spawn_blocking(move || generate_local_thumbnail(PathBuf::from(path), cache_dir))
        .await
        .map_err(|error| PlayerError::Io(format!("Thumbnail task failed: {error}")))??;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[tauri::command]
pub async fn list_local_dir(path: String, _app: AppHandle) -> Result<Vec<LocalFile>> {
    let target_path = if path.trim().is_empty() {
        PathBuf::from(home_dir())
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

    Ok(serde_json::json!({
        "hwdec": hwdec,
        "extra_mpv_args": extra_args,
        "cache": cache,
    }))
}

#[tauri::command]
pub async fn set_setting_cmd(key: String, value: String, app: AppHandle) -> Result<()> {
    let pool = ensure_db(&app).await?;
    set_setting(pool, &key, &value).await
}

/// Reports whether a TMDB key is available, without ever returning the key itself.
#[tauri::command]
pub async fn tmdb_status(app: AppHandle) -> Result<serde_json::Value> {
    let pool = ensure_db(&app).await?;
    let key = tmdb::api_key(pool).await?;
    let configured = key.is_some();
    let key_source = key
        .as_ref()
        .map(|(_, source)| *source)
        .unwrap_or("none");
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

#[tauri::command]
pub async fn tmdb_clear_enrichment(app: AppHandle) -> Result<serde_json::Value> {
    let pool = ensure_db(&app).await?;
    let cleared = tmdb::clear_enrichment(pool).await?;
    Ok(serde_json::json!({ "cleared": cleared }))
}

#[cfg(test)]
mod local_path_tests {
    use super::{expand_home, generate_local_thumbnail, home_dir};
    use std::path::PathBuf;
    use std::{fs, thread};
    use std::time::Duration;

    #[test]
    fn expands_tilde_home_paths_without_rewriting_other_paths() {
        let home = home_dir();
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/Videos"), home.join("Videos"));
        assert_eq!(expand_home("~\\Videos"), home.join("Videos"));
        assert_eq!(expand_home("/tmp/media"), PathBuf::from("/tmp/media"));
        assert_eq!(expand_home("~someone/Videos"), PathBuf::from("~someone/Videos"));
    }

    #[test]
    fn generates_and_caches_jpeg_video_posters() {
        let temp = std::env::temp_dir().join(format!("lumen-thumbnail-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).expect("temporary test directory");
        let video = temp.join("clip.mp4");
        let generated = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=blue:s=640x360:d=1", "-frames:v", "1", "-y"])
            .arg(&video)
            .output()
            .expect("ffmpeg should be installed for the thumbnail integration test");
        assert!(generated.status.success(), "could not create test video: {}", String::from_utf8_lossy(&generated.stderr));
        assert!(video.metadata().expect("video metadata").len() > 0);

        let cache = temp.join("cache");
        let first = generate_local_thumbnail(video.clone(), cache.clone()).expect("thumbnail extraction");
        assert!(first.starts_with(&[0xff, 0xd8]), "poster is a JPEG");
        assert!(first.len() <= 2_000_000, "poster stays small");
        let cached = fs::read_dir(&cache).expect("cache directory").next().expect("cached poster").expect("cache entry").path();
        let modified = fs::metadata(&cached).expect("cached poster metadata").modified().expect("mtime");
        thread::sleep(Duration::from_millis(20));
        let second = generate_local_thumbnail(video.clone(), cache.clone()).expect("cached thumbnail");
        assert_eq!(first, second, "repeated requests return identical cached poster bytes");
        assert_eq!(modified, fs::metadata(cached).expect("cached poster metadata").modified().expect("mtime"), "cache hit does not rewrite the poster");

        let invalid = temp.join("not-a-video.txt");
        fs::write(&invalid, "not video").expect("invalid fixture");
        assert!(generate_local_thumbnail(invalid, cache).is_err(), "non-video files do not produce posters");
        fs::remove_dir_all(temp).expect("remove test fixture");
    }
}

#[derive(Debug, Serialize)]
pub struct LocalFile {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}
