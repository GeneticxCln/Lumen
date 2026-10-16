//! The embedded player needs an X11 window, so it only exists on Linux. Other platforms get the
//! same API reporting "not supported", so the rest of the app does not need to care.

use tauri::AppHandle;

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Support {
    pub supported: bool,
    pub reason: Option<String>,
}

pub fn detect_display() {}

pub fn support() -> Support {
    Support {
        supported: false,
        reason: Some("Playing inside the window is only available on Linux.".into()),
    }
}

pub fn in_use() -> bool {
    false
}

pub fn set_in_use(_value: bool) {}

pub async fn ensure_area(_app: &AppHandle) -> Result<u64, String> {
    Err(support().reason.unwrap_or_default())
}

pub fn set_visible(_app: &AppHandle, _visible: bool, _height: i32) -> Result<(), String> {
    Ok(())
}

pub fn mpv_args(_xid: u64) -> String {
    String::new()
}
