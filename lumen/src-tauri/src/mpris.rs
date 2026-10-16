//! MPRIS: lets the desktop see and control playback (media keys, the GNOME/KDE media widget,
//! `playerctl`). The protocol side is generic over `Controls` so it can be tested against a real
//! D-Bus session without a window; the app supplies the mpv-backed implementation.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mpris_server::zbus::fdo;
use mpris_server::{
    LoopStatus, Metadata, PlaybackRate, PlaybackStatus, PlayerInterface, Property, RootInterface,
    Server, Time, TrackId, Volume,
};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Listener, Manager};
use tokio::sync::mpsc;

/// The D-Bus name is `org.mpris.MediaPlayer2.<this>`.
pub const BUS_NAME_SUFFIX: &str = "lumen";
const TRACK_ID: &str = "/dev/lumen/track/current";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Stopped,
    Paused,
    Playing,
}

/// What the player is doing right now, as far as the desktop needs to know.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub status: Status,
    pub title: Option<String>,
    pub length_secs: f64,
    pub position_secs: f64,
}

/// Reads a `player://state` event. Nothing loaded (no title and no duration) is "stopped".
pub fn snapshot_from_event(payload: &Value) -> Snapshot {
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let length_secs = payload
        .get("duration")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .max(0.0);
    let position_secs = payload
        .get("time")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .max(0.0);
    let paused = payload
        .get("paused")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let status = if title.is_none() && length_secs <= 0.0 {
        Status::Stopped
    } else if paused {
        Status::Paused
    } else {
        Status::Playing
    };
    Snapshot {
        status,
        title,
        length_secs,
        position_secs,
    }
}

fn playback_status(status: Status) -> PlaybackStatus {
    match status {
        Status::Stopped => PlaybackStatus::Stopped,
        Status::Paused => PlaybackStatus::Paused,
        Status::Playing => PlaybackStatus::Playing,
    }
}

fn metadata_of(snapshot: &Snapshot) -> Metadata {
    let mut metadata = Metadata::builder()
        .trackid(TrackId::try_from(TRACK_ID).expect("a valid object path"))
        .build();
    if snapshot.status == Status::Stopped {
        return metadata;
    }
    metadata.set_title(snapshot.title.clone());
    if snapshot.length_secs > 0.0 {
        metadata.set_length(Some(Time::from_micros(
            (snapshot.length_secs * 1_000_000.0) as i64,
        )));
    }
    metadata
}

/// The properties that differ between two snapshots. Position is not one of them: the spec has
/// clients read it when they need it, and it changes several times a second.
fn changes(old: &Snapshot, new: &Snapshot) -> Vec<Property> {
    let mut changed = Vec::new();
    if old.status != new.status {
        changed.push(Property::PlaybackStatus(playback_status(new.status)));
        let active = new.status != Status::Stopped;
        changed.push(Property::CanPlay(active));
        changed.push(Property::CanPause(active));
        changed.push(Property::CanSeek(active));
    }
    if old.title != new.title
        || (old.length_secs - new.length_secs).abs() > 0.5
        || old.status != new.status
    {
        changed.push(Property::Metadata(metadata_of(new)));
    }
    changed
}

/// What the desktop can ask the app to do.
#[async_trait]
pub trait Controls: Send + Sync + 'static {
    async fn toggle_pause(&self) -> Result<(), String>;
    async fn seek_to(&self, seconds: f64) -> Result<(), String>;
    /// 1.0 is mpv's normal level.
    async fn volume(&self) -> Option<f64>;
    async fn set_volume(&self, volume: f64) -> Result<(), String>;
    /// Bring the window to the front.
    fn raise(&self);
    /// Stop and close the player, like the Stop button (so progress is saved).
    fn stop(&self);
}

struct LumenMpris<C: Controls> {
    controls: Arc<C>,
    snapshot: Arc<Mutex<Snapshot>>,
    /// zbus runs these methods on its own executor, outside tokio; control calls need tokio.
    runtime: tokio::runtime::Handle,
}

impl<C: Controls> LumenMpris<C> {
    fn now(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn failed(message: String) -> fdo::Error {
        fdo::Error::Failed(message)
    }

    /// Runs one control call on its own task and waits for it. The MPRIS traits need futures that
    /// are `Sync`; the boxed futures of `Controls` are only `Send`, and a join handle is both.
    async fn call<T, F, Fut>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(Arc<C>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        let controls = self.controls.clone();
        self.runtime
            .spawn(async move { f(controls).await })
            .await
            .map_err(|error| error.to_string())?
    }
}

impl<C: Controls> RootInterface for LumenMpris<C> {
    async fn raise(&self) -> fdo::Result<()> {
        self.controls.raise();
        Ok(())
    }
    async fn quit(&self) -> fdo::Result<()> {
        Ok(()) // closing the app from a media widget is not offered
    }
    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn set_fullscreen(&self, _fullscreen: bool) -> mpris_server::zbus::Result<()> {
        Ok(())
    }
    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(true)
    }
    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn identity(&self) -> fdo::Result<String> {
        Ok("Lumen".into())
    }
    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok("lumen".into())
    }
    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(vec!["http".into(), "https".into(), "file".into()])
    }
    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

impl<C: Controls> PlayerInterface for LumenMpris<C> {
    async fn next(&self) -> fdo::Result<()> {
        Ok(())
    }
    async fn previous(&self) -> fdo::Result<()> {
        Ok(())
    }
    async fn pause(&self) -> fdo::Result<()> {
        if self.now().status == Status::Playing {
            self.call(|c| async move { c.toggle_pause().await })
                .await
                .map_err(Self::failed)?;
        }
        Ok(())
    }
    async fn play_pause(&self) -> fdo::Result<()> {
        if self.now().status != Status::Stopped {
            self.call(|c| async move { c.toggle_pause().await })
                .await
                .map_err(Self::failed)?;
        }
        Ok(())
    }
    async fn stop(&self) -> fdo::Result<()> {
        self.controls.stop();
        Ok(())
    }
    async fn play(&self) -> fdo::Result<()> {
        if self.now().status == Status::Paused {
            self.call(|c| async move { c.toggle_pause().await })
                .await
                .map_err(Self::failed)?;
        }
        Ok(())
    }
    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        let current = self.now();
        if current.status == Status::Stopped {
            return Ok(());
        }
        let target = (current.position_secs + offset.as_micros() as f64 / 1_000_000.0).max(0.0);
        let target = if current.length_secs > 0.0 {
            target.min(current.length_secs)
        } else {
            target
        };
        self.call(move |c| async move { c.seek_to(target).await })
            .await
            .map_err(Self::failed)
    }
    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        // A position for a track that is no longer the current one must be ignored (MPRIS spec).
        if track_id.as_str() != TRACK_ID || self.now().status == Status::Stopped {
            return Ok(());
        }
        let target = (position.as_micros() as f64 / 1_000_000.0).max(0.0);
        self.call(move |c| async move { c.seek_to(target).await })
            .await
            .map_err(Self::failed)
    }
    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Ok(()) // opening addresses goes through the app's own confirmation, not a media widget
    }
    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(playback_status(self.now().status))
    }
    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        Ok(LoopStatus::None)
    }
    async fn set_loop_status(&self, _loop_status: LoopStatus) -> mpris_server::zbus::Result<()> {
        Ok(())
    }
    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }
    async fn set_rate(&self, _rate: PlaybackRate) -> mpris_server::zbus::Result<()> {
        Ok(())
    }
    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn set_shuffle(&self, _shuffle: bool) -> mpris_server::zbus::Result<()> {
        Ok(())
    }
    async fn metadata(&self) -> fdo::Result<Metadata> {
        Ok(metadata_of(&self.now()))
    }
    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(self
            .call(|c| async move { Ok(c.volume().await) })
            .await
            .ok()
            .flatten()
            .unwrap_or(1.0))
    }
    async fn set_volume(&self, volume: Volume) -> mpris_server::zbus::Result<()> {
        if !volume.is_finite() {
            return Ok(());
        }
        let level = volume.clamp(0.0, 1.5);
        self.call(move |c| async move { c.set_volume(level).await })
            .await
            .map_err(|m| fdo::Error::Failed(m).into())
    }
    async fn position(&self) -> fdo::Result<Time> {
        Ok(Time::from_micros(
            (self.now().position_secs * 1_000_000.0) as i64,
        ))
    }
    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }
    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }
    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn can_play(&self) -> fdo::Result<bool> {
        Ok(self.now().status != Status::Stopped)
    }
    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(self.now().status != Status::Stopped)
    }
    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(self.now().status != Status::Stopped)
    }
    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }
}

/// Serves MPRIS until `updates` closes, publishing property changes as snapshots arrive.
pub async fn run<C: Controls>(
    controls: C,
    mut updates: mpsc::UnboundedReceiver<Snapshot>,
) -> Result<(), String> {
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));
    let server = Server::new(
        BUS_NAME_SUFFIX,
        LumenMpris {
            controls: Arc::new(controls),
            snapshot: snapshot.clone(),
            runtime: tokio::runtime::Handle::current(),
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let mut last = Snapshot::default();
    while let Some(next) = updates.recv().await {
        let changed = changes(&last, &next);
        *snapshot.lock().unwrap_or_else(|p| p.into_inner()) = next.clone();
        if !changed.is_empty()
            && let Err(error) = server.properties_changed(changed).await
        {
            eprintln!("lumen: MPRIS update failed: {error}");
        }
        last = next;
    }
    Ok(())
}

/// The app's controls: mpv for playback, the window for raise, the page for stop.
struct AppControls {
    app: AppHandle,
}

#[async_trait]
impl Controls for AppControls {
    async fn toggle_pause(&self) -> Result<(), String> {
        let backend = crate::commands::current_backend()
            .await
            .map_err(String::from)?;
        backend.pause().await.map_err(String::from)
    }
    async fn seek_to(&self, seconds: f64) -> Result<(), String> {
        let backend = crate::commands::current_backend()
            .await
            .map_err(String::from)?;
        backend.seek(seconds).await.map_err(String::from)
    }
    async fn volume(&self) -> Option<f64> {
        let backend = crate::commands::current_backend().await.ok()?;
        backend.volume().await.map(|v| v / 100.0)
    }
    async fn set_volume(&self, volume: f64) -> Result<(), String> {
        let backend = crate::commands::current_backend()
            .await
            .map_err(String::from)?;
        backend
            .set_volume((volume * 100.0).round() as i64)
            .await
            .map_err(String::from)
    }
    fn raise(&self) {
        if let Some(window) = self.app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
    fn stop(&self) {
        // The page owns Stop (it saves progress and closes the player), so ask it to.
        let _ = self.app.emit("mpris://stop", ());
    }
}

/// Starts MPRIS for the running app. A session without a D-Bus bus simply has no media keys.
pub fn start(app: AppHandle) {
    let (tx, rx) = mpsc::unbounded_channel();
    app.listen("player://state", move |event| {
        if let Ok(payload) = serde_json::from_str::<Value>(event.payload()) {
            let _ = tx.send(snapshot_from_event(&payload));
        }
    });
    tauri::async_runtime::spawn(async move {
        if let Err(error) = run(AppControls { app }, rx).await {
            eprintln!("lumen: media keys unavailable (MPRIS): {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn player_events_become_mpris_snapshots() {
        let playing = snapshot_from_event(
            &json!({"title": "Heat", "duration": 7200.0, "time": 12.5, "paused": false}),
        );
        assert_eq!(
            playing,
            Snapshot {
                status: Status::Playing,
                title: Some("Heat".into()),
                length_secs: 7200.0,
                position_secs: 12.5
            }
        );
        assert_eq!(
            snapshot_from_event(&json!({"title": "Heat", "duration": 7200.0, "paused": true}))
                .status,
            Status::Paused
        );
        assert_eq!(
            snapshot_from_event(&json!({"title": null, "duration": 0.0, "paused": true})).status,
            Status::Stopped,
            "nothing loaded"
        );
        assert_eq!(snapshot_from_event(&json!({})).status, Status::Stopped);
        // A stream still buffering has a title but no duration yet: it is not "stopped".
        assert_eq!(
            snapshot_from_event(&json!({"title": "Live", "duration": 0.0, "paused": false})).status,
            Status::Playing
        );
        assert_eq!(
            snapshot_from_event(
                &json!({"title": "x", "duration": -5.0, "time": -1.0, "paused": false})
            )
            .length_secs,
            0.0
        );
    }

    #[test]
    fn only_real_changes_are_published_and_position_is_not_one() {
        let playing = Snapshot {
            status: Status::Playing,
            title: Some("Heat".into()),
            length_secs: 100.0,
            position_secs: 1.0,
        };
        assert!(
            changes(
                &playing,
                &Snapshot {
                    position_secs: 50.0,
                    ..playing.clone()
                }
            )
            .is_empty(),
            "a tick of the clock is not a change"
        );
        let paused = Snapshot {
            status: Status::Paused,
            ..playing.clone()
        };
        let changed = changes(&playing, &paused);
        assert!(
            changed
                .iter()
                .any(|p| matches!(p, Property::PlaybackStatus(PlaybackStatus::Paused)))
        );
        let renamed = Snapshot {
            title: Some("Ronin".into()),
            ..playing.clone()
        };
        assert!(
            changes(&playing, &renamed)
                .iter()
                .any(|p| matches!(p, Property::Metadata(_)))
        );
        let stopped = Snapshot::default();
        assert!(
            changes(&playing, &stopped)
                .iter()
                .any(|p| matches!(p, Property::CanPlay(false)))
        );
    }

    #[test]
    fn metadata_carries_the_title_and_length_only_while_something_is_loaded() {
        let loaded = metadata_of(&Snapshot {
            status: Status::Playing,
            title: Some("Heat".into()),
            length_secs: 90.0,
            position_secs: 0.0,
        });
        assert_eq!(loaded.title(), Some("Heat"));
        assert_eq!(loaded.length(), Some(Time::from_secs(90)));
        assert_eq!(loaded.trackid().unwrap().as_str(), TRACK_ID);
        let idle = metadata_of(&Snapshot::default());
        assert_eq!(idle.title(), None);
        assert_eq!(idle.trackid().unwrap().as_str(), TRACK_ID);
    }

    #[derive(Default, Clone)]
    struct Recorder {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Recorder {
        fn log(&self, entry: String) {
            self.calls.lock().unwrap().push(entry);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Controls for Recorder {
        async fn toggle_pause(&self) -> Result<(), String> {
            self.log("toggle".into());
            Ok(())
        }
        async fn seek_to(&self, seconds: f64) -> Result<(), String> {
            self.log(format!("seek {seconds:.1}"));
            Ok(())
        }
        async fn volume(&self) -> Option<f64> {
            Some(0.8)
        }
        async fn set_volume(&self, volume: f64) -> Result<(), String> {
            self.log(format!("volume {volume:.2}"));
            Ok(())
        }
        fn raise(&self) {
            self.log("raise".into());
        }
        fn stop(&self) {
            self.log("stop".into());
        }
    }

    async fn busctl(address: &str, args: &[&str]) -> String {
        let output = tokio::process::Command::new("busctl")
            .arg(format!("--address={address}"))
            .arg("--json=short")
            .arg("--") // so a negative number is an argument, not an option
            .args(args)
            .output()
            .await
            .expect("busctl runs");
        assert!(
            output.status.success(),
            "busctl {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    const DEST: &str = "org.mpris.MediaPlayer2.lumen";
    const PATH: &str = "/org/mpris/MediaPlayer2";
    const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

    async fn property(address: &str, interface: &str, name: &str) -> Value {
        let out = busctl(address, &["get-property", DEST, PATH, interface, name]).await;
        serde_json::from_str::<Value>(&out).expect("busctl prints JSON")["data"].clone()
    }

    /// The protocol against a real D-Bus session, read and driven by an independent client
    /// (`busctl`), the way a desktop's media keys and widgets would.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn desktop_clients_can_read_and_control_playback_over_a_real_dbus_session() {
        let tools = ["dbus-daemon", "busctl"].iter().all(|tool| {
            std::process::Command::new(tool)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        });
        if !tools {
            eprintln!("skipped: dbus-daemon or busctl is not installed");
            return;
        }
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut bus = tokio::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("a private session bus starts");
        let mut lines = BufReader::new(bus.stdout.take().unwrap()).lines();
        let address = lines
            .next_line()
            .await
            .unwrap()
            .expect("the bus prints its address");
        // The server connects to "the session bus"; point it at the private one. Nothing else in
        // this test binary reads this variable.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };

        let recorder = Recorder::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(run(recorder.clone(), rx));
        // Wait for the name to appear on the bus.
        let mut up = false;
        for _ in 0..50 {
            let out = tokio::process::Command::new("busctl")
                .arg(format!("--address={address}"))
                .args(["list", "--no-pager"])
                .output()
                .await
                .unwrap();
            if String::from_utf8_lossy(&out.stdout).contains(DEST) {
                up = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(up, "the player registers {DEST}");
        use std::time::Duration;

        // Identity, and that nothing is playing yet.
        assert_eq!(
            property(&address, "org.mpris.MediaPlayer2", "Identity").await,
            "Lumen"
        );
        assert_eq!(
            property(&address, "org.mpris.MediaPlayer2", "DesktopEntry").await,
            "lumen"
        );
        assert_eq!(
            property(&address, PLAYER, "PlaybackStatus").await,
            "Stopped"
        );
        assert_eq!(property(&address, PLAYER, "CanPlay").await, false);

        // A movie starts playing: status, title, length and position are visible to the client.
        tx.send(Snapshot {
            status: Status::Playing,
            title: Some("Heat".into()),
            length_secs: 7200.0,
            position_secs: 100.0,
        })
        .unwrap();
        for _ in 0..50 {
            if property(&address, PLAYER, "PlaybackStatus").await == "Playing" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            property(&address, PLAYER, "PlaybackStatus").await,
            "Playing"
        );
        assert_eq!(property(&address, PLAYER, "CanPause").await, true);
        let metadata = property(&address, PLAYER, "Metadata").await;
        assert_eq!(metadata["xesam:title"]["data"], "Heat", "{metadata}");
        assert_eq!(
            metadata["mpris:length"]["data"], 7_200_000_000_i64,
            "{metadata}"
        );
        assert_eq!(metadata["mpris:trackid"]["data"], TRACK_ID);
        assert_eq!(
            property(&address, PLAYER, "Position").await,
            100_000_000_i64
        );
        assert_eq!(property(&address, PLAYER, "Volume").await, 0.8);

        // Media keys: PlayPause toggles; Play does nothing while playing; Pause toggles once.
        busctl(&address, &["call", DEST, PATH, PLAYER, "PlayPause"]).await;
        busctl(&address, &["call", DEST, PATH, PLAYER, "Play"]).await;
        busctl(&address, &["call", DEST, PATH, PLAYER, "Pause"]).await;
        assert_eq!(
            recorder.calls(),
            vec!["toggle", "toggle"],
            "PlayPause and Pause act; Play on a playing title does not"
        );

        // Seeking: relative from the current position, absolute for SetPosition, never past the end or before zero.
        busctl(
            &address,
            &["call", DEST, PATH, PLAYER, "Seek", "x", "30000000"],
        )
        .await;
        busctl(
            &address,
            &["call", DEST, PATH, PLAYER, "Seek", "x", "-500000000"],
        )
        .await;
        busctl(
            &address,
            &[
                "call",
                DEST,
                PATH,
                PLAYER,
                "SetPosition",
                "ox",
                TRACK_ID,
                "42000000",
            ],
        )
        .await;
        busctl(
            &address,
            &[
                "call",
                DEST,
                PATH,
                PLAYER,
                "SetPosition",
                "ox",
                "/dev/lumen/track/old",
                "9000000",
            ],
        )
        .await;
        busctl(
            &address,
            &["set-property", DEST, PATH, PLAYER, "Volume", "d", "0.5"],
        )
        .await;
        busctl(&address, &["call", DEST, PATH, PLAYER, "Stop"]).await;
        busctl(
            &address,
            &[
                "call",
                DEST,
                "/org/mpris/MediaPlayer2",
                "org.mpris.MediaPlayer2",
                "Raise",
            ],
        )
        .await;
        assert_eq!(
            recorder.calls()[2..],
            [
                "seek 130.0",
                "seek 0.0",
                "seek 42.0",
                "volume 0.50",
                "stop",
                "raise"
            ],
            "a position for an old track is ignored"
        );

        // Pausing from the app is reflected for the clients, and the title going away stops it.
        tx.send(Snapshot {
            status: Status::Paused,
            title: Some("Heat".into()),
            length_secs: 7200.0,
            position_secs: 130.0,
        })
        .unwrap();
        for _ in 0..50 {
            if property(&address, PLAYER, "PlaybackStatus").await == "Paused" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(property(&address, PLAYER, "PlaybackStatus").await, "Paused");
        busctl(&address, &["call", DEST, PATH, PLAYER, "Play"]).await;
        assert_eq!(
            recorder.calls().last().map(String::as_str),
            Some("toggle"),
            "Play resumes a paused title"
        );
        tx.send(Snapshot::default()).unwrap();
        for _ in 0..50 {
            if property(&address, PLAYER, "PlaybackStatus").await == "Stopped" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            property(&address, PLAYER, "PlaybackStatus").await,
            "Stopped"
        );
        assert_eq!(property(&address, PLAYER, "CanSeek").await, false);

        drop(tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
        let _ = bus.kill().await;
    }
}
