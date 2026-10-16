//! Opening things from outside the window: "Open with Lumen", a second launch handed to the
//! running instance, `lumen://` links and dropped files. What each argument means is decided in
//! `lumen_core::launch`; this file only delivers the result to the UI.

use std::path::Path;
use std::sync::Mutex;

use lumen_core::launch::{OpenTarget, classify_arguments};
use tauri::{AppHandle, Emitter, Manager, State};

pub const OPEN_EVENT: &str = "launch://open";

/// Things to open that arrived before the page was listening (the very first launch, or a second
/// launch while the window was still loading). The page takes them once it is ready.
///
/// `targets` and `page_ready` share one lock so a launch deciding whether to queue or emit can
/// never straddle the moment the page marks itself ready and drains the queue: either happens
/// first, never half of each interleaved, so a target can't be queued after the one drain that
/// will ever read the queue.
#[derive(Default)]
struct LaunchState {
    targets: Vec<OpenTarget>,
    page_ready: bool,
}

impl LaunchState {
    /// Decides what happens to newly arrived targets. `Some` means the page is already listening
    /// and the caller must emit them now; `None` means they were queued for the next `take`.
    fn submit(&mut self, targets: Vec<OpenTarget>) -> Option<Vec<OpenTarget>> {
        if self.page_ready {
            Some(targets)
        } else {
            self.targets.extend(targets);
            None
        }
    }

    fn take(&mut self) -> Vec<OpenTarget> {
        self.page_ready = true;
        std::mem::take(&mut self.targets)
    }
}

#[derive(Default)]
pub struct PendingLaunch {
    state: Mutex<LaunchState>,
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
    let to_emit = pending
        .state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .submit(targets);
    if let Some(targets) = to_emit {
        let _ = app.emit(OPEN_EVENT, &targets);
    }
}

/// The page calls this once it is listening: it gets what arrived earlier, and from then on
/// receives new items as `launch://open` events.
#[tauri::command]
pub fn take_launch_targets(pending: State<'_, PendingLaunch>) -> Vec<OpenTarget> {
    pending
        .state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
}

/// Sorts dropped paths into media, playlists and subtitles the same way a launch does.
#[tauri::command]
pub fn classify_open_targets(paths: Vec<String>) -> Vec<OpenTarget> {
    classify_arguments(&paths, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn target(n: usize) -> OpenTarget {
        OpenTarget::Stream {
            url: format!("https://example.com/{n}"),
        }
    }

    #[test]
    fn a_target_arriving_before_take_is_queued_and_returned_by_it() {
        let mut state = LaunchState::default();
        assert_eq!(state.submit(vec![target(1)]), None);
        assert_eq!(state.take(), vec![target(1)]);
    }

    #[test]
    fn a_target_arriving_after_take_is_handed_back_to_emit_at_once() {
        let mut state = LaunchState::default();
        assert_eq!(state.take(), Vec::new());
        assert_eq!(state.submit(vec![target(1)]), Some(vec![target(1)]));
        // Not left sitting in the queue for a `take` that will never come again.
        assert_eq!(state.take(), Vec::new());
    }

    /// Reproduces the race the single lock exists to close: many launches submitting
    /// concurrently with one `take`, interleaved in every order the scheduler allows. Every
    /// submitted target must end up exactly once in either a `take` result or a `submit` that
    /// returned `Some` (the caller's cue to emit it) — never neither, which was the bug (a target
    /// queued after the one `take` that would ever drain it, so it was never delivered at all).
    #[test]
    fn no_target_is_lost_to_a_submit_take_race() {
        const LAUNCHES: usize = 200;
        let state = Arc::new(Mutex::new(LaunchState::default()));
        let taken: Arc<Mutex<Vec<OpenTarget>>> = Arc::new(Mutex::new(Vec::new()));
        let emitted: Arc<Mutex<Vec<OpenTarget>>> = Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for i in 0..LAUNCHES {
            if i == LAUNCHES / 2 {
                let state = state.clone();
                let taken = taken.clone();
                handles.push(thread::spawn(move || {
                    taken.lock().unwrap().extend(state.lock().unwrap().take());
                }));
            }
            let state = state.clone();
            let emitted = emitted.clone();
            handles.push(thread::spawn(move || {
                if let Some(targets) = state.lock().unwrap().submit(vec![target(i)]) {
                    emitted.lock().unwrap().extend(targets);
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        // The one take() that could still be pending never ran: drain whatever is left.
        taken.lock().unwrap().extend(state.lock().unwrap().take());

        let mut accounted_for: Vec<usize> = taken
            .lock()
            .unwrap()
            .iter()
            .chain(emitted.lock().unwrap().iter())
            .map(|t| match t {
                OpenTarget::Stream { url } => url.rsplit('/').next().unwrap().parse().unwrap(),
                _ => unreachable!(),
            })
            .collect();
        accounted_for.sort_unstable();
        accounted_for.dedup();
        assert_eq!(
            accounted_for,
            (0..LAUNCHES).collect::<Vec<_>>(),
            "every submitted target must be delivered exactly once"
        );
    }
}
