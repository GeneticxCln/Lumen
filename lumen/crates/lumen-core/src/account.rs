//! Core account helpers (PKCE S256, loopback callback parsing, and OS keychain app-token storage)
//! shared with `sync.rs` and the Tauri desktop adapter.

use crate::error::{PlayerError, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

pub const DEFAULT_ACCOUNT_ORIGIN: &str = "https://site-b5ff4ad8e70e42569427f90c0b82d6a0.freebuff.page";
const KEYCHAIN_SERVICE: &str = "dev.lumen.app.account";
const KEYCHAIN_ACCOUNT: &str = "app-token";
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REQUEST_HEAD_BYTES: usize = 8192;

const SUCCESS_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Lumen</title></head>\
<body><p>Signed in to Lumen. You can close this tab.</p></body></html>";
const FAILURE_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Lumen</title></head>\
<body><p>This sign-in response did not match the request. Return to Lumen and try again.</p></body></html>";
const NOT_FOUND_PAGE: &str = "<!doctype html><html><body><p>Not found.</p></body></html>";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub signed_in: bool,
    pub sub: Option<String>,
    /// False when a stored token could not be checked (offline or server error).
    pub verified: bool,
}

impl AccountStatus {
    pub fn signed_out() -> Self {
        Self { signed_in: false, sub: None, verified: true }
    }
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
}

#[derive(Deserialize)]
pub struct MeResponse {
    pub sub: String,
}

pub enum TokenCheck {
    Valid(String),
    Rejected,
    Unreachable,
}

/// The Freebuff-hosted account site. A debug build may point at a local Worker through
/// LUMEN_ACCOUNT_URL; release builds always use the built-in origin.
pub fn account_origin() -> String {
    if cfg!(debug_assertions)
        && let Ok(origin) = std::env::var("LUMEN_ACCOUNT_URL")
    {
        return origin.trim_end_matches('/').to_string();
    }
    DEFAULT_ACCOUNT_ORIGIN.to_string()
}

fn keychain_error(error: keyring::Error) -> PlayerError {
    PlayerError::Io(format!("OS keychain unavailable: {error}"))
}

fn token_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).map_err(keychain_error)
}

pub fn load_app_token() -> Result<Option<String>> {
    match token_entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(keychain_error(error)),
    }
}

pub fn store_app_token(token: &str) -> Result<()> {
    token_entry()?.set_password(token).map_err(keychain_error)
}

pub fn clear_app_token() -> Result<()> {
    match token_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(keychain_error(error)),
    }
}

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder().timeout(HTTP_TIMEOUT).build().map_err(PlayerError::from)
}

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 32 bytes from the OS RNG (uuid v4 draws from it), as a 43-character string.
pub fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    base64url(&bytes)
}

/// PKCE S256 challenge for a verifier (RFC 7636).
pub fn s256_challenge(verifier: &str) -> String {
    base64url(Sha256::digest(verifier.as_bytes()).as_slice())
}

/// Parses the request line of an HTTP request into an absolute URL on the loopback host.
pub fn parse_request_target(head: &str) -> Option<Url> {
    let mut parts = head.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    if !target.starts_with('/') {
        return None;
    }
    Url::parse(&format!("http://127.0.0.1{target}")).ok()
}

/// Extracts the one-time code from the callback, but only when the state matches the one
/// this sign-in started with.
pub fn callback_code(url: &Url, expected_state: &str) -> Result<String> {
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err(PlayerError::InvalidArg("Sign-in response did not match this request".into()));
    }
    code.filter(|c| !c.is_empty())
        .ok_or_else(|| PlayerError::InvalidArg("Sign-in response had no code".into()))
}

async fn read_request_head(stream: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while buffer.len() < MAX_REQUEST_HEAD_BYTES {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Accepts loopback connections until the callback with the matching state arrives.
/// Stray requests get a 404 and do not end the sign-in.
pub async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let head = read_request_head(&mut stream).await;
        match parse_request_target(&head) {
            Some(url) if url.path() == "/callback" => match callback_code(&url, expected_state) {
                Ok(code) => {
                    respond(&mut stream, "200 OK", SUCCESS_PAGE).await;
                    return Ok(code);
                }
                Err(_) => respond(&mut stream, "400 Bad Request", FAILURE_PAGE).await,
            },
            _ => respond(&mut stream, "404 Not Found", NOT_FOUND_PAGE).await,
        }
    }
}

pub async fn check_token(origin: &str, token: &str) -> TokenCheck {
    let Ok(client) = http_client() else {
        return TokenCheck::Unreachable;
    };
    let response = match client
        .get(format!("{origin}/api/desktop/me"))
        .bearer_auth(token)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return TokenCheck::Unreachable,
    };
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return TokenCheck::Rejected;
    }
    if !response.status().is_success() {
        return TokenCheck::Unreachable;
    }
    match response.json::<MeResponse>().await {
        Ok(me) => TokenCheck::Valid(me.sub),
        Err(_) => TokenCheck::Unreachable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s256_matches_rfc_7636_vector() {
        assert_eq!(
            s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn test_random_secrets_are_long_and_distinct() {
        let first = random_secret();
        let second = random_secret();
        assert_eq!(first.len(), 43);
        assert_ne!(first, second);
    }

    #[test]
    fn test_request_target_parses_callback() {
        let head = "GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let url = parse_request_target(head).expect("callback line parses");
        assert_eq!(url.path(), "/callback");
        assert_eq!(callback_code(&url, "xyz").unwrap(), "abc");
    }

    #[test]
    fn test_callback_rejects_wrong_state_and_missing_code() {
        let wrong = parse_request_target("GET /callback?code=abc&state=nope HTTP/1.1\r\n").unwrap();
        assert!(callback_code(&wrong, "xyz").is_err(), "state must match");

        let missing = parse_request_target("GET /callback?state=xyz HTTP/1.1\r\n").unwrap();
        assert!(callback_code(&missing, "xyz").is_err(), "code is required");
    }

    #[test]
    fn test_request_target_rejects_non_get_and_relative_forms() {
        assert!(parse_request_target("POST /callback HTTP/1.1\r\n").is_none());
        assert!(parse_request_target("GET http://evil.example/callback HTTP/1.1\r\n").is_none());
    }
}
