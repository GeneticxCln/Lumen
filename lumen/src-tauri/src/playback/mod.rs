use crate::error::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerTrack {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub language: Option<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerQuality {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerStateEvent {
    pub paused: bool,
    pub time: f64,
    pub duration: f64,
    pub buffering: bool,
    pub title: Option<String>,
    pub error: Option<String>,
    pub tracks: Vec<PlayerTrack>,
    pub audio_track: String,
    pub subtitle_track: String,
    pub quality_levels: Vec<PlayerQuality>,
    pub quality: String,
    pub quality_help: String,
}

impl Default for PlayerStateEvent {
    fn default() -> Self {
        Self {
            paused: true,
            time: 0.0,
            duration: 0.0,
            buffering: false,
            title: None,
            error: None,
            tracks: Vec::new(),
            audio_track: "auto".into(),
            subtitle_track: "off".into(),
            quality_levels: vec![
                PlayerQuality {
                    id: "max".into(),
                    label: "Highest bitrate".into(),
                },
                PlayerQuality {
                    id: "min".into(),
                    label: "Lowest bitrate".into(),
                },
            ],
            quality: "max".into(),
            quality_help: "mpv HLS bitrate preference; the stream determines available resolutions"
                .into(),
        }
    }
}

/// What the app needs from a player. The commands, the desktop media controls and the tests only
/// see this, so nothing outside the mpv module knows it is mpv: a different engine, or the same
/// engine embedded in the window, only has to provide these operations.
#[async_trait]
pub trait PlaybackBackend: Send + Sync {
    async fn load(
        &self,
        url: &str,
        headers: Option<&[(&str, &str)]>,
        audio_url: Option<&str>,
    ) -> Result<()>;
    /// Toggles between playing and paused.
    async fn pause(&self) -> Result<()>;
    /// Jumps to an absolute position in seconds.
    async fn seek(&self, seconds: f64) -> Result<()>;
    /// 0 to 150, 100 being the source level.
    async fn set_volume(&self, vol: i64) -> Result<()>;
    async fn volume(&self) -> Option<f64>;
    /// Chooses an audio or subtitle track (`kind` is "audio" or "subtitle"; "auto"/"off" clear it).
    async fn set_track(&self, kind: &str, track_id: &str) -> Result<()>;
    /// "max" or "min" bitrate preference for adaptive streams.
    async fn set_quality(&self, quality: &str) -> Result<()>;
    /// Applies a profile's audio/subtitle preferences. `reset_delay` also zeroes the per-file subtitle delay.
    async fn apply_prefs(&self, prefs: &lumen_core::player_prefs::PlayerPrefs, reset_delay: bool);
    /// Shifts the current file's subtitles; returns the delay actually applied.
    async fn set_subtitle_delay(&self, seconds: f64) -> Result<f64>;
    /// Sets the playback speed of the current file (1.0 is normal); returns the speed applied.
    async fn set_speed(&self, speed: f64) -> Result<f64>;
    async fn add_subtitle_file(&self, path: &str) -> Result<()>;
    async fn set_fullscreen(&self, enabled: bool) -> Result<()>;
    /// False once the player has exited or is being closed.
    fn is_alive(&self) -> bool;
    async fn stop(&self) -> Result<()>;
}

pub mod mpv_ipc;

#[cfg(target_os = "linux")]
pub mod libmpv;
