pub use lumen_core::{
    auth, board, db, error, extract, harvest, media_tools, netguard, pin_guard, player_prefs,
    playlist, probe, search, secrets, series, tmdb,
};

mod bulk;
mod cast;
mod commands;
#[cfg(target_os = "linux")]
mod embed;
#[cfg(not(target_os = "linux"))]
#[path = "embed_unsupported.rs"]
mod embed;
mod launch;
#[cfg(target_os = "linux")]
mod mpris;
mod playback;
mod privacy;
mod sync;
mod updater;

use crate::commands::*;
use crate::db::*;
use tauri::{Manager, RunEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Before anything else can possibly make a network call: if network privacy turns out to be
    // on, nothing must go direct in the gap between process start and `privacy::init` actually
    // reading the setting (further down, once the database is ready).
    lumen_core::privacy::set_route(lumen_core::privacy::Route::Pending);

    // A way out for machines where the page engine's newer compositing path is slow or broken
    // (some NVIDIA/Wayland setups): `LUMEN_WEBKIT_SAFE=1 lumen`. Not the default, because it
    // trades speed elsewhere for safety.
    #[cfg(target_os = "linux")]
    if std::env::var_os("LUMEN_WEBKIT_SAFE").is_some() {
        // SAFETY: first thing in `run`, before any thread that reads the environment exists.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    let app = tauri::Builder::default()
        // First, so a second launch is handed to the running window before anything else starts.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            let args: Vec<String> = argv.into_iter().skip(1).collect();
            launch::handle_arguments(app, &args, Some(std::path::Path::new(&cwd)));
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(launch::PendingLaunch::default())
        .setup(|app| {
            // What this launch was asked to open ("Open with Lumen", a lumen:// link).
            let args: Vec<String> = std::env::args().skip(1).collect();
            launch::handle_arguments(app.handle(), &args, std::env::current_dir().ok().as_deref());
            embed::detect_display();
            #[cfg(target_os = "linux")]
            tune_page_engine(app.handle());
            #[cfg(target_os = "linux")]
            mpris::start(app.handle().clone());
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(app_dir) = app_handle.path().app_data_dir() {
                    let _ = std::fs::create_dir_all(&app_dir);
                    if let Err(e) = init_db(&app_dir).await {
                        eprintln!("Failed to init database: {}", e);
                    } else if let Ok(pool) = ensure_db(&app_handle).await {
                        privacy::init(&app_handle).await;
                        sync::start(pool);
                        updater::start(app_handle.clone());
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            launch::take_launch_targets,
            launch::classify_open_targets,
            board_new_channel_ids,
            set_episodes_watched_cmd,
            list_episode_marks_cmd,
            upcoming_episodes_cmd,
            sync::sync_now_cmd,
            sync::sync_status_cmd,
            sync::sync_state_cmd,
            sync::sync_set_server,
            sync::sync_enable,
            sync::sync_join,
            sync::sync_show_key,
            sync::sync_disable,
            updater::update_state_cmd,
            updater::update_set_mode_cmd,
            updater::update_check_cmd,
            updater::update_install_cmd,
            updater::update_restart_cmd,
            bulk::bulk_read_file_cmd,
            bulk::bulk_extract_text_cmd,
            bulk::bulk_check_cmd,
            bulk::bulk_cancel_cmd,
            bulk::bulk_add_cmd,
            cast::cast_discover_cmd,
            cast::cast_start_cmd,
            cast::cast_control_cmd,
            cast::cast_state_cmd,
            play_url,
            next_fallback_source,
            pause,
            seek,
            set_volume,
            set_fullscreen,
            set_player_track,
            set_player_quality,
            embedded_player_support,
            set_embedded_video,
            get_player_prefs_cmd,
            set_player_prefs_cmd,
            set_subtitle_delay_cmd,
            set_speed_cmd,
            add_subtitle_file_cmd,
            subtitles_state_cmd,
            subtitles_set_online_cmd,
            subtitles_set_service_cmd,
            subtitles_search_cmd,
            subtitles_apply_cmd,
            stop,
            import_playlist_file,
            import_playlist_url,
            delete_playlist_cmd,
            list_playlists_cmd,
            list_channels_cmd,
            search_engine,
            get_search_providers,
            set_search_providers,
            extract_url,
            extractor_status_cmd,
            update_extractor_cmd,
            harvest_page,
            probe_urls,
            toggle_favorite_cmd,
            list_favorites_cmd,
            get_history_cmd,
            save_progress_cmd,
            list_profiles_cmd,
            active_profile_cmd,
            create_profile_cmd,
            rename_profile_cmd,
            delete_profile_cmd,
            set_profile_pin_cmd,
            activate_profile_cmd,
            list_local_dir,
            local_thumbnail,
            local_filmstrip,
            local_poster_candidates,
            get_local_poster_frame,
            set_local_poster_frame,
            get_settings,
            set_setting_cmd,
            set_tmdb_api_key,
            privacy::network_privacy_state_cmd,
            privacy::network_privacy_set_cmd,
            privacy::network_privacy_test_cmd,
            tmdb_status,
            tmdb_enrich,
            tmdb_browse,
            tmdb_clear_enrichment,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app_handle, event| {
        if let RunEvent::Exit = event {
            tauri::async_runtime::block_on(async {
                if let Ok(backend_cell) = get_playback_backend().await {
                    let mut guard = backend_cell.lock().await;
                    if let Some(backend) = guard.take() {
                        let _ = backend.stop().await;
                    }
                }
            });
        }
    });
}

/// Asks the page engine for smooth, hardware-accelerated scrolling and drawing.
#[cfg(target_os = "linux")]
fn tune_page_engine(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.with_webview(|webview| {
        use webkit2gtk::{HardwareAccelerationPolicy, SettingsExt, WebViewExt};
        if let Some(settings) = webview.inner().settings() {
            settings.set_enable_smooth_scrolling(true);
            settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Always);
        }
    });
}
