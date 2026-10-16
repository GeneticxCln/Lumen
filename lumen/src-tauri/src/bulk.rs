//! Tauri adapter for adding many playlist links at once. The link finding and testing are in
//! `lumen_core::bulk`; this reads the file, reports each result to the page as it arrives, and
//! adds the links the user chose.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use lumen_core::bulk::{self, LinkCheck};
use lumen_core::playlist::import::import_m3u_url;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::commands::{allow_lan_sources, ensure_db};
use crate::db::{import_playlist, list_playlists};
use crate::error::{PlayerError, Result};

/// The biggest text file read for links.
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// The run in progress, if any: setting its flag stops the links that have not started.
static RUN: LazyLock<Mutex<Option<Arc<AtomicBool>>>> = LazyLock::new(Default::default);

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
            return Err(PlayerError::InvalidArg("A check is already running".into()));
        }
        *run = Some(cancel.clone());
    }
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
    *RUN.lock().unwrap() = None;
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

/// Adds each link as its own source. A link that is already a source is skipped.
#[tauri::command]
pub async fn bulk_add_cmd(urls: Vec<String>, app: AppHandle) -> Result<BulkAdded> {
    if urls.len() > bulk::MAX_LINKS {
        return Err(PlayerError::InvalidArg(format!(
            "At most {} links at a time",
            bulk::MAX_LINKS
        )));
    }
    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    let mut known: std::collections::HashSet<String> = list_playlists(pool)
        .await?
        .into_iter()
        .filter_map(|playlist| playlist.origin)
        .collect();
    let mut result = BulkAdded {
        added: Vec::new(),
        failed: Vec::new(),
    };
    for url in urls {
        if known.contains(&url) {
            continue;
        }
        let name = Some(bulk::name_from_link(&url));
        let outcome = async {
            let (playlist, channels) = import_m3u_url(&url, name, allow_lan).await?;
            import_playlist(pool, &playlist, &channels).await
        }
        .await;
        match outcome {
            Ok(playlist_id) => {
                known.insert(url.clone());
                result.added.push(Added { url, playlist_id });
            }
            Err(error) => result.failed.push(NotAdded {
                url,
                reason: String::from(error),
            }),
        }
    }
    Ok(result)
}
