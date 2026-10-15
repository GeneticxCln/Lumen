use crate::error::{PlayerError, Result};
use crate::playback::{PlaybackBackend, PlayerStateEvent, PlayerTrack};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as MpvValue;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{timeout, Duration};

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

pub struct MpvIpcBackend {
    socket_path: String,
    mpv_process: std::sync::Mutex<Option<tokio::process::Child>>,
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
}

/// A directory only this user can enter, for mpv's control socket. mpv's IPC accepts commands
/// that run programs, so the socket must not sit where another local user can reach it.
/// `$XDG_RUNTIME_DIR` (mode 0700, per user) is the right place; if it is missing, a private
/// directory is created under the temp dir and checked before use.
fn private_socket_dir() -> Result<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from)
        && runtime.is_dir() {
            return Ok(runtime);
        }
    let dir = std::env::temp_dir().join("lumen-ipc");
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(PlayerError::Mpv(format!("Could not create the player socket directory: {error}"))),
    }
    // A directory made by someone else, or readable by others, is not private: refuse it rather
    // than trust it.
    let meta = std::fs::symlink_metadata(&dir)?;
    let mine = std::fs::metadata("/proc/self")?.uid();
    if !meta.is_dir() || meta.uid() != mine || meta.permissions().mode() & 0o077 != 0 {
        return Err(PlayerError::Mpv(format!(
            "{} is not a private directory owned by you; remove it and try again",
            dir.display()
        )));
    }
    Ok(dir)
}

fn mpv_id(value: &MpvValue) -> Option<String> {
    value.as_i64().map(|id| id.to_string()).or_else(|| value.as_str().map(ToOwned::to_owned))
}

fn mpv_track(track: &MpvValue) -> Option<PlayerTrack> {
    let kind_raw = track.get("type")?.as_str()?;
    let kind = match kind_raw {
        "audio" => "audio",
        "sub" => "subtitle",
        _ => return None,
    };
    let id = mpv_id(track.get("id")?)?;
    let language = track.get("lang").and_then(MpvValue::as_str).filter(|lang| !lang.is_empty()).map(ToOwned::to_owned);
    let title = track.get("title").and_then(MpvValue::as_str).filter(|title| !title.is_empty());
    let codec = track.get("codec").and_then(MpvValue::as_str).filter(|codec| !codec.is_empty());
    let label = title.map(ToOwned::to_owned).or_else(|| language.clone()).or_else(|| codec.map(ToOwned::to_owned)).unwrap_or_else(|| format!("{} track {id}", if kind == "audio" { "Audio" } else { "Subtitle" }));
    Some(PlayerTrack {
        id,
        kind: kind.into(),
        label,
        language,
        selected: track.get("selected").and_then(MpvValue::as_bool).unwrap_or(false),
    })
}

impl MpvIpcBackend {
    pub async fn new(
        app: Option<AppHandle>,
        hwdec: &str,
        cache: &str,
        extra_args: &str,
    ) -> Result<Self> {
        let unique_id = uuid::Uuid::new_v4().simple().to_string();
        let socket_dir = private_socket_dir()?;
        let socket_path = socket_dir
            .join(format!("mpv-{}-{}.sock", std::process::id(), &unique_id[..8]))
            .to_string_lossy()
            .into_owned();

        let _ = std::fs::remove_file(&socket_path);

        let hwdec_arg = format!("--hwdec={}", if hwdec.is_empty() { "auto-safe" } else { hwdec });
        let cache_arg = format!("--cache={}", if cache.is_empty() { "yes" } else { cache });

        let mut mpv_cmd = Command::new("mpv");
        mpv_cmd.args([
            &format!("--input-ipc-server={}", socket_path),
            "--idle=yes",
            "--force-window=yes",
            &hwdec_arg,
            "--vo=gpu-next",
            "--keep-open=yes",
            &cache_arg,
            "--really-quiet",
        ]);

        if !extra_args.trim().is_empty() {
            for arg in extra_args.split_whitespace() {
                mpv_cmd.arg(arg);
            }
        }

        mpv_cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mpv_process = mpv_cmd
            .spawn()
            .map_err(|e| PlayerError::Mpv(format!("Failed to spawn mpv: {}", e)))?;

        // Wait for socket to appear and accept connections
        let mut connected_stream = None;
        for _ in 0..60 {
            if Path::new(&socket_path).exists()
                && let Ok(s) = UnixStream::connect(&socket_path).await {
                    connected_stream = Some(s);
                    break;
                }
            tokio::time::sleep(Duration::from_millis(75)).await;
        }

        let stream = connected_stream.ok_or_else(|| {
            PlayerError::Mpv("mpv IPC socket did not become ready in time".into())
        })?;

        let (reader_half, mut writer_half) = stream.into_split();
        let mut reader = BufReader::new(reader_half);

        let (tx, mut rx) =
            mpsc::unbounded_channel::<(MpvRequest, oneshot::Sender<Result<MpvValue>>)>();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<MpvValue>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let state = Arc::new(Mutex::new(PlayerStateEvent::default()));

        let (load_state_tx, _) = tokio::sync::watch::channel(0_u8);

        let pending_reader = pending.clone();
        let state_reader = state.clone();
        let app_reader = app.clone();
        let load_state_reader = load_state_tx.clone();

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
                    Ok(0) => break,
                    Ok(_) => {
                        if let Ok(msg) = serde_json::from_str::<MpvMessage>(&line) {
                            // 1. Handle command response if request_id matches
                            if let Some(req_id) = msg.request_id
                                && req_id > 0 {
                                    let mut map = pending_reader.lock().await;
                                    if let Some(responder) = map.remove(&req_id) {
                                        let err_str = msg.error.unwrap_or_else(|| "success".into());
                                        if err_str == "success" {
                                            let _ = responder
                                                .send(Ok(msg.data.unwrap_or(MpvValue::Null)));
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
                                                        if last_time_emit.elapsed() >= TIME_EMIT_INTERVAL {
                                                            last_time_emit = std::time::Instant::now();
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
                                                    if let Some(tracks) = msg.data.as_ref().and_then(|value| value.as_array()) {
                                                        st.tracks = tracks.iter().filter_map(mpv_track).collect();
                                                        st.audio_track = tracks.iter().find(|track| track.get("type").and_then(|kind| kind.as_str()) == Some("audio") && track.get("selected").and_then(|value| value.as_bool()) == Some(true)).and_then(|track| track.get("id")).and_then(mpv_id).unwrap_or_else(|| "auto".into());
                                                        st.subtitle_track = tracks.iter().find(|track| track.get("type").and_then(|kind| kind.as_str()) == Some("sub") && track.get("selected").and_then(|value| value.as_bool()) == Some(true)).and_then(|track| track.get("id")).and_then(mpv_id).unwrap_or_else(|| "off".into());
                                                        changed = true;
                                                    }
                                                }
                                                "hls-bitrate" => {
                                                    if let Some(quality) = msg.data.as_ref().and_then(|value| value.as_str()) {
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
                                        if *load_state_reader.borrow() == 1 && msg.reason.as_deref() != Some("redirect") {
                                            let _ = load_state_reader.send_replace(3);
                                        }
                                        if msg.reason.as_deref() == Some("error") {
                                            st.error = Some(
                                                msg.file_error.unwrap_or_else(|| {
                                                    "Playback failed".to_string()
                                                }),
                                            );
                                            st.paused = true;
                                        }
                                        changed = true;
                                    }
                                    _ => {}
                                }

                                if changed
                                    && let Some(ref handle) = app_reader {
                                        let _ = handle.emit("player://state", st.clone());
                                    }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let pending_writer = pending.clone();
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
                    let mut map = pending_writer.lock().await;
                    if let Some(r) = map.remove(&req_id) {
                        let _ = r.send(Err(PlayerError::Mpv("Failed to write to mpv socket".into())));
                    }
                    break;
                }
            }
        });

        let backend = Self {
            socket_path,
            mpv_process: std::sync::Mutex::new(Some(mpv_process)),
            writer: tx,
            request_id: AtomicU64::new(1),
            pending: pending.clone(),
            load_state: load_state_tx,
            closing: AtomicBool::new(false),
            state,
            app,
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

    pub fn is_alive(&self) -> bool {
        if self.closing.load(Ordering::SeqCst) {
            return false;
        }
        let mut process = self.mpv_process.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match process.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Wakes a `load` that is still waiting for mpv, making it fail right away. Used by `stop`
    /// so that pressing Stop on a stream that never loads does not wait out the settle timeout.
    pub fn cancel_load(&self) {
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
    async fn load(&self, url: &str, headers: Option<&[(&str, &str)]>) -> Result<()> {
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
                .set_property("http-header-fields", serde_json::json!(Vec::<String>::new()))
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
                    3 => return Err(PlayerError::Mpv("mpv failed to load the requested media".into())),
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
            Err(_) => return Err(PlayerError::Mpv("Timed out waiting for mpv to load the requested media".into())),
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

    async fn set_property(&self, name: &str, value: MpvValue) -> Result<()> {
        let quality = value.as_str().unwrap_or("max").to_string();
        self.send_command(vec![
            MpvValue::String("set_property".to_string()),
            MpvValue::String(name.to_string()),
            value,
        ])
        .await?;
        if name == "hls-bitrate"
            && let Some(app) = self.app.as_ref() {
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
                let id = track_id.parse::<i64>().map_err(|_| PlayerError::InvalidArg("Invalid track id".into()))?;
                let track_list = self.send_command(vec![MpvValue::String("get_property".into()), MpvValue::String("track-list".into())]).await?;
                let exists = track_list.as_array().is_some_and(|tracks| tracks.iter().any(|track| {
                    track.get("id").and_then(|value| value.as_i64()) == Some(id)
                        && track.get("type").and_then(|value| value.as_str()) == Some(if kind == "audio" { "audio" } else { "sub" })
                }));
                if !exists { return Err(PlayerError::InvalidArg("Track is no longer available".into())); }
                (if kind == "audio" { "aid" } else { "sid" }, MpvValue::Number(id.into()))
            }
            _ => return Err(PlayerError::InvalidArg("Unknown player track kind".into())),
        };
        self.set_property(property, value).await
    }

    async fn set_quality(&self, quality: &str) -> Result<()> {
        if !matches!(quality, "max" | "min") {
            return Err(PlayerError::InvalidArg("Unsupported mpv HLS bitrate preference".into()));
        }
        self.set_property("hls-bitrate", MpvValue::String(quality.into())).await
    }

    async fn stop(&self) -> Result<()> {
        self.cancel_load();
        let _ = self
            .send_command(vec![MpvValue::String("quit".to_string())])
            .await;
        Ok(())
    }
}

impl Drop for MpvIpcBackend {
    fn drop(&mut self) {
        let process = self.mpv_process.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(mut process) = process.take() {
            let _ = process.start_kill();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
        backend.load(&url, None).await.expect("initial load should succeed");
        // Replacing a playing item emits `end-file` for the old entry before the new entry's
        // `start-file`; that old end event must not release the readiness wait early.
        backend.load(&url, None).await.expect("replacement load should succeed");

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
            tokio::spawn(async move { backend.load(&url, None).await })
        };

        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(!loading.is_finished(), "the load should still be waiting on the silent server");

        let started = std::time::Instant::now();
        backend.stop().await.unwrap();
        let outcome = timeout(Duration::from_secs(3), loading)
            .await
            .expect("load must return soon after stop, not after the 30 s settle timeout")
            .expect("load task should not panic");
        assert!(started.elapsed() < Duration::from_secs(3));
        let error = outcome.expect_err("an interrupted load is an error");
        assert!(error.to_string().contains("stopped"), "unexpected error: {error}");
        held.abort();
    }
}
