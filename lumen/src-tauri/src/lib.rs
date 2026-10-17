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
                        apply_webview_proxy(&app_handle);
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
            fetch_remote_image,
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

/// How the embedded page engine's own network requests (not this app's — see `remote_image.rs`
/// and the network-privacy module for those) should relate to the current route.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WebviewProxyMode {
    /// Network privacy is off: behave exactly as before this feature existed.
    Default,
    /// Network privacy is meant to be on. Carries either the real relay's address, or, if the
    /// relay turned out not to be usable, an address nothing listens on — so the page engine's
    /// own requests fail the same way every other guarded path does rather than ever going
    /// direct.
    Custom(String),
}

/// Maps `lumen_core::privacy::child_proxy_url()`'s result to a [`WebviewProxyMode`]. A pure
/// function so this mapping is tested without needing a real webview.
fn webview_proxy_target(proxy: lumen_core::error::Result<Option<String>>) -> WebviewProxyMode {
    match proxy {
        Ok(None) => WebviewProxyMode::Default,
        Ok(Some(url)) => WebviewProxyMode::Custom(url),
        // Nothing is listening on port 1 (it's in the reserved range); any request through it
        // simply fails to connect.
        Err(_) => WebviewProxyMode::Custom("http://127.0.0.1:1".to_string()),
    }
}

/// Points the embedded page engine's own network requests (favicon prefetch, speculative
/// preconnects, and anything the frontend was not already updated to route itself — see
/// `fetch_remote_image`) at the current network-privacy route, as defense in depth. Called once
/// at startup, once the route is resolved, and again whenever the network-privacy setting
/// changes.
#[cfg(target_os = "linux")]
pub(crate) fn apply_webview_proxy(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let target = webview_proxy_target(lumen_core::privacy::child_proxy_url());
    let _ = window.with_webview(move |webview| {
        use webkit2gtk::{
            NetworkProxyMode, NetworkProxySettings, WebContextExt, WebViewExt,
            WebsiteDataManagerExt,
        };
        let Some(context) = webview.inner().context() else {
            return;
        };
        let Some(data_manager) = context.website_data_manager() else {
            return;
        };
        match target {
            WebviewProxyMode::Default => {
                data_manager.set_network_proxy_settings(NetworkProxyMode::Default, None);
            }
            WebviewProxyMode::Custom(url) => {
                let mut settings = NetworkProxySettings::new(Some(url.as_str()), &[]);
                data_manager
                    .set_network_proxy_settings(NetworkProxyMode::Custom, Some(&mut settings));
            }
        }
    });
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn apply_webview_proxy(_app: &tauri::AppHandle) {}

#[cfg(test)]
mod webview_proxy_tests {
    use super::*;

    #[test]
    fn privacy_off_uses_the_default_system_proxy() {
        assert_eq!(webview_proxy_target(Ok(None)), WebviewProxyMode::Default);
    }

    #[test]
    fn privacy_on_points_the_page_engine_at_the_relay() {
        assert_eq!(
            webview_proxy_target(Ok(Some("http://lumen:tok@127.0.0.1:12345".to_string()))),
            WebviewProxyMode::Custom("http://lumen:tok@127.0.0.1:12345".to_string())
        );
    }

    #[test]
    fn an_unusable_relay_fails_closed_rather_than_going_direct() {
        let target = webview_proxy_target(Err(lumen_core::error::PlayerError::Http(
            "down".to_string(),
        )));
        assert_ne!(target, WebviewProxyMode::Default);
    }
}
