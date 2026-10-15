use crate::error::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as MpvValue;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerStateEvent {
    pub paused: bool,
    pub time: f64,
    pub duration: f64,
    pub buffering: bool,
    pub title: Option<String>,
    pub error: Option<String>,
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
        }
    }
}

#[async_trait]
pub trait PlaybackBackend: Send + Sync {
    async fn load(&self, url: &str, headers: Option<&[(&str, &str)]>) -> Result<()>;
    async fn pause(&self) -> Result<()>;
    async fn seek(&self, seconds: f64) -> Result<()>;
    async fn set_volume(&self, vol: i64) -> Result<()>;
    async fn set_property(&self, name: &str, value: MpvValue) -> Result<()>;
    async fn stop(&self) -> Result<()>;
}

pub mod mpv_ipc;
