//! Opening things from outside the window: "Open with Lumen", a second launch handed to the
//! running instance, `lumen://` links and dropped files. What each argument means is decided in
//! `lumen_core::launch`; this file only delivers the result to the UI.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use lumen_core::launch::{OpenTarget, classify_arguments};
use tauri::{AppHandle, Emitter, Manager, State};

pub const OPEN_EVENT: &str = "launch://open";

/// Things to open that arrived before the page was listening (the very first launch, or a second
/// launch while the window was still loading). The page takes them once it is ready.
#[derive(Default)]
pub struct PendingLaunch {
    targets: Mutex<Vec<OpenTarget>>,
    page_ready: AtomicBool,
}

impl PendingLaunch {
    fn push(&self, mut targets: Vec<OpenTarget>) {
        self.targets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .append(&mut targets);
    }
}

/// Handles arguments from this launch or from another launch forwarded to this instance.
pub fn handle_arguments(app: &AppHandle, args: &[String], cwd: Option<&Path>) {
    let targets = classify_arguments(args, cwd);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    if targets.is_empty() {
        return;
    }
    eprintln!("lumen: asked to open {} item(s)", targets.len());
    let pending = app.state::<PendingLaunch>();
    if pending.page_ready.load(Ordering::SeqCst) {
        let _ = app.emit(OPEN_EVENT, &targets);
    } else {
        pending.push(targets);
    }
}

/// The page calls this once it is listening: it gets what arrived earlier, and from then on
/// receives new items as `launch://open` events.
#[tauri::command]
pub fn take_launch_targets(pending: State<'_, PendingLaunch>) -> Vec<OpenTarget> {
    pending.page_ready.store(true, Ordering::SeqCst);
    std::mem::take(&mut *pending.targets.lock().unwrap_or_else(|p| p.into_inner()))
}

/// Sorts dropped paths into media, playlists and subtitles the same way a launch does.
#[tauri::command]
pub fn classify_open_targets(paths: Vec<String>) -> Vec<OpenTarget> {
    classify_arguments(&paths, None)
}
