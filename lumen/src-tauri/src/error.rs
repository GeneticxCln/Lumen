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

/// reqwest's error text includes the full request URL. That URL can carry a secret (a TMDB v3
/// key travels as `?api_key=`, IPTV playlist URLs carry tokens), and these messages reach the UI
/// and logs, so the URL is stripped before the error is turned into text.
pub fn redacted(e: reqwest::Error) -> String {
    e.without_url().to_string()
}

impl From<reqwest::Error> for PlayerError {
    fn from(e: reqwest::Error) -> Self {
        PlayerError::Http(redacted(e))
    }
}

impl From<PlayerError> for String {
    fn from(e: PlayerError) -> Self {
        e.to_string()
    }
}

pub type Result<T, E = PlayerError> = std::result::Result<T, E>;
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_errors_do_not_leak_secrets_from_the_request_url() {
        // Port 9 (discard) is closed on a normal machine, so the request fails to connect.
        let failure = reqwest::Client::new()
            .get("http://127.0.0.1:9/3/trending")
            .query(&[("api_key", "SUPERSECRETKEY123")])
            .send()
            .await
            .expect_err("nothing should be listening on the discard port");

        assert!(
            failure.to_string().contains("SUPERSECRETKEY123"),
            "precondition: reqwest's own text includes the URL (this is what we guard against)"
        );
        assert!(!redacted(failure).contains("SUPERSECRETKEY123"));

        let again = reqwest::Client::new()
            .get("http://127.0.0.1:9/?token=abc123")
            .send()
            .await
            .expect_err("closed port");
        let text = String::from(PlayerError::from(again));
        assert!(!text.contains("abc123"), "the From impl redacts too: {text}");
    }
}
