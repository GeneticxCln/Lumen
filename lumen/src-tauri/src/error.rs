use thiserror::Error;
use serde::Serialize;

#[derive(Error, Debug, Serialize)]
pub enum PlayerError {
    #[error("mpv error: {0}")]
    Mpv(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("JSON error: {0}")]
    Json(String),

    #[error("SQL error: {0}")]
    Sql(String),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("Sidecar error: {0}")]
    Sidecar(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid argument: {0}")]
    InvalidArg(String),
}

impl From<std::io::Error> for PlayerError {
    fn from(e: std::io::Error) -> Self {
        PlayerError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for PlayerError {
    fn from(e: serde_json::Error) -> Self {
        PlayerError::Json(e.to_string())
    }
}

impl From<sqlx::Error> for PlayerError {
    fn from(e: sqlx::Error) -> Self {
        PlayerError::Sql(e.to_string())
    }
}

impl From<reqwest::Error> for PlayerError {
    fn from(e: reqwest::Error) -> Self {
        PlayerError::Http(e.to_string())
    }
}

impl From<PlayerError> for String {
    fn from(e: PlayerError) -> Self {
        e.to_string()
    }
}

pub type Result<T, E = PlayerError> = std::result::Result<T, E>;