//! Embedded player (experimental).
//!
//! The video is drawn inside Lumen's own window instead of a second one: an area packed above the
//! web page holds the picture, and the page (title, controls) fills the rest. Two ways to fill it:
//!
//! - **X11 / XWayland**: mpv is handed the area's native window (`--wid`) and draws into it.
//! - **Native Wayland**: a window cannot be handed to another program there, so libmpv runs inside
//!   Lumen and draws into a GTK OpenGL area (`playback::libmpv`), and the usual control socket is
//!   served by that player. It needs libmpv at run time; without it the separate mpv window is used.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use gtk::prelude::*;
use tauri::{AppHandle, Manager};

use crate::playback::libmpv;
use crate::playback::mpv_ipc::{EmbeddedEngine, MpvIpcBackend};

/// Whether the GDK display is X11. Set once, on the main thread, at startup.
static X11_SESSION: AtomicBool = AtomicBool::new(false);
/// How many times libmpv asked for a new frame (`LUMEN_GL_DEBUG` only reports it).
static UPDATES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Whether it is a native Wayland display.
static WAYLAND_SESSION: AtomicBool = AtomicBool::new(false);
static VIDEO_XID: OnceLock<u64> = OnceLock::new();
/// Set when mpv was started to draw into the area, so fullscreen toggles the window instead.
static IN_USE: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The area itself (a drawing area for X11, an OpenGL area for Wayland), which only the main
    /// thread may touch.
    static AREA: RefCell<Option<gtk::Widget>> = const { RefCell::new(None) };
    /// The same area when it is the OpenGL one.
    static GL_AREA: RefCell<Option<gtk::GLArea>> = const { RefCell::new(None) };
    /// What libmpv draws with, while a player lives inside the window.
    static RENDER: RefCell<Option<libmpv::RenderContext>> = const { RefCell::new(None) };
}

/// How the video gets into the window on this display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// mpv draws into a native window (`--wid`).
    Wid,
    /// libmpv runs in the app and draws into an OpenGL area.
    Gl,
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
    let display = gtk::gdk::Display::default();
    let x11 = display
        .as_ref()
        .is_some_and(|display| display.is::<gdkx11::X11Display>());
    let wayland = display
        .as_ref()
        .is_some_and(|display| display.type_().name() == "GdkWaylandDisplay");
    X11_SESSION.store(x11, Ordering::SeqCst);
    WAYLAND_SESSION.store(wayland, Ordering::SeqCst);
}

/// How to embed on this display, or `None` when it cannot be done.
pub fn mode() -> Option<Mode> {
    if X11_SESSION.load(Ordering::SeqCst) {
        Some(Mode::Wid)
    } else if WAYLAND_SESSION.load(Ordering::SeqCst) && libmpv::availability().is_ok() {
        Some(Mode::Gl)
    } else {
        None
    }
}

pub fn support() -> Support {
    if mode().is_some() {
        return Support {
            supported: true,
            reason: None,
        };
    }
    let reason = if WAYLAND_SESSION.load(Ordering::SeqCst) {
        let why = libmpv::availability().err().unwrap_or_default();
        format!(
            "Lumen is running as a native Wayland window. Playing inside it needs libmpv ({why}). \
             Install it, or start Lumen with GDK_BACKEND=x11."
        )
    } else {
        "This kind of display cannot host the video inside the window. Start Lumen with GDK_BACKEND=x11."
            .into()
    };
    Support {
        supported: false,
        reason: Some(reason),
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
    AREA.with(|slot| *slot.borrow_mut() = Some(area.upcast()));
    Ok(xid)
}

/// The X11 window id of the video area, creating the area the first time.
pub async fn ensure_area(app: &AppHandle) -> Result<u64, String> {
    if let Some(xid) = VIDEO_XID.get() {
        return Ok(*xid);
    }
    if mode() != Some(Mode::Wid) {
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

// ---- native Wayland: libmpv inside the window -------------------------------------------------

/// The libmpv options for a player inside the window: the user's own arguments first, then the
/// ones that make it draw here, serve the control socket and leave the controls to the page (so a
/// user argument cannot point it somewhere else).
pub fn engine_options(
    hwdec: &str,
    cache: &str,
    extra_args: &str,
    socket_path: &str,
) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> = vec![("config".into(), "yes".into())];
    options.extend(
        extra_args
            .split_whitespace()
            .filter_map(libmpv::parse_option),
    );
    let fixed = [
        ("vo", "libmpv"),
        ("hwdec", if hwdec.is_empty() { "auto-safe" } else { hwdec }),
        ("cache", if cache.is_empty() { "yes" } else { cache }),
        ("idle", "yes"),
        ("keep-open", "yes"),
        ("sub-auto", "fuzzy"),
        ("sub-file-paths", "subs:Subs:subtitles:Subtitles"),
        ("osc", "no"),
        ("input-default-bindings", "no"),
        ("input-vo-keyboard", "no"),
        ("input-cursor", "no"),
        ("cursor-autohide", "no"),
        ("msg-level", "all=no"),
        ("input-ipc-server", socket_path),
    ];
    options.extend(
        fixed
            .into_iter()
            .map(|(name, value)| (name.to_string(), value.to_string())),
    );
    options
}

/// The player inside the window. Dropping it frees the drawing context first (on the thread that
/// owns the OpenGL context) and then the player.
struct GlEngine {
    player: Option<libmpv::Player>,
    app: AppHandle,
}

impl EmbeddedEngine for GlEngine {}

impl Drop for GlEngine {
    fn drop(&mut self) {
        let Some(player) = self.player.take() else {
            return;
        };
        let released = self.app.run_on_main_thread(move || {
            GL_AREA.with(|slot| {
                if let Some(area) = slot.borrow().as_ref() {
                    area.make_current();
                }
            });
            RENDER.with(|slot| slot.borrow_mut().take());
            drop(player);
        });
        // If the main loop is gone the player cannot be released safely; leaking it is harmless.
        if released.is_err() {
            eprintln!("lumen: the window is closing; the embedded player was left to the system");
        }
    }
}

fn current_framebuffer() -> i32 {
    type GlGetIntegerv = unsafe extern "C" fn(u32, *mut i32);
    static FUNCTION: OnceLock<usize> = OnceLock::new();
    let address = *FUNCTION.get_or_init(|| libmpv::gl_function("glGetIntegerv") as usize);
    if address == 0 {
        return 0;
    }
    let mut fbo = 0i32;
    // SAFETY: `glGetIntegerv` has this signature; 0x8CA6 is GL_FRAMEBUFFER_BINDING.
    unsafe { std::mem::transmute::<usize, GlGetIntegerv>(address)(0x8CA6, &mut fbo) };
    fbo
}

/// Describes the picture in the current framebuffer (its size and how much of it is not black);
/// only for `LUMEN_GL_DEBUG`.
fn probe_picture(fbo: i32, width: i32, height: i32) -> Option<String> {
    type GlBindFramebuffer = unsafe extern "C" fn(u32, u32);
    type GlReadPixels = unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut std::ffi::c_void);
    let address = libmpv::gl_function("glReadPixels") as usize;
    if address == 0 || width <= 0 || height <= 0 {
        return Some(format!("{width}x{height}, nothing to read"));
    }
    let bind = libmpv::gl_function("glBindFramebuffer") as usize;
    if bind != 0 {
        // SAFETY: 0x8CA8 is GL_READ_FRAMEBUFFER.
        unsafe { std::mem::transmute::<usize, GlBindFramebuffer>(bind)(0x8CA8, fbo as u32) };
    }
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: GL_RGBA (0x1908) / GL_UNSIGNED_BYTE (0x1401) into a buffer of width * height * 4 bytes.
    unsafe {
        std::mem::transmute::<usize, GlReadPixels>(address)(
            0,
            0,
            width,
            height,
            0x1908,
            0x1401,
            pixels.as_mut_ptr().cast(),
        )
    };
    let lit = pixels
        .chunks(4)
        .filter(|px| px[0] > 16 || px[1] > 16 || px[2] > 16)
        .count();
    let centre = (height as usize / 2 * width as usize + width as usize / 2) * 4;
    Some(format!(
        "{width}x{height} fbo {fbo}, {:.0}% of the pixels are lit, centre {:?}",
        lit as f64 * 100.0 / (width as f64 * height as f64),
        &pixels[centre..centre + 4]
    ))
}

/// Makes the OpenGL area the first time and returns it. Main thread only.
fn gl_area(window: &tauri::WebviewWindow) -> Result<gtk::GLArea, String> {
    if let Some(area) = GL_AREA.with(|slot| slot.borrow().clone()) {
        return Ok(area);
    }
    let vbox = window
        .default_vbox()
        .map_err(|error| format!("The window layout is not available: {error}"))?;
    let area = gtk::GLArea::new();
    area.set_auto_render(false);
    area.set_size_request(-1, 0);
    area.set_no_show_all(true);
    vbox.pack_start(&area, false, false, 0);
    vbox.reorder_child(&area, 0);
    area.connect_render(|area, _| {
        RENDER.with(|slot| {
            let Some(render) = slot.borrow().as_ref().map(|r| r as *const libmpv::RenderContext) else {
                return;
            };
            let scale = area.scale_factor();
            let (width, height) = (area.allocated_width() * scale, area.allocated_height() * scale);
            // SAFETY: the pointer was just taken from the cell, on this thread, and is used right away.
            let fbo = current_framebuffer();
            if let Err(error) = unsafe { &*render }.draw(fbo, width, height) {
                eprintln!("lumen: drawing the video failed: {error}");
            } else if std::env::var_os("LUMEN_GL_DEBUG").is_some() {
                thread_local!(static FRAMES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) });
                FRAMES.with(|frames| {
                    frames.set(frames.get() + 1);
                    if frames.get() % 25 == 1 {
                        eprintln!(
                            "lumen: frame {} drawn ({} requested by mpv), {}",
                            frames.get(),
                            UPDATES.load(Ordering::Relaxed),
                            probe_picture(fbo, width, height).unwrap_or_default()
                        );
                    }
                });
            }
        });
        gtk::glib::Propagation::Stop
    });
    area.realize();
    area.make_current();
    if let Some(error) = area.error() {
        area.unrealize();
        vbox.remove(&area);
        return Err(format!(
            "OpenGL is not available for the video area: {error}"
        ));
    }
    AREA.with(|slot| *slot.borrow_mut() = Some(area.clone().upcast()));
    GL_AREA.with(|slot| *slot.borrow_mut() = Some(area.clone()));
    Ok(area)
}

/// Starts a player inside the window (native Wayland) and returns the path of its control socket
/// together with the handle that keeps it alive.
pub async fn start_gl_engine(
    app: &AppHandle,
    hwdec: &str,
    cache: &str,
    extra_args: &str,
) -> Result<(String, Box<dyn EmbeddedEngine>), String> {
    if mode() != Some(Mode::Gl) {
        return Err(support().reason.unwrap_or_default());
    }
    let socket_path = MpvIpcBackend::new_socket_path().map_err(String::from)?;
    let options = engine_options(hwdec, cache, extra_args, &socket_path);
    let window = app
        .get_webview_window("main")
        .ok_or("The main window is not available")?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let on_main = window.clone();
    window
        .run_on_main_thread(move || {
            let _ = tx.send(start_on_main(&on_main, &options));
        })
        .map_err(|error| error.to_string())?;
    let player = rx
        .await
        .map_err(|_| "The window did not answer".to_string())??;
    Ok((
        socket_path,
        Box::new(GlEngine {
            player: Some(player),
            app: app.clone(),
        }),
    ))
}

fn start_on_main(
    window: &tauri::WebviewWindow,
    options: &[(String, String)],
) -> Result<libmpv::Player, String> {
    let area = gl_area(window)?;
    area.make_current();
    let player = libmpv::Player::new()?;
    for (name, value) in options {
        // A user argument mpv does not know is skipped; the ones Lumen needs must work.
        if let Err(error) = player.set_option(name, value) {
            eprintln!("lumen: {error}");
        }
    }
    player.initialize()?;
    let mut render = libmpv::RenderContext::new(&player)?;
    let weak: gtk::glib::SendWeakRef<gtk::GLArea> = area.downgrade().into();
    render.set_update_callback(move || {
        UPDATES.fetch_add(1, Ordering::Relaxed);
        let weak = weak.clone();
        gtk::glib::idle_add_once(move || {
            if let Some(area) = weak.upgrade() {
                area.queue_render();
            }
        });
    });
    RENDER.with(|slot| *slot.borrow_mut() = Some(render));
    Ok(player)
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
    fn a_session_that_cannot_embed_explains_itself() {
        X11_SESSION.store(false, Ordering::SeqCst);
        WAYLAND_SESSION.store(false, Ordering::SeqCst);
        let support = support();
        assert!(!support.supported);
        assert!(support.reason.unwrap().contains("GDK_BACKEND=x11"));
        assert_eq!(mode(), None);
    }

    #[test]
    fn wayland_embeds_when_libmpv_is_there_and_says_what_is_missing_when_not() {
        X11_SESSION.store(false, Ordering::SeqCst);
        WAYLAND_SESSION.store(true, Ordering::SeqCst);
        let support = support();
        if libmpv::availability().is_ok() {
            assert!(support.supported);
            assert_eq!(mode(), Some(Mode::Gl));
        } else {
            assert!(!support.supported);
            let reason = support.reason.unwrap();
            assert!(
                reason.contains("libmpv") && reason.contains("GDK_BACKEND=x11"),
                "{reason}"
            );
        }
        WAYLAND_SESSION.store(false, Ordering::SeqCst);
    }

    #[test]
    fn the_in_window_player_keeps_its_own_settings_last() {
        let options = engine_options(
            "vaapi",
            "",
            "--vo=gpu --no-audio --osc=yes --bogus",
            "/run/user/1/mpv.sock",
        );
        let value = |name: &str| {
            options
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        // The user's argument is there, but what Lumen needs wins.
        assert!(options.contains(&("audio".to_string(), "no".to_string())));
        assert_eq!(
            value("vo"),
            Some("libmpv"),
            "a user vo cannot move the picture"
        );
        assert_eq!(value("osc"), Some("no"), "the page draws the controls");
        assert_eq!(value("hwdec"), Some("vaapi"));
        assert_eq!(
            value("cache"),
            Some("yes"),
            "an empty setting means the default"
        );
        assert_eq!(value("input-ipc-server"), Some("/run/user/1/mpv.sock"));
        assert_eq!(value("config"), Some("yes"));
    }
}
