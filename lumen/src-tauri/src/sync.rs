//! Tauri desktop adapter for cross-device sync commands.
//! All sync merge, cursor, and queue logic lives in `lumen_core::sync`.

pub use lumen_core::sync::*;

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
