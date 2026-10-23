//! Tauri adapter for adding many playlist links at once. The link finding and testing are in
//! `lumen_core::bulk`; this reads the file, reports each result to the page as it arrives, and
//! adds the links the user chose.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use lumen_core::bulk::{self, AddOutcome, AddResult, LinkCheck};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::commands::{allow_lan_sources, ensure_db};
use crate::db::list_playlists;
use crate::error::{PlayerError, Result};

/// The biggest text file read for links.
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// The run in progress, if any: setting its flag stops the links that have not started.
static RUN: LazyLock<Mutex<Option<Arc<AtomicBool>>>> = LazyLock::new(Default::default);

/// Clears `RUN` when a check/add run ends, however it ends. A bare "clear it at the end of the
/// function" relies on every path reaching that one line; an early `?` on a DB error taken after
/// the lock was acquired would otherwise skip it and wedge every later check/add until restart.
struct RunGuard;

impl Drop for RunGuard {
    fn drop(&mut self) {
        *RUN.lock().unwrap() = None;
    }
}

/// What was found in a text.
#[derive(Serialize)]
pub struct FoundLinks {
    pub links: Vec<String>,
    /// Links left out because they are already sources.
    pub already: usize,
}

async fn without_known(app: &AppHandle, found: Vec<String>) -> Result<FoundLinks> {
    let pool = ensure_db(app).await?;
    let known: std::collections::HashSet<String> = list_playlists(pool)
        .await?
        .into_iter()
        .filter_map(|playlist| playlist.origin)
        .collect();
    let total = found.len();
    let links: Vec<String> = found.into_iter().filter(|l| !known.contains(l)).collect();
    Ok(FoundLinks {
        already: total - links.len(),
        links,
    })
}

/// Reads a text file and returns the links in it.
#[tauri::command]
pub async fn bulk_read_file_cmd(path: String, app: AppHandle) -> Result<FoundLinks> {
    let metadata = tokio::fs::metadata(&path).await?;
    if !metadata.is_file() {
        return Err(PlayerError::InvalidArg("That is not a file".into()));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(PlayerError::InvalidArg(
            "That file is too large to be a list of links (over 5 MB)".into(),
        ));
    }
    let bytes = tokio::fs::read(&path).await?;
    without_known(
        &app,
        bulk::extract_links(&lumen_core::text_encoding::decode_text(&bytes)),
    )
    .await
}

/// The links in pasted text.
#[tauri::command]
pub async fn bulk_extract_text_cmd(text: String, app: AppHandle) -> Result<FoundLinks> {
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(PlayerError::InvalidArg("That text is too long".into()));
    }
    without_known(&app, bulk::extract_links(&text)).await
}

/// Tests the links, sending each result to the page as a `bulk://result` event. Returns how many
/// were tested (fewer than given if stopped). Only one run at a time.
#[tauri::command]
pub async fn bulk_check_cmd(urls: Vec<String>, app: AppHandle) -> Result<usize> {
    if urls.len() > bulk::MAX_LINKS {
        return Err(PlayerError::InvalidArg(format!(
            "At most {} links at a time",
            bulk::MAX_LINKS
        )));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut run = RUN.lock().unwrap();
        if run.is_some() {
            return Err(PlayerError::InvalidArg(
                "A check or add is already running".into(),
            ));
        }
        *run = Some(cancel.clone());
    }
    let _run_guard = RunGuard;
    let pool = ensure_db(&app).await;
    let allow_lan = match &pool {
        Ok(pool) => allow_lan_sources(pool).await,
        Err(_) => false,
    };
    let emitter = app.clone();
    let tested = bulk::check_links(urls, allow_lan, cancel, move |check: LinkCheck| {
        let _ = emitter.emit("bulk://result", check);
    })
    .await;
    Ok(tested)
}

/// Stops the links of the current run that have not started.
#[tauri::command]
pub fn bulk_cancel_cmd() {
    if let Some(cancel) = RUN.lock().unwrap().as_ref() {
        cancel.store(true, Ordering::SeqCst);
    }
}

#[derive(Serialize)]
pub struct Added {
    pub url: String,
    pub playlist_id: i64,
}

#[derive(Serialize)]
pub struct NotAdded {
    pub url: String,
    pub reason: String,
}

#[derive(Serialize)]
pub struct BulkAdded {
    pub added: Vec<Added>,
    pub failed: Vec<NotAdded>,
}

/// Adds each link as its own source, a few at a time (like `bulk_check_cmd`), sending each result
/// to the page as a `bulk://add-result` event as soon as it lands instead of leaving the dialog
/// on one opaque "Adding…" for however long the whole batch takes. A link that is already a
/// source is skipped. Only one run (check or add) at a time; `bulk_cancel_cmd` stops links that
/// have not started yet.
#[tauri::command]
pub async fn bulk_add_cmd(urls: Vec<String>, app: AppHandle) -> Result<BulkAdded> {
    if urls.len() > bulk::MAX_LINKS {
        return Err(PlayerError::InvalidArg(format!(
            "At most {} links at a time",
            bulk::MAX_LINKS
        )));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut run = RUN.lock().unwrap();
        if run.is_some() {
            return Err(PlayerError::InvalidArg(
                "A check or add is already running".into(),
            ));
        }
        *run = Some(cancel.clone());
    }
    let _run_guard = RunGuard;

    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    let known: std::collections::HashSet<String> = list_playlists(pool)
        .await?
        .into_iter()
        .filter_map(|playlist| playlist.origin)
        .collect();

    let emitter = app.clone();
    let results = bulk::add_links(pool, urls, allow_lan, &known, cancel, move |result| {
        let _ = emitter.emit("bulk://add-result", result);
    })
    .await;

    let mut result = BulkAdded {
        added: Vec::new(),
        failed: Vec::new(),
    };
    for AddResult { url, outcome } in results {
        match outcome {
            AddOutcome::Added { playlist_id } => result.added.push(Added { url, playlist_id }),
            AddOutcome::Failed { reason } => result.failed.push(NotAdded { url, reason }),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduces the shape of the bug: a `?` taken after `RUN` is set, before the run's own
    /// cleanup line. Without the guard, `RUN` stays `Some(..)` forever past this early return.
    #[test]
    fn run_guard_clears_run_on_an_early_return() {
        fn simulate_early_db_failure() -> Result<()> {
            let cancel = Arc::new(AtomicBool::new(false));
            {
                let mut run = RUN.lock().unwrap();
                assert!(
                    run.is_none(),
                    "a prior test left RUN set; run tests serially"
                );
                *run = Some(cancel.clone());
            }
            let _run_guard = RunGuard;
            Err(PlayerError::InvalidArg("simulated DB failure".into()))?;
            unreachable!("the `?` above always returns early");
        }

        assert!(simulate_early_db_failure().is_err());
        assert!(
            RUN.lock().unwrap().is_none(),
            "RunGuard must clear RUN even when an early `?` skips the run's own cleanup line"
        );
    }
}
