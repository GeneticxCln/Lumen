//! Tauri desktop adapter for cross-device sync commands.
//! All sync merge, cursor, queue, key and encryption logic lives in `lumen_core`
//! (`sync`, `sync_keys`, `sync_crypto`).

pub use lumen_core::sync::*;
use lumen_core::sync_keys::{self, SyncState};

use crate::commands::ensure_db;
use crate::error::Result;
use tauri::AppHandle;

#[tauri::command]
pub async fn sync_now_cmd(app: AppHandle) -> Result<SyncStatus> {
    let pool = ensure_db(&app).await?;
    sync_now(pool).await
}

#[tauri::command]
pub async fn sync_status_cmd(app: AppHandle) -> Result<SyncStatus> {
    let pool = ensure_db(&app).await?;
    status(pool).await
}

/// Whether sync is on for this device, and which server it uses.
#[tauri::command]
pub async fn sync_state_cmd(app: AppHandle) -> Result<SyncState> {
    let pool = ensure_db(&app).await?;
    sync_keys::state(pool).await
}

/// Sets the sync server address (empty restores the built-in default). Only while sync is off.
#[tauri::command]
pub async fn sync_set_server(url: String, app: AppHandle) -> Result<SyncState> {
    let pool = ensure_db(&app).await?;
    sync_keys::set_server(pool, &url).await
}

/// Turns sync on with a new key and returns the key text, to be shown to the user once.
#[tauri::command]
pub async fn sync_enable(app: AppHandle) -> Result<String> {
    let pool = ensure_db(&app).await?;
    sync_keys::enable(pool).await
}

/// Turns sync on with the key text from another device.
#[tauri::command]
pub async fn sync_join(key: String, app: AppHandle) -> Result<SyncState> {
    let pool = ensure_db(&app).await?;
    sync_keys::join(pool, &key).await?;
    sync_keys::state(pool).await
}

/// The key text of this device's group. Returned only on request; it is never in `sync_state_cmd`.
#[tauri::command]
pub async fn sync_show_key() -> Result<String> {
    sync_keys::show_key()
}

/// Turns sync off here. With `delete_remote`, the synced copy is also erased from the server.
#[tauri::command]
pub async fn sync_disable(delete_remote: bool, app: AppHandle) -> Result<SyncState> {
    let pool = ensure_db(&app).await?;
    sync_keys::disable(pool, delete_remote).await?;
    sync_keys::state(pool).await
}
