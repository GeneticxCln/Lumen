use crate::error::{PlayerError, Result};
use crate::playback::{PlaybackBackend, PlayerStateEvent, PlayerTrack};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as MpvValue;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{Duration, timeout};

#[derive(Debug, Serialize)]
struct MpvRequest {
    command: Vec<serde_json::Value>,
    request_id: u64,
}

#[derive(Debug, Deserialize)]
struct MpvMessage {
    request_id: Option<u64>,
    error: Option<String>,
    data: Option<MpvValue>,
    event: Option<String>,
    name: Option<String>,
    reason: Option<String>,
    file_error: Option<String>,
}

/// How long `load` waits for mpv to report the file it was handed. Bounded so an unreachable
/// stream cannot hold the command open indefinitely; network streams get a reasonable startup
/// window, and a timeout is surfaced instead of pretending the file is ready for resume-seek.
/// Shortest gap between `player://state` events caused only by the playback clock moving.
const TIME_EMIT_INTERVAL: Duration = Duration::from_millis(250);

const LOAD_SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Shown when Stop interrupts a load that had not finished.
pub const CANCELLED_MESSAGE: &str = "Playback was stopped before the stream finished loading";

/// The last lines mpv wrote to its error output, kept so a failed start can say why.
pub type MpvLog = Arc<std::sync::Mutex<std::collections::VecDeque<String>>>;
const MPV_LOG_LINES: usize = 20;

/// How long a new mpv gets to open its control socket. A first start can be slow (shader cache,
/// hardware decoding probe, audio device), so this is generous; a player that has already exited
/// is noticed at once and does not wait this long.
const SOCKET_WAIT: Duration = Duration::from_secs(20);
const SOCKET_POLL: Duration = Duration::from_millis(75);

/// What keeps the player behind the socket running.
pub enum Engine {
    /// A separate mpv process, which opens its own window (or draws into a window it was given).
    Process(tokio::process::Child),
    /// A libmpv player inside this program, drawing into the app window. Dropping it destroys the player.
    // Held so that dropping it stops the player.
    Embedded(#[allow(dead_code)] Box<dyn EmbeddedEngine>),
}

/// A player living inside this process. Its `Drop` stops it.
pub trait EmbeddedEngine: Send + Sync {}

pub struct MpvIpcBackend {
    socket_path: String,
    engine: std::sync::Mutex<Option<Engine>>,
    writer: mpsc::UnboundedSender<(MpvRequest, oneshot::Sender<Result<MpvValue>>)>,
    request_id: AtomicU64,
    /// Requests waiting for mpv's reply, so a timed-out one can be dropped instead of lingering.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<MpvValue>>>>>,
    /// Load lifecycle from the reader loop: 0 = not started, 1 = start-file seen, 2 = loaded,
    /// 3 = failed, 4 = cancelled. Keeping the intermediate state matters when replacing a file: mpv may emit
    /// end-file for the old item before start-file for the new one.
    load_state: tokio::sync::watch::Sender<u8>,
    /// Set by `cancel_load`/`stop`. Once set, a `load` that is waiting returns an error instead of
    /// holding the caller for the rest of `LOAD_SETTLE_TIMEOUT`, and new loads are refused.
    closing: AtomicBool,
    /// Latest player state shared with the reader loop, so UI-only changes can emit it whole.
    state: Arc<Mutex<PlayerStateEvent>>,
    app: Option<AppHandle>,
    /// Cleared by the reader or writer task the moment the control connection is found dead (EOF,
    /// a read error, or a failed write). For `Engine::Embedded`, this is the only signal that the
    /// in-window engine has died, since there is no child process to `try_wait()` on.
    connection_alive: Arc<AtomicBool>,
}

/// The longest socket file name made in the directory: `/mpv-<pid>-<8 hex>.sock`.
const SOCKET_NAME_ALLOWANCE: usize = 32;
/// A Unix socket path must fit `sun_path` (104 bytes on some systems, 108 on Linux). A directory
/// that would push it over makes mpv fail to bind and the app wait out a timeout, so it is skipped.
const SOCKET_PATH_LIMIT: usize = 104;

fn socket_path_fits(dir: &std::path::Path) -> bool {
    dir.as_os_str().len() + SOCKET_NAME_ALLOWANCE < SOCKET_PATH_LIMIT
}

/// True when `dir` exists, is owned by the current user, and no other user can enter it (mode
/// bits `0o077` all clear). mpv's IPC accepts commands that run programs, so every path that
/// supplies the socket directory must pass this check rather than trust the directory on sight.
fn is_private_dir(dir: &std::path::Path) -> std::io::Result<bool> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let meta = std::fs::symlink_metadata(dir)?;
    let mine = std::fs::metadata("/proc/self")?.uid();
    Ok(meta.is_dir() && meta.uid() == mine && meta.permissions().mode() & 0o077 == 0)
}

/// A directory only this user can enter, for mpv's control socket. `$XDG_RUNTIME_DIR` (mode
/// 0700, per user) is the right place; if it is missing, or not actually private once checked
/// (an inherited value pointing somewhere a misconfigured host made world- or group-enterable),
/// a private directory is created under the temp dir and checked the same way before use.
fn private_socket_dir() -> Result<std::path::PathBuf> {
    use std::os::unix::fs::DirBuilderExt;

    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from)
        && socket_path_fits(&runtime)
        && is_private_dir(&runtime).unwrap_or(false)
    {
        return Ok(runtime);
    }
    let dir = std::env::temp_dir().join("lumen-ipc");
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(PlayerError::Mpv(format!(
                "Could not create the player socket directory: {error}"
            )));
        }
    }
    // A directory made by someone else, or readable by others, is not private: refuse it rather
    // than trust it.
    if !is_private_dir(&dir)? {
        return Err(PlayerError::Mpv(format!(
            "{} is not a private directory owned by you; remove it and try again",
            dir.display()
        )));
    }
    Ok(dir)
}

fn mpv_id(value: &MpvValue) -> Option<String> {
    value
        .as_i64()
        .map(|id| id.to_string())
        .or_else(|| value.as_str().map(ToOwned::to_owned))
}

fn mpv_track(track: &MpvValue) -> Option<PlayerTrack> {
    let kind_raw = track.get("type")?.as_str()?;
    let kind = match kind_raw {
        "audio" => "audio",
        "sub" => "subtitle",
        _ => return None,
    };
    let id = mpv_id(track.get("id")?)?;
    let language = track
        .get("lang")
        .and_then(MpvValue::as_str)
        .filter(|lang| !lang.is_empty())
        .map(ToOwned::to_owned);
    let title = track
        .get("title")
        .and_then(MpvValue::as_str)
        .filter(|title| !title.is_empty());
    let codec = track
        .get("codec")
        .and_then(MpvValue::as_str)
        .filter(|codec| !codec.is_empty());
    let label = title
        .map(ToOwned::to_owned)
        .or_else(|| language.clone())
        .or_else(|| codec.map(ToOwned::to_owned))
        .unwrap_or_else(|| {
            format!(
                "{} track {id}",
                if kind == "audio" { "Audio" } else { "Subtitle" }
            )
        });
    Some(PlayerTrack {
        id,
        kind: kind.into(),
        label,
        language,
        selected: track
            .get("selected")
            .and_then(MpvValue::as_bool)
            .unwrap_or(false),
    })
}

/// Reads mpv's error output into `log` (bounded). With `LUMEN_DEBUG_MPV` set it is also printed.
async fn collect_log(stderr: tokio::process::ChildStderr, log: MpvLog) {
    let echo = std::env::var_os("LUMEN_DEBUG_MPV").is_some();
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if echo {
            eprintln!("mpv: {line}");
        }
        let mut guard = log.lock().unwrap_or_else(|p| p.into_inner());
        if guard.len() == MPV_LOG_LINES {
            guard.pop_front();
        }
        guard.push_back(line);
    }
}

/// The last lines of `log`, as one string for an error message.
fn log_tail(log: Option<&MpvLog>) -> String {
    let Some(log) = log else {
        return String::new();
    };
    let guard = log.lock().unwrap_or_else(|p| p.into_inner());
    let lines: Vec<&str> = guard
        .iter()
        .rev()
        .take(6)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    lines.join("; ")
}

/// Waits for the control socket to accept a connection. A player process that has already
/// exited ends the wait at once, with its exit status and what it printed.
async fn wait_for_socket(
    socket_path: &str,
    engine: &mut Engine,
    log: Option<&MpvLog>,
) -> Result<UnixStream> {
    let deadline = tokio::time::Instant::now() + SOCKET_WAIT;
    loop {
        if Path::new(socket_path).exists()
            && let Ok(stream) = UnixStream::connect(socket_path).await
        {
            return Ok(stream);
        }
        if let Engine::Process(child) = engine
            && let Ok(Some(status)) = child.try_wait()
        {
            // Give the error reader a moment to take the last lines.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let tail = log_tail(log);
            return Err(PlayerError::Mpv(if tail.is_empty() {
                format!("mpv stopped before it was ready ({status})")
            } else {
                format!("mpv stopped before it was ready ({status}): {tail}")
            }));
        }
        if tokio::time::Instant::now() >= deadline {
            let tail = log_tail(log);
            return Err(PlayerError::Mpv(if tail.is_empty() {
                format!(
                    "mpv did not open its control socket within {} seconds",
                    SOCKET_WAIT.as_secs()
                )
            } else {
                format!(
                    "mpv did not open its control socket within {} seconds: {tail}",
                    SOCKET_WAIT.as_secs()
                )
            }));
        }
        tokio::time::sleep(SOCKET_POLL).await;
    }
}

impl MpvIpcBackend {
    /// A fresh control-socket path in a directory only this user can enter.
    pub fn new_socket_path() -> Result<String> {
        let unique_id = uuid::Uuid::new_v4().simple().to_string();
        let socket_dir = private_socket_dir()?;
        let socket_path = socket_dir
            .join(format!(
                "mpv-{}-{}.sock",
                std::process::id(),
                &unique_id[..8]
            ))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_file(&socket_path);
        Ok(socket_path)
    }

    /// Builds the mpv `Command` (arguments only — no stdio/env/spawn, so this is testable without
    /// a real mpv binary). `--network-timeout` and `--stream-lavf-o`'s reconnect options are
    /// libavformat HTTP/HLS settings: a harmless no-op for local files, and placed before
    /// `extra_args` so a user's own arguments can still override them (mpv's own parsing takes
    /// the last occurrence of an option).
    fn build_command(socket_path: &str, hwdec: &str, cache: &str, extra_args: &str) -> Command {
        let hwdec_arg = format!(
            "--hwdec={}",
            if hwdec.is_empty() { "auto-safe" } else { hwdec }
        );
        let cache_arg = format!("--cache={}", if cache.is_empty() { "yes" } else { cache });

        let mut mpv_cmd = Command::new("mpv");
        mpv_cmd.args([
            &format!("--input-ipc-server={}", socket_path),
            "--idle=yes",
            "--force-window=yes",
            &hwdec_arg,
            // The newer renderer first, the older one if this machine cannot run it.
            "--vo=gpu-next,gpu",
            "--keep-open=yes",
            &cache_arg,
            // Subtitles that sit beside a local file are picked up by name ("Movie.srt" for
            // "Movie.mkv"), also from the usual subtitle folders. Streams have no sidecars.
            "--sub-auto=fuzzy",
            "--sub-file-paths=subs:Subs:subtitles:Subtitles",
            // Warnings and errors only, kept (see `MpvLog`) to explain a start that fails.
            "--msg-level=all=warn",
            "--network-timeout=10",
            "--stream-lavf-o=reconnect=1,reconnect_streamed=1,reconnect_delay_max=7,reconnect_at_eof=1",
        ]);

        if !extra_args.trim().is_empty() {
            for arg in extra_args.split_whitespace() {
                mpv_cmd.arg(arg);
            }
        }
        mpv_cmd
    }

    pub async fn new(
        app: Option<AppHandle>,
        hwdec: &str,
        cache: &str,
        extra_args: &str,
    ) -> Result<Self> {
        let socket_path = Self::new_socket_path()?;
        let mut mpv_cmd = Self::build_command(&socket_path, hwdec, cache, extra_args);

        // Inside an AppImage, mpv must not load the AppImage's copies of system libraries.
        lumen_core::child_env::scrub(mpv_cmd.as_std_mut());
        // mpv must reach the network only the way `extra_args` above just told it to (direct, or
        // through the network-privacy relay's `--http-proxy`), never a proxy this desktop session
        // happens to have set in the environment.
        lumen_core::child_env::strip_proxy_env(mpv_cmd.as_std_mut());
        mpv_cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut mpv_process = mpv_cmd.spawn().map_err(|e| {
            PlayerError::Mpv(format!(
                "Failed to start mpv ({e}). Is mpv installed and on the PATH?"
            ))
        })?;
        let log = MpvLog::default();
        if let Some(stderr) = mpv_process.stderr.take() {
            tokio::spawn(collect_log(stderr, log.clone()));
        }

        Self::connect_logged(app, socket_path, Engine::Process(mpv_process), Some(log)).await
    }

    /// Connects to the control socket of a player that is starting (or running) and sets the
    /// backend up around it. `engine` is what keeps that player alive.
    pub async fn connect(
        app: Option<AppHandle>,
        socket_path: String,
        engine: Engine,
    ) -> Result<Self> {
        Self::connect_logged(app, socket_path, engine, None).await
    }

    /// `connect`, with the error output of the player (if it is a process) to explain a failure.
    pub async fn connect_logged(
        app: Option<AppHandle>,
        socket_path: String,
        mut engine: Engine,
        log: Option<MpvLog>,
    ) -> Result<Self> {
        let stream = wait_for_socket(&socket_path, &mut engine, log.as_ref()).await?;

        let (reader_half, mut writer_half) = stream.into_split();
        let mut reader = BufReader::new(reader_half);

        let (tx, mut rx) =
            mpsc::unbounded_channel::<(MpvRequest, oneshot::Sender<Result<MpvValue>>)>();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<MpvValue>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let state = Arc::new(Mutex::new(PlayerStateEvent::default()));

        let (load_state_tx, _) = tokio::sync::watch::channel(0_u8);
        let connection_alive = Arc::new(AtomicBool::new(true));

        let pending_reader = pending.clone();
        let state_reader = state.clone();
        let app_reader = app.clone();
        let load_state_reader = load_state_tx.clone();
        let connection_alive_reader = connection_alive.clone();

        // Spawn reader loop for both command responses and property-change events
        tokio::spawn(async move {
            let mut line = String::new();
            // mpv reports `time-pos` on every frame. The UI needs a clock tick, not a frame rate,
            // so those events are forwarded a few times a second. The state still holds the latest
            // time, and any other event (pause, tracks, ...) carries it along.
            let mut last_time_emit = std::time::Instant::now() - TIME_EMIT_INTERVAL;
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        connection_alive_reader.store(false, Ordering::SeqCst);
                        break;
                    }
                    Ok(_) => {
                        if let Ok(msg) = serde_json::from_str::<MpvMessage>(&line) {
                            // 1. Handle command response if request_id matches
                            if let Some(req_id) = msg.request_id
                                && req_id > 0
                            {
                                let mut map = pending_reader.lock().await;
                                if let Some(responder) = map.remove(&req_id) {
                                    let err_str = msg.error.unwrap_or_else(|| "success".into());
                                    if err_str == "success" {
                                        let _ =
                                            responder.send(Ok(msg.data.unwrap_or(MpvValue::Null)));
                                    } else {
                                        let _ = responder.send(Err(PlayerError::Mpv(err_str)));
                                    }
                                    continue;
                                }
                            }

                            // 2. Handle mpv events & property-change notifications
                            if let Some(ev) = msg.event.as_deref() {
                                let mut st = state_reader.lock().await;
                                let mut changed = false;

                                match ev {
                                    "property-change" => {
                                        if let Some(prop_name) = msg.name.as_deref() {
                                            match prop_name {
                                                "pause" => {
                                                    if let Some(b) =
                                                        msg.data.as_ref().and_then(|v| v.as_bool())
                                                    {
                                                        st.paused = b;
                                                        changed = true;
                                                    }
                                                }
                                                "time-pos" => {
                                                    if let Some(t) =
                                                        msg.data.as_ref().and_then(|v| v.as_f64())
                                                    {
                                                        st.time = t;
                                                        if last_time_emit.elapsed()
                                                            >= TIME_EMIT_INTERVAL
                                                        {
                                                            last_time_emit =
                                                                std::time::Instant::now();
                                                            changed = true;
                                                        }
                                                    }
                                                }
                                                "duration" => {
                                                    if let Some(d) =
                                                        msg.data.as_ref().and_then(|v| v.as_f64())
                                                    {
                                                        st.duration = d;
                                                        changed = true;
                                                    }
                                                }
                                                "paused-for-cache" | "core-idle" => {
                                                    if prop_name == "paused-for-cache"
                                                        && let Some(b) = msg
                                                            .data
                                                            .as_ref()
                                                            .and_then(|v| v.as_bool())
                                                    {
                                                        st.buffering = b;
                                                        changed = true;
                                                    }
                                                }
                                                "media-title" => {
                                                    if let Some(title) =
                                                        msg.data.as_ref().and_then(|v| v.as_str())
                                                    {
                                                        st.title = Some(title.to_string());
                                                        changed = true;
                                                    }
                                                }
                                                "track-list" => {
                                                    if let Some(tracks) = msg
                                                        .data
                                                        .as_ref()
                                                        .and_then(|value| value.as_array())
                                                    {
                                                        st.tracks = tracks
                                                            .iter()
                                                            .filter_map(mpv_track)
                                                            .collect();
                                                        st.audio_track = tracks
                                                            .iter()
                                                            .find(|track| {
                                                                track
                                                                    .get("type")
                                                                    .and_then(|kind| kind.as_str())
                                                                    == Some("audio")
                                                                    && track
                                                                        .get("selected")
                                                                        .and_then(|value| {
                                                                            value.as_bool()
                                                                        })
                                                                        == Some(true)
                                                            })
                                                            .and_then(|track| track.get("id"))
                                                            .and_then(mpv_id)
                                                            .unwrap_or_else(|| "auto".into());
                                                        st.subtitle_track = tracks
                                                            .iter()
                                                            .find(|track| {
                                                                track
                                                                    .get("type")
                                                                    .and_then(|kind| kind.as_str())
                                                                    == Some("sub")
                                                                    && track
                                                                        .get("selected")
                                                                        .and_then(|value| {
                                                                            value.as_bool()
                                                                        })
                                                                        == Some(true)
                                                            })
                                                            .and_then(|track| track.get("id"))
                                                            .and_then(mpv_id)
                                                            .unwrap_or_else(|| "off".into());
                                                        changed = true;
                                                    }
                                                }
                                                "hls-bitrate" => {
                                                    if let Some(quality) = msg
                                                        .data
                                                        .as_ref()
                                                        .and_then(|value| value.as_str())
                                                    {
                                                        st.quality = quality.to_string();
                                                        changed = true;
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    "start-file" => {
                                        st.error = None;
                                        st.tracks.clear();
                                        st.audio_track = "auto".into();
                                        st.subtitle_track = "off".into();
                                        st.buffering = true;
                                        let _ = load_state_reader.send_replace(1);
                                        changed = true;
                                    }
                                    "file-loaded" => {
                                        st.buffering = false;
                                        st.paused = false;
                                        st.error = None;
                                        let _ = load_state_reader.send_replace(2);
                                        changed = true;
                                    }
                                    "end-file" => {
                                        st.buffering = false;
                                        // An end-file before start-file belongs to the previous item
                                        // during replacement; only finish the current load after start.
                                        // `redirect` means mpv is moving on to another entry, so the load is
                                        // still going. Any other end before `file-loaded` means this load
                                        // did not succeed, including Stop and an empty file reaching EOF.
                                        if *load_state_reader.borrow() == 1
                                            && msg.reason.as_deref() != Some("redirect")
                                        {
                                            let _ = load_state_reader.send_replace(3);
                                        }
                                        if msg.reason.as_deref() == Some("error") {
                                            st.error =
                                                Some(msg.file_error.unwrap_or_else(|| {
                                                    "Playback failed".to_string()
                                                }));
                                            st.paused = true;
                                        }
                                        changed = true;
                                    }
                                    _ => {}
                                }

                                if changed && let Some(ref handle) = app_reader {
                                    let _ = handle.emit("player://state", st.clone());
                                }
                            }
                        }
                    }
                    Err(_) => {
                        connection_alive_reader.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });

        let pending_writer = pending.clone();
        let log_writer = log.clone();
        let connection_alive_writer = connection_alive.clone();
        tokio::spawn(async move {
            while let Some((req, responder)) = rx.recv().await {
                let req_id = req.request_id;
                {
                    let mut map = pending_writer.lock().await;
                    map.insert(req_id, responder);
                }

                let data = match serde_json::to_string(&req) {
                    Ok(s) => s + "\n",
                    Err(e) => {
                        let mut map = pending_writer.lock().await;
                        if let Some(r) = map.remove(&req_id) {
                            let _ = r.send(Err(PlayerError::Json(e.to_string())));
                        }
                        continue;
                    }
                };

                if writer_half.write_all(data.as_bytes()).await.is_err() {
                    // The pipe is gone because mpv itself exited; say what it printed before it
                    // did, instead of leaving this as a bare "write failed".
                    let tail = log_tail(log_writer.as_ref());
                    let message = if tail.is_empty() {
                        "mpv stopped responding (its control connection closed)".to_string()
                    } else {
                        format!("mpv stopped responding (its control connection closed): {tail}")
                    };
                    connection_alive_writer.store(false, Ordering::SeqCst);
                    let mut map = pending_writer.lock().await;
                    if let Some(r) = map.remove(&req_id) {
                        let _ = r.send(Err(PlayerError::Mpv(message)));
                    }
                    break;
                }
            }
        });

        let backend = Self {
            socket_path,
            engine: std::sync::Mutex::new(Some(engine)),
            writer: tx,
            request_id: AtomicU64::new(1),
            pending: pending.clone(),
            load_state: load_state_tx,
            closing: AtomicBool::new(false),
            state,
            app,
            connection_alive,
        };

        // Observe standard mpv properties per PLAN.md §3.1
        for (obs_id, prop) in [
            (1_i64, "pause"),
            (2_i64, "time-pos"),
            (3_i64, "duration"),
            (4_i64, "paused-for-cache"),
            (5_i64, "media-title"),
            (6_i64, "track-list"),
            (7_i64, "hls-bitrate"),
        ] {
            let _ = backend
                .send_command(vec![
                    MpvValue::String("observe_property".into()),
                    serde_json::json!(obs_id),
                    MpvValue::String(prop.into()),
                ])
                .await;
        }

        Ok(backend)
    }

    fn is_alive(&self) -> bool {
        if self.closing.load(Ordering::SeqCst) {
            return false;
        }
        let mut engine = self
            .engine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match engine.as_mut() {
            Some(Engine::Process(child)) => matches!(child.try_wait(), Ok(None)),
            // No child process to `try_wait()` on; the reader/writer tasks clearing this flag on
            // EOF, a read error, or a failed write is the only sign the engine has died.
            Some(Engine::Embedded(_)) => self.connection_alive.load(Ordering::SeqCst),
            None => false,
        }
    }

    /// Wakes a `load` that is still waiting for mpv, making it fail right away. Used by `stop`
    /// so that pressing Stop on a stream that never loads does not wait out the settle timeout.
    fn cancel_load(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.load_state.send_replace(4);
    }

    async fn send_command(&self, cmd: Vec<serde_json::Value>) -> Result<MpvValue> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let req = MpvRequest {
            command: cmd,
            request_id: id,
        };
        let (tx, rx) = oneshot::channel();
        self.writer
            .send((req, tx))
            .map_err(|_| PlayerError::Mpv("mpv command channel closed".into()))?;

        match timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(PlayerError::Mpv("mpv response channel closed".into())),
            Err(_) => {
                // No reply is coming for this id; forget it so the map cannot grow over a long session.
                self.pending.lock().await.remove(&id);
                Err(PlayerError::Mpv("mpv IPC command timed out".into()))
            }
        }
    }
}

#[async_trait]
impl PlaybackBackend for MpvIpcBackend {
    async fn load(
        &self,
        url: &str,
        headers: Option<&[(&str, &str)]>,
        audio_url: Option<&str>,
    ) -> Result<()> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(PlayerError::Mpv(CANCELLED_MESSAGE.into()));
        }
        if let Some(hdrs) = headers {
            let header_list: Vec<String> =
                hdrs.iter().map(|(k, v)| format!("{}: {}", k, v)).collect();
            let _ = self
                .set_property("http-header-fields", serde_json::json!(header_list))
                .await;
        } else {
            let _ = self
                .set_property(
                    "http-header-fields",
                    serde_json::json!(Vec::<String>::new()),
                )
                .await;
        }

        // For split audio/video streams from yt-dlp, attach the external audio track via
        // mpv's `audio-files` property (equivalent to `--audio-file`), cleared otherwise.
        if let Some(aud) = audio_url.map(str::trim).filter(|s| !s.is_empty()) {
            let _ = self
                .set_property("audio-files", serde_json::json!(vec![aud]))
                .await;
        } else {
            let _ = self
                .set_property("audio-files", serde_json::json!(Vec::<String>::new()))
                .await;
        }

        // A fresh load starts out unloaded, and this receiver is subscribed before the command
        // goes out, so the readiness signal cannot be missed.
        let mut load_state_rx = self.load_state.subscribe();
        self.load_state.send_replace(0);

        self.send_command(vec![
            MpvValue::String("loadfile".to_string()),
            MpvValue::String(url.to_string()),
            MpvValue::String("replace".to_string()),
        ])
        .await?;

        // `loadfile` only acknowledges that the command was accepted — it returns long before
        // mpv has the file. Any command sent in that window fails with "error running command",
        // which is why the resume `seek` that follows this call used to be dropped silently, and
        // playback restarted from zero. Wait for the file to be loaded (or to fail) first.
        let wait_result = timeout(LOAD_SETTLE_TIMEOUT, async {
            loop {
                if self.closing.load(Ordering::SeqCst) {
                    return Err(PlayerError::Mpv(CANCELLED_MESSAGE.into()));
                }
                match *load_state_rx.borrow_and_update() {
                    2 => return Ok(()),
                    3 => {
                        return Err(PlayerError::Mpv(
                            "mpv failed to load the requested media".into(),
                        ));
                    }
                    _ => {}
                }
                if load_state_rx.changed().await.is_err() {
                    return Err(PlayerError::Mpv("mpv load-state channel closed".into()));
                }
            }
        })
        .await;

        match wait_result {
            Ok(result) => result?,
            Err(_) => {
                return Err(PlayerError::Mpv(
                    "Timed out waiting for mpv to load the requested media".into(),
                ));
            }
        }

        self.set_property("pause", serde_json::json!(false)).await?;
        Ok(())
    }

    async fn pause(&self) -> Result<()> {
        // Cycle pause so calling pause() toggles between play and pause
        self.send_command(vec![
            MpvValue::String("cycle".to_string()),
            MpvValue::String("pause".to_string()),
        ])
        .await?;
        Ok(())
    }

    async fn seek(&self, seconds: f64) -> Result<()> {
        self.send_command(vec![
            MpvValue::String("seek".to_string()),
            serde_json::json!(seconds.max(0.0)),
            MpvValue::String("absolute".to_string()),
        ])
        .await?;
        Ok(())
    }

    async fn set_volume(&self, vol: i64) -> Result<()> {
        self.send_command(vec![
            MpvValue::String("set_property".to_string()),
            MpvValue::String("volume".to_string()),
            serde_json::json!(vol.clamp(0, 150)),
        ])
        .await?;
        Ok(())
    }

    async fn volume(&self) -> Option<f64> {
        MpvIpcBackend::volume(self).await
    }

    async fn set_track(&self, kind: &str, track_id: &str) -> Result<()> {
        MpvIpcBackend::set_track(self, kind, track_id).await
    }

    async fn set_quality(&self, quality: &str) -> Result<()> {
        MpvIpcBackend::set_quality(self, quality).await
    }

    async fn apply_prefs(&self, prefs: &lumen_core::player_prefs::PlayerPrefs, reset_delay: bool) {
        MpvIpcBackend::apply_prefs(self, prefs, reset_delay).await
    }

    async fn set_subtitle_delay(&self, seconds: f64) -> Result<f64> {
        MpvIpcBackend::set_subtitle_delay(self, seconds).await
    }

    async fn set_speed(&self, speed: f64) -> Result<f64> {
        let applied = lumen_core::player_prefs::clamp_speed(speed)?;
        self.set_property("speed", serde_json::json!(applied))
            .await?;
        Ok(applied)
    }

    async fn add_subtitle_file(&self, path: &str) -> Result<()> {
        MpvIpcBackend::add_subtitle_file(self, path).await
    }

    async fn set_fullscreen(&self, enabled: bool) -> Result<()> {
        self.set_property("fullscreen", serde_json::json!(enabled))
            .await
    }

    fn is_alive(&self) -> bool {
        MpvIpcBackend::is_alive(self)
    }

    async fn stop(&self) -> Result<()> {
        self.cancel_load();
        let _ = self
            .send_command(vec![MpvValue::String("quit".to_string())])
            .await;
        Ok(())
    }
}

impl MpvIpcBackend {
    async fn set_property(&self, name: &str, value: MpvValue) -> Result<()> {
        let quality = value.as_str().unwrap_or("max").to_string();
        self.send_command(vec![
            MpvValue::String("set_property".to_string()),
            MpvValue::String(name.to_string()),
            value,
        ])
        .await?;
        if name == "hls-bitrate"
            && let Some(app) = self.app.as_ref()
        {
            let snapshot = {
                let mut st = self.state.lock().await;
                st.quality = quality;
                st.clone()
            };
            let _ = app.emit("player://state", snapshot);
        }
        Ok(())
    }

    async fn set_track(&self, kind: &str, track_id: &str) -> Result<()> {
        let (property, value) = match kind {
            "audio" if track_id == "auto" => ("aid", MpvValue::String("auto".into())),
            "subtitle" if track_id == "off" => ("sid", MpvValue::String("no".into())),
            "audio" | "subtitle" => {
                let id = track_id
                    .parse::<i64>()
                    .map_err(|_| PlayerError::InvalidArg("Invalid track id".into()))?;
                let track_list = self
                    .send_command(vec![
                        MpvValue::String("get_property".into()),
                        MpvValue::String("track-list".into()),
                    ])
                    .await?;
                let exists = track_list.as_array().is_some_and(|tracks| {
                    tracks.iter().any(|track| {
                        track.get("id").and_then(|value| value.as_i64()) == Some(id)
                            && track.get("type").and_then(|value| value.as_str())
                                == Some(if kind == "audio" { "audio" } else { "sub" })
                    })
                });
                if !exists {
                    return Err(PlayerError::InvalidArg(
                        "Track is no longer available".into(),
                    ));
                }
                (
                    if kind == "audio" { "aid" } else { "sid" },
                    MpvValue::Number(id.into()),
                )
            }
            _ => return Err(PlayerError::InvalidArg("Unknown player track kind".into())),
        };
        self.set_property(property, value).await
    }

    async fn set_quality(&self, quality: &str) -> Result<()> {
        if !matches!(quality, "max" | "min") {
            return Err(PlayerError::InvalidArg(
                "Unsupported mpv HLS bitrate preference".into(),
            ));
        }
        self.set_property("hls-bitrate", MpvValue::String(quality.into()))
            .await
    }

    /// Applies a profile's audio and subtitle preferences. Done before each load (with
    /// `reset_delay`, since a subtitle delay belongs to one file) and when the preferences change.
    /// Best effort per property: one that mpv refuses must not stop the rest, or the playback.
    async fn apply_prefs(&self, prefs: &lumen_core::player_prefs::PlayerPrefs, reset_delay: bool) {
        for (name, value) in prefs.mpv_properties(reset_delay) {
            if let Err(error) = self.set_property(name, value).await {
                eprintln!("lumen: could not set {name}: {error}");
            }
        }
    }

    /// mpv's current volume (0 to 150, 100 being unamplified), or `None` if it cannot be read.
    async fn volume(&self) -> Option<f64> {
        self.send_command(vec![
            MpvValue::String("get_property".into()),
            MpvValue::String("volume".into()),
        ])
        .await
        .ok()
        .and_then(|value| value.as_f64())
    }

    /// Shifts the subtitles of the current file by `seconds` (positive shows them later).
    async fn set_subtitle_delay(&self, seconds: f64) -> Result<f64> {
        let delay = lumen_core::player_prefs::clamp_delay(seconds)?;
        self.set_property("sub-delay", serde_json::json!(delay))
            .await?;
        Ok(delay)
    }

    /// Adds a subtitle file the user chose to the current file and shows it.
    async fn add_subtitle_file(&self, path: &str) -> Result<()> {
        let file = lumen_core::player_prefs::validate_subtitle_file(path)?;
        self.send_command(vec![
            MpvValue::String("sub-add".into()),
            MpvValue::String(file.to_string_lossy().into_owned()),
            MpvValue::String("select".into()),
        ])
        .await?;
        Ok(())
    }
}

impl Drop for MpvIpcBackend {
    fn drop(&mut self) {
        let engine = self
            .engine
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match engine.take() {
            Some(Engine::Process(mut process)) => {
                let _ = process.start_kill();
            }
            // Dropping the embedded player stops it.
            Some(Engine::Embedded(_)) | None => {}
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn reconnect_flags_are_present_and_come_before_the_users_own_arguments() {
        let cmd =
            MpvIpcBackend::build_command("/tmp/lumen-test.sock", "", "", "--no-audio --vid=2");
        let args = args_of(&cmd);

        let network_timeout = args.iter().position(|a| a == "--network-timeout=10");
        let reconnect = args.iter().position(|a| {
            a == "--stream-lavf-o=reconnect=1,reconnect_streamed=1,reconnect_delay_max=7,reconnect_at_eof=1"
        });
        let first_user_arg = args.iter().position(|a| a == "--no-audio");

        assert!(
            network_timeout.is_some(),
            "expected --network-timeout, got {args:?}"
        );
        assert!(
            reconnect.is_some(),
            "expected the reconnect stream-lavf-o flag, got {args:?}"
        );
        assert!(
            network_timeout.unwrap() < first_user_arg.unwrap()
                && reconnect.unwrap() < first_user_arg.unwrap(),
            "reconnect flags must come before extra_args, so a user can still override them: {args:?}"
        );
    }

    #[test]
    fn reconnect_flags_are_present_even_with_no_extra_args() {
        let cmd = MpvIpcBackend::build_command("/tmp/lumen-test.sock", "auto-safe", "yes", "");
        let args = args_of(&cmd);
        assert!(args.contains(&"--network-timeout=10".to_string()));
        assert!(
            args.iter()
                .any(|a| a.starts_with("--stream-lavf-o=reconnect=1"))
        );
    }

    #[tokio::test]
    async fn a_player_that_exits_at_once_is_reported_with_what_it_printed() {
        let mut child = Command::new("sh")
            .args(["-c", "echo libfoo.so.1 needs FOO_2 >&2; exit 3"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let log = MpvLog::default();
        tokio::spawn(collect_log(child.stderr.take().unwrap(), log.clone()));
        let mut engine = Engine::Process(child);
        let started = std::time::Instant::now();
        let error = wait_for_socket("/nonexistent/lumen-test.sock", &mut engine, Some(&log))
            .await
            .map(drop)
            .unwrap_err();
        let text = String::from(error);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "gave up at once, not after the full wait"
        );
        assert!(text.contains("stopped before it was ready"), "{text}");
        assert!(text.contains("exit status: 3"), "{text}");
        assert!(
            text.contains("FOO_2"),
            "mpv's own words are in the message: {text}"
        );
    }

    #[tokio::test]
    async fn a_socket_that_appears_late_is_still_found() {
        let dir = std::env::temp_dir().join(format!("lumen-sock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("late.sock").to_string_lossy().into_owned();
        let late = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(700)).await;
            let listener = tokio::net::UnixListener::bind(&late).unwrap();
            let _ = listener.accept().await;
        });
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut engine = Engine::Process(child);
        assert!(wait_for_socket(&path, &mut engine, None).await.is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Accepts one connection on `path`, answers every request on it with `"success"` (as the
    /// setup commands `connect` sends expect), then closes the connection after `alive_for` —
    /// standing in for mpv dying partway through a session rather than at startup.
    async fn accept_respond_then_vanish(path: String, alive_for: Duration) {
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (reader_half, mut writer_half) = stream.into_split();
        let mut reader = BufReader::new(reader_half);
        let respond = async {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let Some(id) = line
                            .split_once("\"request_id\":")
                            .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
                        else {
                            continue;
                        };
                        let reply = format!("{{\"request_id\":{id},\"error\":\"success\"}}\n");
                        if writer_half.write_all(reply.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
            }
        };
        let _ = tokio::time::timeout(alive_for, respond).await;
        // Both halves drop here: the peer's next write hits a closed pipe.
    }

    #[tokio::test]
    async fn a_player_that_dies_mid_session_explains_itself_on_the_next_command() {
        let path = MpvIpcBackend::new_socket_path().unwrap();
        tokio::spawn(accept_respond_then_vanish(
            path.clone(),
            Duration::from_millis(200),
        ));
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let log = MpvLog::default();
        log.lock()
            .unwrap()
            .push_back("[ao/pulse] Context status failed.".to_string());
        // `connect_logged` itself sends and awaits the property-observation setup calls, so by
        // the time it returns the fake mpv has already answered them.
        let backend = MpvIpcBackend::connect_logged(None, path, Engine::Process(child), Some(log))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(400)).await;
        let error = backend.pause().await.unwrap_err();
        let text = String::from(error);
        assert!(text.contains("stopped responding"), "{text}");
        assert!(
            text.contains("Context status failed"),
            "mpv's own words are kept: {text}"
        );
    }

    #[tokio::test]
    async fn the_same_failure_without_a_log_still_says_the_connection_closed() {
        let path = MpvIpcBackend::new_socket_path().unwrap();
        tokio::spawn(accept_respond_then_vanish(
            path.clone(),
            Duration::from_millis(200),
        ));
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let backend = MpvIpcBackend::connect(None, path, Engine::Process(child))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(400)).await;
        let error = backend.pause().await.unwrap_err();
        assert_eq!(
            String::from(error),
            "mpv error: mpv stopped responding (its control connection closed)"
        );
    }

    /// A stand-in for the real in-window libmpv engine: nothing to embed, just something that
    /// implements the marker trait so `Engine::Embedded` can be constructed in a test.
    struct FakeEmbeddedEngine;
    impl EmbeddedEngine for FakeEmbeddedEngine {}

    #[tokio::test]
    async fn a_dead_embedded_engine_is_no_longer_reported_alive() {
        let path = MpvIpcBackend::new_socket_path().unwrap();
        tokio::spawn(accept_respond_then_vanish(
            path.clone(),
            Duration::from_millis(200),
        ));
        let backend =
            MpvIpcBackend::connect(None, path, Engine::Embedded(Box::new(FakeEmbeddedEngine)))
                .await
                .unwrap();
        assert!(
            backend.is_alive(),
            "freshly connected, nothing has died yet"
        );

        tokio::time::sleep(Duration::from_millis(400)).await;
        // Nothing to `try_wait()` on for an embedded engine; only the reader task noticing the
        // fake mpv closed the connection (the same EOF a real crash would produce) can tell.
        assert!(
            !backend.is_alive(),
            "the control connection closed, so a stale backend must not be reused"
        );
    }

    use std::io::Write;
    use std::path::PathBuf;

    #[test]
    fn a_runtime_directory_too_long_for_a_unix_socket_is_not_used() {
        assert!(socket_path_fits(std::path::Path::new("/run/user/1000")));
        let long = format!("/tmp/{}", "x".repeat(90));
        assert!(!socket_path_fits(std::path::Path::new(&long)));
    }

    /// These tests drive a real mpv. On a machine without it (a minimal CI image) they skip, with
    /// a message, rather than failing for a reason unrelated to the code under test.
    fn mpv_installed() -> bool {
        let found = std::process::Command::new("mpv")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !found {
            eprintln!("skipped: mpv is not installed");
        }
        found
    }

    /// A PCM WAV of the requested length, written by hand so the test needs no encoder.
    fn write_wav(path: &Path, seconds: u32) -> std::io::Result<()> {
        const RATE: u32 = 8000;
        let data_len = RATE * seconds * 2;
        let mut file = std::fs::File::create(path)?;
        file.write_all(b"RIFF")?;
        file.write_all(&(36 + data_len).to_le_bytes())?;
        file.write_all(b"WAVEfmt ")?;
        file.write_all(&16u32.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&RATE.to_le_bytes())?;
        file.write_all(&(RATE * 2).to_le_bytes())?;
        file.write_all(&2u16.to_le_bytes())?;
        file.write_all(&16u16.to_le_bytes())?;
        file.write_all(b"data")?;
        file.write_all(&data_len.to_le_bytes())?;
        file.write_all(&vec![0u8; data_len as usize])
    }

    /// The resume path, exercised against the real player: `load` must hand control back with
    /// the file actually loaded, so the `seek` that follows lands instead of being rejected.
    /// This is the regression that made "resume where you left off" silently start from zero.
    #[tokio::test]
    async fn test_load_waits_for_the_file_so_a_resume_seek_lands() {
        if !mpv_installed() {
            return;
        }
        let path = std::env::temp_dir().join(format!(
            "lumen-playback-{}.wav",
            uuid::Uuid::new_v4().simple()
        ));
        write_wav(&path, 15).expect("the fixture should be writable");

        // Null video and audio outputs keep this headless; appended options win over the ones
        // the constructor sets first.
        let backend = MpvIpcBackend::new(None, "no", "no", "--vo=null --ao=null --force-window=no")
            .await
            .expect("mpv should start and expose its IPC socket");

        let url = path.to_string_lossy().to_string();
        backend
            .load(&url, None, None)
            .await
            .expect("initial load should succeed");
        // Replacing a playing item emits `end-file` for the old entry before the new entry's
        // `start-file`; that old end event must not release the readiness wait early.
        backend
            .load(&url, None, None)
            .await
            .expect("replacement load should succeed");

        // `play_url` seeks to the saved position as soon as `load` returns.
        backend
            .seek(5.0)
            .await
            .expect("a seek issued right after load must be accepted");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut position = 0.0_f64;
        while std::time::Instant::now() < deadline {
            let query = backend
                .send_command(vec![
                    MpvValue::String("get_property".to_string()),
                    MpvValue::String("time-pos".to_string()),
                ])
                .await;
            if let Ok(value) = query {
                position = value.as_f64().unwrap_or(position);
                if position > 4.5 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        assert!(
            position > 4.5,
            "playback must continue from the resume point, not restart at zero (time-pos {position})"
        );

        let _ = backend.stop().await;
        let _ = std::fs::remove_file(&path);
    }

    /// Stop must not wait out `LOAD_SETTLE_TIMEOUT` when a stream never answers. The server below
    /// accepts the connection and then says nothing, which is what a dead IPTV host looks like.
    #[tokio::test]
    async fn test_stop_interrupts_a_load_that_never_finishes() {
        if !mpv_installed() {
            return;
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                open.push(stream); // keep the socket open, send nothing
            }
        });

        let backend = Arc::new(
            MpvIpcBackend::new(None, "no", "no", "--vo=null --ao=null --force-window=no")
                .await
                .expect("mpv should start"),
        );
        let url = format!("http://127.0.0.1:{port}/never.m3u8");
        let loading = {
            let backend = backend.clone();
            tokio::spawn(async move { backend.load(&url, None, None).await })
        };

        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(
            !loading.is_finished(),
            "the load should still be waiting on the silent server"
        );

        let started = std::time::Instant::now();
        backend.stop().await.unwrap();
        let outcome = timeout(Duration::from_secs(3), loading)
            .await
            .expect("load must return soon after stop, not after the 30 s settle timeout")
            .expect("load task should not panic");
        assert!(started.elapsed() < Duration::from_secs(3));
        let error = outcome.expect_err("an interrupted load is an error");
        assert!(
            error.to_string().contains("stopped"),
            "unexpected error: {error}"
        );
        held.abort();
    }

    fn ffmpeg_installed() -> bool {
        let found = std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !found {
            eprintln!("skipped: ffmpeg is not installed");
        }
        found
    }

    fn headless_backend_args() -> &'static str {
        "--vo=null --ao=null --force-window=no"
    }

    async fn property(backend: &MpvIpcBackend, name: &str) -> serde_json::Value {
        backend
            .send_command(vec![
                MpvValue::String("get_property".into()),
                MpvValue::String(name.into()),
            ])
            .await
            .unwrap_or(serde_json::Value::Null)
    }

    /// A 3-second video with two audio tracks (English, Japanese) and two subtitle tracks
    /// (English, Spanish), plus a sidecar `.srt` beside it.
    fn write_multilingual_fixture(dir: &Path) -> PathBuf {
        let srt = |text: &str| format!("1\n00:00:00,500 --> 00:00:02,500\n{text}\n");
        std::fs::write(dir.join("en.srt"), srt("Hello")).unwrap();
        std::fs::write(dir.join("es.srt"), srt("Hola")).unwrap();
        std::fs::write(dir.join("episode.srt"), srt("Sidecar")).unwrap();
        let out = dir.join("episode.mkv");
        let status = std::process::Command::new("ffmpeg")
            .current_dir(dir)
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x64:d=3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:duration=3",
                "-i",
                "en.srt",
                "-i",
                "es.srt",
                "-map",
                "0",
                "-map",
                "1",
                "-map",
                "2",
                "-map",
                "3",
                "-map",
                "4",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
                "-c:s",
                "srt",
                "-metadata:s:a:0",
                "language=eng",
                "-metadata:s:a:1",
                "language=jpn",
                "-metadata:s:s:0",
                "language=eng",
                "-metadata:s:s:1",
                "language=spa",
                "-shortest",
            ])
            .arg(&out)
            .status()
            .expect("ffmpeg should run");
        assert!(status.success(), "fixture encode failed");
        out
    }

    /// Preferences really change what mpv selects: audio language, subtitle language, hidden
    /// subtitles, a sidecar file next to the video, a file added by hand, and a per-file delay.
    #[tokio::test]
    async fn test_audio_and_subtitle_preferences_drive_real_track_selection() {
        if !mpv_installed() || !ffmpeg_installed() {
            return;
        }
        use lumen_core::player_prefs::PlayerPrefs;
        let dir =
            std::env::temp_dir().join(format!("lumen-subs-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = write_multilingual_fixture(&dir);
        let url = video.to_string_lossy().to_string();
        let backend = MpvIpcBackend::new(None, "no", "no", headless_backend_args())
            .await
            .expect("mpv starts");

        // Default: mpv's own choice (first audio track).
        backend.apply_prefs(&PlayerPrefs::default(), true).await;
        backend.load(&url, None, None).await.expect("load");
        assert_eq!(
            property(&backend, "aid").await,
            serde_json::json!(1),
            "first audio track by default"
        );

        // The sidecar file beside the video was found by name and selected: a file the user put next
        // to the video wins over embedded tracks (mpv's rule, kept on purpose).
        let tracks = property(&backend, "track-list").await;
        let sidecar = tracks
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["type"] == "sub" && t["external"] == true);
        assert!(
            sidecar.is_some_and(|t| t["selected"] == true),
            "episode.srt beside episode.mkv is loaded and selected: {tracks}"
        );

        // Japanese audio and Spanish subtitles, on a copy with no sidecar so the embedded tracks decide.
        let plain_dir = dir.join("plain");
        std::fs::create_dir_all(&plain_dir).unwrap();
        let plain = plain_dir.join("episode.mkv");
        std::fs::copy(&video, &plain).unwrap();
        let plain_url = plain.to_string_lossy().to_string();
        let prefs = PlayerPrefs {
            audio_lang: "ja".into(),
            subtitles: "es".into(),
            ..Default::default()
        };
        backend.apply_prefs(&prefs, true).await;
        backend.load(&plain_url, None, None).await.expect("load");
        assert_eq!(
            property(&backend, "aid").await,
            serde_json::json!(2),
            "Japanese audio is the second track"
        );
        let sid = property(&backend, "sid")
            .await
            .as_i64()
            .expect("a subtitle track is selected");
        let tracks = property(&backend, "track-list").await;
        let selected = tracks
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["type"] == "sub" && t["id"] == sid)
            .expect("selected sub exists");
        assert_eq!(
            selected["lang"], "spa",
            "the Spanish subtitle is chosen: {selected}"
        );
        assert_eq!(
            property(&backend, "sub-visibility").await,
            serde_json::json!(true)
        );

        // English subtitles instead.
        backend
            .apply_prefs(
                &PlayerPrefs {
                    subtitles: "en".into(),
                    ..Default::default()
                },
                true,
            )
            .await;
        backend.load(&plain_url, None, None).await.expect("load");
        let sid = property(&backend, "sid")
            .await
            .as_i64()
            .expect("a subtitle track is selected");
        let tracks = property(&backend, "track-list").await;
        let selected = tracks
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["type"] == "sub" && t["id"] == sid)
            .unwrap();
        assert_eq!(selected["lang"], "eng");

        // Subtitles off hides them; size and position are applied.
        let off = PlayerPrefs {
            subtitles: "off".into(),
            sub_scale: 1.5,
            sub_pos: 90,
            ..Default::default()
        };
        backend.apply_prefs(&off, true).await;
        assert_eq!(
            property(&backend, "sub-visibility").await,
            serde_json::json!(false)
        );
        assert_eq!(property(&backend, "sub-scale").await.as_f64(), Some(1.5));
        assert_eq!(property(&backend, "sub-pos").await.as_f64(), Some(90.0));

        // A delay applies to this file and is gone on the next load.
        assert_eq!(backend.set_subtitle_delay(1.26).await.unwrap(), 1.3);
        assert_eq!(property(&backend, "sub-delay").await.as_f64(), Some(1.3));
        backend.apply_prefs(&PlayerPrefs::default(), true).await;
        backend.load(&url, None, None).await.expect("load");
        assert_eq!(
            property(&backend, "sub-delay").await.as_f64(),
            Some(0.0),
            "delay does not carry to the next file"
        );

        // A subtitle file added by hand becomes a selected track; junk is refused before mpv sees it.
        let before = property(&backend, "track-list")
            .await
            .as_array()
            .unwrap()
            .len();
        let extra = dir.join("extra.srt");
        std::fs::write(&extra, "1\n00:00:00,500 --> 00:00:01,500\nExtra\n").unwrap();
        backend
            .add_subtitle_file(extra.to_str().unwrap())
            .await
            .expect("subtitle added");
        let after = property(&backend, "track-list")
            .await
            .as_array()
            .unwrap()
            .len();
        assert_eq!(after, before + 1, "one track was added");
        assert!(backend.add_subtitle_file("/etc/passwd").await.is_err());

        // Subtitle style reaches mpv, and speed is set live and starts at 1 on the next file.
        let styled = PlayerPrefs {
            sub_color: "#FFEE00".into(),
            sub_bold: true,
            sub_back_color: "#000000".into(),
            ..Default::default()
        };
        backend.apply_prefs(&styled, true).await;
        assert_eq!(
            property(&backend, "sub-color")
                .await
                .as_str()
                .map(str::to_ascii_uppercase),
            Some("#FFFFEE00".to_string()),
            "mpv reports colours with their alpha"
        );
        assert_eq!(
            property(&backend, "sub-bold").await,
            serde_json::json!(true)
        );
        assert_eq!(
            PlaybackBackend::set_speed(&backend, 1.52).await.unwrap(),
            1.5
        );
        assert_eq!(property(&backend, "speed").await.as_f64(), Some(1.5));
        backend.apply_prefs(&PlayerPrefs::default(), true).await;
        assert_eq!(property(&backend, "speed").await.as_f64(), Some(1.0));

        let _ = backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A subtitle fetched from a service is stored by the core and loaded by mpv as a real track,
    /// in each format the store can produce.
    #[tokio::test]
    async fn test_stored_downloaded_subtitles_load_as_tracks() {
        if !mpv_installed() {
            return;
        }
        use lumen_core::subtitles::{SubtitleCandidate, store};
        let dir =
            std::env::temp_dir().join(format!("lumen-fetched-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("tone.wav");
        write_wav(&media, 15).expect("fixture");
        let backend = MpvIpcBackend::new(None, "no", "no", headless_backend_args())
            .await
            .expect("mpv starts");
        backend
            .load(&media.to_string_lossy(), None, None)
            .await
            .expect("load");

        let candidate = |id: &str| SubtitleCandidate {
            provider: "test".into(),
            id: id.into(),
            language: "en".into(),
            release: String::new(),
            hearing_impaired: false,
            downloads: None,
            fps: None,
            hash_match: false,
            score: 0,
            url: String::new(),
        };
        let texts = [
            ("srt", "1\n00:00:01,000 --> 00:00:02,000\nHello\n"),
            ("vtt", "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n"),
            (
                "ass",
                "[Script Info]\nScriptType: v4.00+\n\n[Events]\nFormat: Layer, Start, End, Style, Text\nDialogue: 0,0:00:01.00,0:00:02.00,Default,Hello\n",
            ),
        ];
        for (extension, text) in texts {
            let before = property(&backend, "track-list")
                .await
                .as_array()
                .unwrap()
                .len();
            let path = store::save(&dir, "stream:test", &candidate(extension), text).unwrap();
            assert_eq!(path.extension().unwrap(), extension);
            backend
                .add_subtitle_file(&path.to_string_lossy())
                .await
                .unwrap_or_else(|error| panic!("{extension} subtitle refused: {error}"));
            let after = property(&backend, "track-list")
                .await
                .as_array()
                .unwrap()
                .len();
            assert_eq!(after, before + 1, "{extension} became a track");
        }

        let _ = backend.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
