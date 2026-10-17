//! Tauri side of network privacy: reads and writes the settings, keeps the OS keychain in
//! sync, and runs the lifecycle (starting the relay at launch, re-checking it whenever a
//! relevant setting changes). The relay itself and the routing decisions live in
//! `lumen_core::privacy`; this file only wires that module to the app.

use lumen_core::privacy::{self, Route};
use lumen_core::{db, secrets};
use serde::Serialize;
use tauri::AppHandle;

use crate::commands::{allow_lan_sources, ensure_db};
use crate::error::Result;

/// Reads the real network-privacy settings and resolves the route away from `Pending` (set at
/// the very start of `run`, before this async setup task could possibly lose the race to a
/// network call). Called once at startup, after the database is ready.
pub async fn init(app: &AppHandle) {
    let Ok(pool) = ensure_db(app).await else {
        // No database means no settings to read, and nothing this early could have asked to
        // connect to anything either. Leave the route `Pending` rather than guess `Direct`.
        return;
    };
    let allow_lan = allow_lan_sources(pool).await;
    privacy::reconcile(pool, allow_lan).await;
}

#[derive(Serialize)]
pub struct NetworkPrivacyState {
    pub enabled: bool,
    pub upstream: String,
    pub username: String,
    pub has_password: bool,
    /// `"off"`, `"active"` or `"unavailable"`.
    pub status: String,
    pub error: Option<String>,
}

async fn state(app: &AppHandle) -> Result<NetworkPrivacyState> {
    let pool = ensure_db(app).await?;
    let enabled = matches!(
        db::get_setting(pool, privacy::ENABLED_SETTING).await,
        Ok(Some(value)) if value == "true"
    );
    let upstream = db::get_setting(pool, privacy::UPSTREAM_SETTING)
        .await?
        .unwrap_or_default();
    let username = db::get_setting(pool, privacy::USERNAME_SETTING)
        .await?
        .unwrap_or_default();
    let has_password = secrets::load_proxy_password()
        .unwrap_or(None)
        .is_some_and(|p| !p.is_empty());
    let (status, error) = match privacy::current_route() {
        Route::Direct | Route::Pending => ("off".to_string(), None),
        Route::Relay { .. } => ("active".to_string(), None),
        Route::Unavailable(reason) => ("unavailable".to_string(), Some(reason)),
    };
    Ok(NetworkPrivacyState {
        enabled,
        upstream,
        username,
        has_password,
        status,
        error,
    })
}

#[tauri::command]
pub async fn network_privacy_state_cmd(app: AppHandle) -> Result<NetworkPrivacyState> {
    state(&app).await
}

/// Validates and saves the network-privacy settings, then re-checks the relay to match.
/// `password`: `None` leaves the saved password as-is, `Some("")` clears it, anything else
/// replaces it. The upstream address is validated before anything is saved, so a typo never
/// leaves the feature half-configured.
#[tauri::command]
pub async fn network_privacy_set_cmd(
    enabled: bool,
    upstream: String,
    username: String,
    password: Option<String>,
    app: AppHandle,
) -> Result<NetworkPrivacyState> {
    let upstream = upstream.trim().to_string();
    let username = username.trim().to_string();
    if enabled {
        privacy::parse_upstream(&upstream)?;
    }
    if let Some(password) = &password {
        if password.is_empty() {
            secrets::delete_proxy_password()?;
        } else {
            secrets::store_proxy_password(password)?;
        }
    }

    let pool = ensure_db(&app).await?;
    db::set_setting(pool, privacy::UPSTREAM_SETTING, &upstream).await?;
    db::set_setting(pool, privacy::USERNAME_SETTING, &username).await?;
    db::set_setting(
        pool,
        privacy::ENABLED_SETTING,
        if enabled { "true" } else { "false" },
    )
    .await?;

    let allow_lan = allow_lan_sources(pool).await;
    privacy::reconcile(pool, allow_lan).await;
    state(&app).await
}

/// Tests an upstream address and credentials before they are saved, so a mistake shows up in
/// the form instead of only after the feature is turned on.
#[tauri::command]
pub async fn network_privacy_test_cmd(
    upstream: String,
    username: String,
    password: Option<String>,
) -> Result<String> {
    let mut parsed = privacy::parse_upstream(upstream.trim())?;
    let username = username.trim();
    parsed.username = (!username.is_empty()).then(|| username.to_string());
    let started = std::time::Instant::now();
    privacy::test_upstream(&parsed, password.as_deref().filter(|p| !p.is_empty())).await?;
    Ok(format!("{}ms", started.elapsed().as_millis()))
}
