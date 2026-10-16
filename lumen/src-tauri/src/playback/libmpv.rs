//! libmpv, loaded at run time.
//!
//! Playing inside a native Wayland window cannot use mpv's `--wid` (there is no window id to hand
//! over), so there the player runs inside Lumen: libmpv draws into an OpenGL area of the window.
//! The library is opened with `dlopen` when first needed, so Lumen neither needs libmpv to build
//! nor to start; without it the separate mpv window is used. Only the few functions below are used,
//! and the structures are the ones in mpv's `client.h`, `render.h` and `render_gl.h`.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;

unsafe extern "C" {
    fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn setlocale(category: c_int, locale: *const c_char) -> *const c_char;
}

const RTLD_NOW: c_int = 2;
const LC_NUMERIC: c_int = 1;

const RENDER_PARAM_API_TYPE: c_int = 1;
const RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const RENDER_PARAM_OPENGL_FBO: c_int = 3;
const RENDER_PARAM_FLIP_Y: c_int = 4;

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct OpenGlInitParams {
    get_proc_address: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
    ctx: *mut c_void,
}

#[repr(C)]
struct OpenGlFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

type Handle = *mut c_void;
type RenderCtx = *mut c_void;

struct Api {
    create: unsafe extern "C" fn() -> Handle,
    set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    terminate_destroy: unsafe extern "C" fn(Handle),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    render_create: unsafe extern "C" fn(*mut RenderCtx, Handle, *mut RenderParam) -> c_int,
    render: unsafe extern "C" fn(RenderCtx, *mut RenderParam) -> c_int,
    render_free: unsafe extern "C" fn(RenderCtx),
    render_set_update_callback:
        unsafe extern "C" fn(RenderCtx, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void),
    render_update: unsafe extern "C" fn(RenderCtx) -> u64,
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn open_library() -> Result<*mut c_void, String> {
    for name in ["libmpv.so.2", "libmpv.so.1", "libmpv.so"] {
        let name = CString::new(name).expect("no nul");
        let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
        if !handle.is_null() {
            return Ok(handle);
        }
    }
    Err("libmpv is not installed (the package is usually called libmpv2)".into())
}

fn symbol<T: Copy>(handle: *mut c_void, name: &str) -> Result<T, String> {
    let cname = CString::new(name).expect("no nul");
    let found = unsafe { dlsym(handle, cname.as_ptr()) };
    if found.is_null() {
        return Err(format!("this libmpv is too old: {name} is missing"));
    }
    // SAFETY: the symbol is a C function whose signature `T` was written from mpv's headers.
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&found) })
}

fn load() -> Result<Api, String> {
    let handle = open_library()?;
    Ok(Api {
        create: symbol(handle, "mpv_create")?,
        set_option_string: symbol(handle, "mpv_set_option_string")?,
        initialize: symbol(handle, "mpv_initialize")?,
        terminate_destroy: symbol(handle, "mpv_terminate_destroy")?,
        error_string: symbol(handle, "mpv_error_string")?,
        render_create: symbol(handle, "mpv_render_context_create")?,
        render: symbol(handle, "mpv_render_context_render")?,
        render_free: symbol(handle, "mpv_render_context_free")?,
        render_set_update_callback: symbol(handle, "mpv_render_context_set_update_callback")?,
        render_update: symbol(handle, "mpv_render_context_update")?,
    })
}

fn api() -> Result<&'static Api, String> {
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

/// Whether libmpv can be loaded here, and if not, why.
pub fn availability() -> Result<(), String> {
    api().map(|_| ())
}

fn describe(api: &Api, code: c_int) -> String {
    let text = unsafe { (api.error_string)(code) };
    if text.is_null() {
        return format!("error {code}");
    }
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// One mpv option from a command-line style argument: `--name=value`, `--name` (yes) or
/// `--no-name` (no). Anything that is not an option gives `None`.
pub fn parse_option(argument: &str) -> Option<(String, String)> {
    let body = argument.strip_prefix("--")?;
    if body.is_empty() {
        return None;
    }
    if let Some((name, value)) = body.split_once('=') {
        return (!name.is_empty()).then(|| (name.to_string(), value.to_string()));
    }
    match body.strip_prefix("no-") {
        Some("") => None,
        Some(name) => Some((name.to_string(), "no".into())),
        None => Some((body.to_string(), "yes".into())),
    }
}

/// A libmpv player. The client API is thread-safe, so the handle may be used from any thread.
pub struct Player {
    api: &'static Api,
    handle: Handle,
}

// SAFETY: mpv's client API may be called from any thread.
unsafe impl Send for Player {}
unsafe impl Sync for Player {}

impl Player {
    /// Creates a player; options are set before `initialize`.
    pub fn new() -> Result<Self, String> {
        let api = api()?;
        // mpv refuses to start unless numbers are parsed the C way, and GTK sets the user's locale.
        unsafe { setlocale(LC_NUMERIC, c"C".as_ptr()) };
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err("libmpv could not create a player".into());
        }
        Ok(Self { api, handle })
    }

    pub fn set_option(&self, name: &str, value: &str) -> Result<(), String> {
        let (name_c, value_c) = (
            CString::new(name).map_err(|_| "option name contains a NUL")?,
            CString::new(value).map_err(|_| "option value contains a NUL")?,
        );
        let code =
            unsafe { (self.api.set_option_string)(self.handle, name_c.as_ptr(), value_c.as_ptr()) };
        if code < 0 {
            return Err(format!(
                "mpv option {name}={value}: {}",
                describe(self.api, code)
            ));
        }
        Ok(())
    }

    pub fn initialize(&self) -> Result<(), String> {
        let code = unsafe { (self.api.initialize)(self.handle) };
        if code < 0 {
            return Err(format!("mpv did not start: {}", describe(self.api, code)));
        }
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { (self.api.terminate_destroy)(self.handle) };
    }
}

/// Resolves OpenGL functions for libmpv: through EGL (Wayland), else GLX.
unsafe extern "C" fn get_proc_address(_: *mut c_void, name: *const c_char) -> *mut c_void {
    type Lookup = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    for (library, function) in [
        ("libEGL.so.1", "eglGetProcAddress"),
        ("libGL.so.1", "glXGetProcAddressARB"),
    ] {
        let (Ok(library), Ok(function)) = (CString::new(library), CString::new(function)) else {
            continue;
        };
        let handle = unsafe { dlopen(library.as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            continue;
        }
        let lookup = unsafe { dlsym(handle, function.as_ptr()) };
        if lookup.is_null() {
            continue;
        }
        // SAFETY: both functions have this signature.
        let lookup: Lookup = unsafe { std::mem::transmute::<*mut c_void, Lookup>(lookup) };
        let found = unsafe { lookup(name) };
        if !found.is_null() {
            return found;
        }
    }
    std::ptr::null_mut()
}

/// Looks one GL function up by name, for the few the window code needs itself.
pub fn gl_function(name: &str) -> *mut c_void {
    match CString::new(name) {
        Ok(name) => unsafe { get_proc_address(std::ptr::null_mut(), name.as_ptr()) },
        Err(_) => std::ptr::null_mut(),
    }
}

/// libmpv's drawing context for one OpenGL context. Create it, use it and drop it on the thread
/// that owns that context, with the context current.
pub struct RenderContext {
    api: &'static Api,
    ctx: RenderCtx,
    /// The callback libmpv keeps a pointer to; freed after libmpv has let go of it.
    callback: Option<*mut Box<dyn Fn() + Send>>,
}

impl RenderContext {
    pub fn new(player: &Player) -> Result<Self, String> {
        let mut init = OpenGlInitParams {
            get_proc_address: Some(get_proc_address),
            ctx: std::ptr::null_mut(),
        };
        // For this parameter the data is the API name itself, not a pointer to it.
        let mut params = [
            RenderParam {
                kind: RENDER_PARAM_API_TYPE,
                data: c"opengl".as_ptr().cast_mut().cast(),
            },
            RenderParam {
                kind: RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&mut init as *mut OpenGlInitParams).cast(),
            },
            RenderParam {
                kind: 0,
                data: std::ptr::null_mut(),
            },
        ];
        let mut ctx: RenderCtx = std::ptr::null_mut();
        let code =
            unsafe { (player.api.render_create)(&mut ctx, player.handle, params.as_mut_ptr()) };
        if code < 0 {
            return Err(format!(
                "libmpv could not set up drawing: {}",
                describe(player.api, code)
            ));
        }
        Ok(Self {
            api: player.api,
            ctx,
            callback: None,
        })
    }

    /// `callback` runs on a libmpv thread whenever a new frame should be drawn.
    pub fn set_update_callback(&mut self, callback: impl Fn() + Send + 'static) {
        unsafe extern "C" fn trampoline(data: *mut c_void) {
            // SAFETY: `data` is the box made below, alive until the context is freed.
            let callback = unsafe { &*(data as *const Box<dyn Fn() + Send>) };
            callback();
        }
        let boxed: *mut Box<dyn Fn() + Send> = Box::into_raw(Box::new(Box::new(callback)));
        unsafe { (self.api.render_set_update_callback)(self.ctx, Some(trampoline), boxed.cast()) };
        if let Some(old) = self.callback.replace(boxed) {
            drop(unsafe { Box::from_raw(old) });
        }
    }

    /// Draws the current frame into framebuffer `fbo` of the given size (flipped for GTK).
    pub fn draw(&self, fbo: i32, width: i32, height: i32) -> Result<(), String> {
        // libmpv wants to be told a frame was seen before it will produce the next one.
        unsafe { (self.api.render_update)(self.ctx) };
        let mut target = OpenGlFbo {
            fbo,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: c_int = 1;
        let mut params = [
            RenderParam {
                kind: RENDER_PARAM_OPENGL_FBO,
                data: (&mut target as *mut OpenGlFbo).cast(),
            },
            RenderParam {
                kind: RENDER_PARAM_FLIP_Y,
                data: (&mut flip as *mut c_int).cast(),
            },
            RenderParam {
                kind: 0,
                data: std::ptr::null_mut(),
            },
        ];
        let code = unsafe { (self.api.render)(self.ctx, params.as_mut_ptr()) };
        if code < 0 {
            return Err(describe(self.api, code));
        }
        Ok(())
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        unsafe {
            // Stop callbacks first, then free the context, then the callback's memory.
            (self.api.render_set_update_callback)(self.ctx, None, std::ptr::null_mut());
            (self.api.render_free)(self.ctx);
        }
        if let Some(callback) = self.callback.take() {
            drop(unsafe { Box::from_raw(callback) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_style_options_become_name_value_pairs() {
        let parsed = |text: &str| parse_option(text);
        assert_eq!(
            parsed("--demuxer-max-bytes=50MiB"),
            Some(("demuxer-max-bytes".into(), "50MiB".into()))
        );
        assert_eq!(parsed("--no-audio"), Some(("audio".into(), "no".into())));
        assert_eq!(parsed("--mute"), Some(("mute".into(), "yes".into())));
        assert_eq!(
            parsed("--sub-font=DejaVu Sans=Bold"),
            Some(("sub-font".into(), "DejaVu Sans=Bold".into()))
        );
        for not_an_option in ["", "-v", "video.mkv", "--", "--=x", "--no-"] {
            assert_eq!(parsed(not_an_option), None, "{not_an_option:?}");
        }
    }

    #[test]
    fn a_missing_libmpv_is_an_explained_error_not_a_crash() {
        match availability() {
            Ok(()) => eprintln!("libmpv is installed here"),
            Err(reason) => assert!(reason.contains("libmpv"), "{reason}"),
        }
    }

    /// With libmpv installed, a player can be created, configured and initialised without a window.
    #[test]
    fn a_player_starts_and_rejects_unknown_options() {
        if availability().is_err() {
            eprintln!("skipped: libmpv is not installed");
            return;
        }
        let player = Player::new().expect("player");
        player.set_option("vo", "null").expect("known option");
        player.set_option("ao", "null").expect("known option");
        assert!(player.set_option("definitely-not-an-option", "1").is_err());
        player.initialize().expect("initialise");
    }
}
