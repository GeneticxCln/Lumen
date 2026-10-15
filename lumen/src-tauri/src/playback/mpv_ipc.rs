use crate::error::{PlayerError, Result};
use crate::playback::{PlaybackBackend, PlayerStateEvent};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as MpvValue;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
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
const LOAD_SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct MpvIpcBackend {
    socket_path: String,
    mpv_process: Option<tokio::process::Child>,
    writer: mpsc::UnboundedSender<(MpvRequest, oneshot::Sender<Result<MpvValue>>)>,
    request_id: AtomicU64,
    /// Load lifecycle from the reader loop: 0 = not started, 1 = start-file seen, 2 = loaded,
    /// 3 = failed. Keeping the intermediate state matters when replacing a file: mpv may emit
    /// end-file for the old item before start-file for the new one.
    load_state: tokio::sync::watch::Sender<u8>,
}

impl MpvIpcBackend {
    pub async fn new(
        app: Option<AppHandle>,
        hwdec: &str,
        cache: &str,
        extra_args: &str,
    ) -> Result<Self> {
        let unique_id = uuid::Uuid::new_v4().simple().to_string();
        let socket_path = format!("/tmp/lumen-mpv-{}-{}.sock", std::process::id(), &unique_id[..8]);

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
            if Path::new(&socket_path).exists() {
                if let Ok(s) = UnixStream::connect(&socket_path).await {
                    connected_stream = Some(s);
                    break;
                }
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
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if let Ok(msg) = serde_json::from_str::<MpvMessage>(&line) {
                            // 1. Handle command response if request_id matches
                            if let Some(req_id) = msg.request_id {
                                if req_id > 0 {
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
                                                        changed = true;
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
                                                    if prop_name == "paused-for-cache" {
                                                        if let Some(b) = msg
                                                            .data
                                                            .as_ref()
                                                            .and_then(|v| v.as_bool())
                                                        {
                                                            st.buffering = b;
                                                            changed = true;
                                                        }
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
                                                _ => {}
                                            }
                                        }
                                    }
                                    "start-file" => {
                                        st.error = None;
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
                                        if *load_state_reader.borrow() == 1 {
                                            let failed = msg.reason.as_deref() == Some("error");
                                            let _ = load_state_reader.send_replace(if failed { 3 } else { 2 });
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

                                if changed {
                                    if let Some(ref handle) = app_reader {
                                        let _ = handle.emit("player://state", st.clone());
                                    }
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
            mpv_process: Some(mpv_process),
            writer: tx,
            request_id: AtomicU64::new(1),
            load_state: load_state_tx,
        };

        // Observe standard mpv properties per PLAN.md §3.1
        for (obs_id, prop) in [
            (1_i64, "pause"),
            (2_i64, "time-pos"),
            (3_i64, "duration"),
            (4_i64, "paused-for-cache"),
            (5_i64, "media-title"),
            (6_i64, "track-list"),
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

    pub async fn is_alive(&mut self) -> bool {
        if let Some(ref mut child) = self.mpv_process {
            matches!(child.try_wait(), Ok(None))
        } else {
            false
        }
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
            Err(_) => Err(PlayerError::Mpv("mpv IPC command timed out".into())),
        }
    }
}

#[async_trait]
impl PlaybackBackend for MpvIpcBackend {
    async fn load(&self, url: &str, headers: Option<&[(&str, &str)]>) -> Result<()> {
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
        self.send_command(vec![
            MpvValue::String("set_property".to_string()),
            MpvValue::String(name.to_string()),
            value,
        ])
        .await?;
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let _ = self
            .send_command(vec![MpvValue::String("quit".to_string())])
            .await;
        Ok(())
    }
}

impl Drop for MpvIpcBackend {
    fn drop(&mut self) {
        if let Some(mut process) = self.mpv_process.take() {
            let _ = process.start_kill();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
}
