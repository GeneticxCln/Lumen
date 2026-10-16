use crate::error::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as MpvValue;

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
                PlayerQuality { id: "max".into(), label: "Highest bitrate".into() },
                PlayerQuality { id: "min".into(), label: "Lowest bitrate".into() },
            ],
            quality: "max".into(),
            quality_help: "mpv HLS bitrate preference; the stream determines available resolutions".into(),
        }
    }
}

#[async_trait]
pub trait PlaybackBackend: Send + Sync {
    async fn load(&self, url: &str, headers: Option<&[(&str, &str)]>, audio_url: Option<&str>) -> Result<()>;
    async fn pause(&self) -> Result<()>;
    async fn seek(&self, seconds: f64) -> Result<()>;
    async fn set_volume(&self, vol: i64) -> Result<()>;
    async fn set_property(&self, name: &str, value: MpvValue) -> Result<()>;
    async fn stop(&self) -> Result<()>;
}

pub mod mpv_ipc;
