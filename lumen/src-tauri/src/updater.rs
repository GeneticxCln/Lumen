//! Tauri adapter for updating Lumen from its latest GitHub release. The release logic (version
//! comparison, checksum, safe replacement) is in `lumen_core::update`; this decides what the
//! running copy can do, runs the daily check and tells the page.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lumen_core::update::{self, Available, Kind};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::commands::ensure_db;
use crate::db::{get_setting, set_setting};
use crate::error::{PlayerError, Result};

const MODE_SETTING: &str = "update_mode";
const LAST_CHECK_SETTING: &str = "update_last_check";
const INSTALLED_TAG_SETTING: &str = "update_installed_tag";
const NOTIFIED_TAG_SETTING: &str = "update_notified_tag";

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
/// A check is skipped if the last one was more recent than this.
const MIN_GAP_SECS: i64 = 20 * 3600;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Set while a download+install is in flight, so the manual "Install" button and the background
/// auto-update check can never run `install()` at the same time and race on the same temp file.
static INSTALLING: AtomicBool = AtomicBool::new(false);

/// Clears `INSTALLING` when dropped, so a failed or cancelled install still releases the guard.
#[derive(Debug)]
struct InstallGuard;

impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}

fn start_install() -> Result<InstallGuard> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err(PlayerError::InvalidArg(
            "An update is already downloading or installing".into(),
        ));
    }
    Ok(InstallGuard)
}

/// How Lumen treats a newer release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Download and install it, then ask for a restart.
    Auto,
    /// Only say that it exists.
    Notify,
    /// Never look.
    Off,
}

impl Mode {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "notify" => Some(Self::Notify),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Notify => "notify",
            Self::Off => "off",
        }
    }
}

async fn mode(pool: &sqlx::Pool<sqlx::Sqlite>) -> Mode {
    match get_setting(pool, MODE_SETTING).await {
        Ok(Some(value)) => Mode::parse(&value).unwrap_or(Mode::Auto),
        _ => Mode::Auto,
    }
}

/// The AppImage file this copy is running from, if it is one.
fn appimage_path() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("APPIMAGE")?);
    path.is_file().then_some(path)
}

fn kind() -> Kind {
    if appimage_path().is_some() {
        Kind::AppImage
    } else {
        Kind::Managed
    }
}

/// What the page shows in Settings.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub current: String,
    pub mode: Mode,
    pub kind: Kind,
    /// Seconds since the epoch of the last check, if any.
    pub last_checked: Option<i64>,
    /// A release was installed and waits for a restart.
    pub installed: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct UpdateEvent {
    tag: String,
    page: String,
}

#[tauri::command]
pub async fn update_state_cmd(app: AppHandle) -> Result<UpdateState> {
    let pool = ensure_db(&app).await?;
    Ok(UpdateState {
        current: CURRENT_VERSION.to_string(),
        mode: mode(pool).await,
        kind: kind(),
        last_checked: get_setting(pool, LAST_CHECK_SETTING)
            .await?
            .and_then(|v| v.parse().ok()),
        installed: get_setting(pool, INSTALLED_TAG_SETTING)
            .await?
            .filter(|tag| !tag.is_empty()),
    })
}

#[tauri::command]
pub async fn update_set_mode_cmd(mode: String, app: AppHandle) -> Result<()> {
    let parsed = Mode::parse(&mode)
        .ok_or_else(|| PlayerError::InvalidArg(format!("{mode} is not an update mode")))?;
    let pool = ensure_db(&app).await?;
    set_setting(pool, MODE_SETTING, parsed.as_str()).await
}

async fn look(app: &AppHandle) -> Result<Option<Available>> {
    let pool = ensure_db(app).await?;
    let found = update::check(
        &update::github_guard(),
        update::LATEST_RELEASE_API,
        CURRENT_VERSION,
        std::env::consts::ARCH,
        kind() == Kind::AppImage,
    )
    .await;
    // Remember that a look happened even if it failed, so a broken network is not retried at once.
    let _ = set_setting(
        pool,
        LAST_CHECK_SETTING,
        &chrono::Utc::now().timestamp().to_string(),
    )
    .await;
    found
}

/// Looks for a newer release now (the "Check now" button).
#[tauri::command]
pub async fn update_check_cmd(app: AppHandle) -> Result<Option<Available>> {
    look(&app).await
}

async fn install(app: &AppHandle, available: &Available) -> Result<()> {
    let target = appimage_path().ok_or_else(|| {
        PlayerError::InvalidArg("This copy of Lumen is updated by its package manager".into())
    })?;
    let package = available.package.as_ref().ok_or_else(|| {
        PlayerError::NotFound("This release has no verified download for this machine".into())
    })?;
    let directory = target
        .parent()
        .ok_or_else(|| PlayerError::Io("The program has no folder".into()))?;
    let file = update::download(&update::github_guard(), package, directory).await?;
    update::replace_appimage(&file, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&file);
    })?;
    let pool = ensure_db(app).await?;
    set_setting(pool, INSTALLED_TAG_SETTING, &available.tag).await
}

/// Installs the release a check found (the "Install" button). Returns its tag.
#[tauri::command]
pub async fn update_install_cmd(app: AppHandle) -> Result<String> {
    let available = look(&app)
        .await?
        .ok_or_else(|| PlayerError::NotFound("Lumen is up to date".into()))?;
    let _guard = start_install()?;
    install(&app, &available).await?;
    Ok(available.tag)
}

/// Starts the installed version in place of this one.
#[tauri::command]
pub async fn update_restart_cmd(app: AppHandle) -> Result<()> {
    let target = appimage_path()
        .ok_or_else(|| PlayerError::InvalidArg("There is no installed update to start".into()))?;
    // The new copy starts after this one has gone, or the single-instance guard would hand it
    // over to this one and exit.
    let mut starter = std::process::Command::new("sh");
    lumen_core::child_env::scrub(&mut starter);
    starter
        .arg("-c")
        .arg("sleep 1; exec \"$0\"")
        .arg(target)
        .spawn()
        .map_err(|e| PlayerError::Io(e.to_string()))?;
    app.exit(0);
    Ok(())
}

/// The background check: shortly after start, then every few hours, at most once a day.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            automatic(&app).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

async fn automatic(app: &AppHandle) {
    let Ok(pool) = ensure_db(app).await else {
        return;
    };
    let mode = mode(pool).await;
    if mode == Mode::Off {
        return;
    }
    let last: Option<i64> = get_setting(pool, LAST_CHECK_SETTING)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
    if last.is_some_and(|t| chrono::Utc::now().timestamp() - t < MIN_GAP_SECS) {
        return;
    }
    let Ok(Some(available)) = look(app).await else {
        return;
    };
    let said = |key: &'static str| {
        let tag = available.tag.clone();
        async move { get_setting(pool, key).await.ok().flatten().as_deref() == Some(tag.as_str()) }
    };
    if said(INSTALLED_TAG_SETTING).await {
        return; // Already put in place; waiting for a restart.
    }
    let event = UpdateEvent {
        tag: available.tag.clone(),
        page: available.page.clone(),
    };
    if mode == Mode::Auto
        && kind() == Kind::AppImage
        && available.package.is_some()
        && let Ok(_guard) = start_install()
    {
        match install(app, &available).await {
            Ok(()) => {
                let _ = app.emit("update://ready", event);
                return;
            }
            Err(error) => eprintln!("lumen: could not install the update: {error}"),
        }
    }
    if !said(NOTIFIED_TAG_SETTING).await {
        let _ = set_setting(pool, NOTIFIED_TAG_SETTING, &available.tag).await;
        let _ = app.emit("update://available", event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `INSTALLING` is a single process-wide flag, so this test (like any other touching it) must
    /// not run in parallel with another that does; `#[serial]`-style isolation is unnecessary here
    /// since it is the only test in this crate touching it, but a changed test name or a new test
    /// added later that also calls `start_install` should use `cargo test -- --test-threads=1` or
    /// its own guard if that stops being true.
    #[test]
    fn a_second_install_is_refused_while_one_is_in_progress() {
        let first = start_install().expect("first caller gets the guard");
        let error = start_install().expect_err("a second caller must not also get it");
        assert!(error.to_string().contains("already"));
        drop(first);
        // Dropping the first guard releases the flag for the next caller.
        start_install().expect("the flag is free once the holder is dropped");
    }
}
