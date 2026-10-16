//! Embedded player (experimental, X11 only).
//!
//! mpv can draw into a native window it is handed (`--wid`). Here that window is a GTK drawing
//! area packed above the web page inside Lumen's own window, so playing a video does not open a
//! second window: the video sits on top, the page (title, controls) fills the rest.
//!
//! It needs an X11 window id, so it works when the app runs on X11 or XWayland
//! (`GDK_BACKEND=x11`). A native Wayland window has no such id; there the separate mpv window is
//! used, as before.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use gtk::prelude::*;
use tauri::{AppHandle, Manager};

/// Whether the GDK display is X11. Set once, on the main thread, at startup.
static X11_SESSION: AtomicBool = AtomicBool::new(false);
static VIDEO_XID: OnceLock<u64> = OnceLock::new();
/// Set when mpv was started to draw into the area, so fullscreen toggles the window instead.
static IN_USE: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The area itself, which only the main thread may touch.
    static AREA: RefCell<Option<gtk::DrawingArea>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Support {
    pub supported: bool,
    /// Why not, in words for the settings page.
    pub reason: Option<String>,
}

/// Records what kind of display this is. Call on the main thread (the `setup` hook).
pub fn detect_display() {
    let x11 =
        gtk::gdk::Display::default().is_some_and(|display| display.is::<gdkx11::X11Display>());
    X11_SESSION.store(x11, Ordering::SeqCst);
}

pub fn support() -> Support {
    if X11_SESSION.load(Ordering::SeqCst) {
        Support {
            supported: true,
            reason: None,
        }
    } else {
        Support {
            supported: false,
            reason: Some(
                "Lumen is running as a native Wayland window, which cannot host another program's video. \
                 Start it with GDK_BACKEND=x11 to use this."
                    .into(),
            ),
        }
    }
}

pub fn in_use() -> bool {
    IN_USE.load(Ordering::SeqCst)
}

pub fn set_in_use(value: bool) {
    IN_USE.store(value, Ordering::SeqCst);
}

fn build(window: &tauri::WebviewWindow) -> Result<u64, String> {
    let vbox = window
        .default_vbox()
        .map_err(|error| format!("The window layout is not available: {error}"))?;
    let area = gtk::DrawingArea::new();
    // Hidden and empty until the page asks for room.
    area.set_size_request(-1, 0);
    area.set_no_show_all(true);
    vbox.pack_start(&area, false, false, 0);
    vbox.reorder_child(&area, 0);
    // Realizing creates the X window mpv will draw into.
    area.realize();
    let gdk_window = area.window().ok_or("The video area has no window")?;
    let x11 = gdk_window.downcast::<gdkx11::X11Window>().map_err(|_| {
        "The video area is not an X11 window (run Lumen with GDK_BACKEND=x11)".to_string()
    })?;
    // An X11 window id is `c_ulong`: 64 bits here, 32 on 32-bit systems.
    #[allow(clippy::unnecessary_cast)]
    let xid = x11.xid() as u64;
    AREA.with(|slot| *slot.borrow_mut() = Some(area));
    Ok(xid)
}

/// The X11 window id of the video area, creating the area the first time.
pub async fn ensure_area(app: &AppHandle) -> Result<u64, String> {
    if let Some(xid) = VIDEO_XID.get() {
        return Ok(*xid);
    }
    if !X11_SESSION.load(Ordering::SeqCst) {
        return Err(support().reason.unwrap_or_default());
    }
    let window = app
        .get_webview_window("main")
        .ok_or("The main window is not available")?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let on_main = window.clone();
    window
        .run_on_main_thread(move || {
            let _ = tx.send(build(&on_main));
        })
        .map_err(|error| error.to_string())?;
    let xid = rx
        .await
        .map_err(|_| "The window did not answer".to_string())??;
    let _ = VIDEO_XID.set(xid);
    Ok(xid)
}

/// Shows the area at `height` pixels (full width, above the page), or hides it.
pub fn set_visible(app: &AppHandle, visible: bool, height: i32) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or("The main window is not available")?;
    window
        .run_on_main_thread(move || {
            AREA.with(|slot| {
                if let Some(area) = slot.borrow().as_ref() {
                    if visible && height > 0 {
                        area.set_size_request(-1, height);
                        area.show();
                    } else {
                        area.hide();
                        area.set_size_request(-1, 0);
                    }
                }
            });
        })
        .map_err(|error| error.to_string())
}

/// mpv arguments that make it draw into the area and leave the controls to the page.
pub fn mpv_args(xid: u64) -> String {
    format!(
        "--wid={xid} --osc=no --no-input-default-bindings --input-vo-keyboard=no --input-cursor=no --cursor-autohide=no"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpv_is_pointed_at_the_area_and_its_own_controls_are_off() {
        let args = mpv_args(12345);
        assert!(args.starts_with("--wid=12345 "));
        for flag in [
            "--osc=no",
            "--no-input-default-bindings",
            "--input-vo-keyboard=no",
        ] {
            assert!(args.contains(flag), "{flag}");
        }
    }

    #[test]
    fn a_non_x11_session_explains_itself() {
        X11_SESSION.store(false, Ordering::SeqCst);
        let support = support();
        assert!(!support.supported);
        assert!(support.reason.unwrap().contains("GDK_BACKEND=x11"));
    }
}
