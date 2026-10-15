mod auth;
mod commands;
mod db;
mod error;
mod playback;
mod playlist;
mod secrets;
mod sidecar;
mod tmdb;

use crate::commands::*;
use crate::db::*;
use crate::sidecar::{get_sidecar, init_sidecar};
use tauri::{Manager, RunEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(app_dir) = app_handle.path().app_data_dir() {
                    let _ = std::fs::create_dir_all(&app_dir);
                    if let Err(e) = init_db(&app_dir).await {
                        eprintln!("Failed to init database: {}", e);
                    }

                    // Initialize sidecar
                    let binaries_dir = app_dir.join("binaries");
                    if let Err(e) = init_sidecar(binaries_dir).await {
                        eprintln!("Sidecar not available at startup: {}", e);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            play_url,
            pause,
            seek,
            set_volume,
            set_fullscreen,
            set_player_track,
            set_player_quality,
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
            get_settings,
            set_setting_cmd,
            set_tmdb_api_key,
            tmdb_status,
            tmdb_enrich,
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
                        use crate::playback::PlaybackBackend;
                        let _ = backend.stop().await;
                    }
                }
                if let Some(sc) = get_sidecar() {
                    let mut guard = sc.lock().await;
                    guard.shutdown().await;
                }
            });
        }
    });
}
