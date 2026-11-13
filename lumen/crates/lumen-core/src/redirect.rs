//! Resolves a stream address's HTTP redirect chain in the application, once, before the address
//! is handed to mpv.
//!
//! mpv follows redirects on its own, and the hops it follows are not checked against `netguard`:
//! a public address can bounce the player to a private one. Resolving here puts every hop through
//! the same checks as every other request Lumen makes. The result is cached, so normal playback
//! of an address already resolved costs no request at all; the cache entry is dropped only when
//! playback of it fails (`invalidate`, `invalidate_current`), because a redirect target is often
//! a token that has expired by then, and the next attempt resolves it afresh.

use crate::error::Result;
use crate::netguard;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, LOCATION, RANGE};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const MAX_HOPS: usize = 5;
/// How long a resolved address is trusted.
const RESOLVED_TTL: Duration = Duration::from_secs(5 * 60);
/// How long a failed resolution (host slow or refusing) is remembered, so an unresponsive host
/// costs one wait, not one per play. Playback then goes to mpv with the address as it was.
const UNRESOLVED_TTL: Duration = Duration::from_secs(60);
const HOP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ENTRIES: usize = 256;

struct Entry {
    target: String,
    allow_lan: bool,
    expires: Instant,
}

struct State {
    entries: HashMap<String, Entry>,
    /// The address the player was last given, so a playback error can drop its entry.
    current: Option<String>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State {
        entries: HashMap::new(),
        current: None,
    })
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the player should open for `url`: the end of its redirect chain, or `url` itself when it
/// does not redirect, is not http(s), or its host did not answer in time. Fails only when a hop
/// is refused by `netguard` (or the network-privacy route is unusable), never because of a slow
/// or unreachable host.
pub async fn resolve(url: &str, headers: &[(&str, &str)], allow_lan: bool) -> Result<String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Ok(url.to_string());
    }
    {
        let mut state = state();
        let hit = state
            .entries
            .get(url)
            .filter(|entry| entry.allow_lan == allow_lan && entry.expires > Instant::now())
            .map(|entry| entry.target.clone());
        if let Some(target) = hit {
            state.current = Some(url.to_string());
            return Ok(target);
        }
    }

    let (target, ttl) = match follow(url, headers, allow_lan).await? {
        Some(target) => (target, RESOLVED_TTL),
        None => (url.to_string(), UNRESOLVED_TTL),
    };
    let mut state = state();
    if state.entries.len() >= MAX_ENTRIES {
        let now = Instant::now();
        state.entries.retain(|_, entry| entry.expires > now);
        if state.entries.len() >= MAX_ENTRIES {
            state.entries.clear();
        }
    }
    state.entries.insert(
        url.to_string(),
        Entry {
            target: target.clone(),
            allow_lan,
            expires: Instant::now() + ttl,
        },
    );
    state.current = Some(url.to_string());
    Ok(target)
}

/// Forgets what `url` resolved to, so the next play resolves it again.
pub fn invalidate(url: &str) {
    state().entries.remove(url);
}

/// `invalidate` for whatever the player was last given; used when the player reports an error
/// and only the player knows it happened.
pub fn invalidate_current() {
    let mut state = state();
    if let Some(url) = state.current.take() {
        state.entries.remove(&url);
    }
}

fn header_map(headers: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in headers.iter().take(20) {
        if value.chars().any(char::is_control) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.trim().as_bytes()),
            HeaderValue::from_str(value.trim()),
        ) {
            map.insert(name, value);
        }
    }
    map
}

/// Walks the chain. `Ok(Some(end))` is where it ends; `Ok(None)` means it could not be followed
/// (no answer, a missing or unusable `Location`, too many hops); `Err` means a hop was refused.
async fn follow(url: &str, headers: &[(&str, &str)], allow_lan: bool) -> Result<Option<String>> {
    let origin = netguard::validate_url(url, allow_lan)?;
    let client = netguard::guarded_client(allow_lan)?
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HOP_TIMEOUT)
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) Lumen/0.1")
        .build()
        .map_err(|error| crate::error::PlayerError::Http(error.to_string()))?;
    let custom = header_map(headers);

    let mut current = origin.clone();
    for _ in 0..=MAX_HOPS {
        netguard::check_url(&current, allow_lan)?;
        let mut request = client.get(current.clone()).header(RANGE, "bytes=0-0");
        // The player would send these headers to the address it was given; they are never sent to
        // another host a redirect points at (a Cookie or Referer is for the original one).
        if current.host_str() == origin.host_str() && current.port() == origin.port() {
            request = request.headers(custom.clone());
        }
        // Only the status line and headers are needed: the response is dropped unread, which
        // closes the connection without pulling any of the stream.
        let Ok(response) = request.send().await else {
            return Ok(None);
        };
        if response.status().is_redirection() {
            let next = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|location| current.join(location).ok());
            match next {
                Some(next) => {
                    current = next;
                    continue;
                }
                None => return Ok(None),
            }
        }
        return Ok(Some(current.to_string()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A loopback server: `/start` redirects to `/mid`, `/mid` to `/end`, anything else answers
    /// 200. Counts the requests it receives.
    async fn serve(hits: Arc<AtomicUsize>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                hits.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0u8; 2048];
                let n = socket.read(&mut buffer).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let reply = match path.as_str() {
                    "/start" => "HTTP/1.1 302 Found\r\nLocation: /mid\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    "/mid" => "HTTP/1.1 302 Found\r\nLocation: /end\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    "/bad" => "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    _ => "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        port
    }

    #[tokio::test]
    async fn a_redirect_chain_resolves_to_its_end_and_is_then_served_from_the_cache() {
        let hits = Arc::new(AtomicUsize::new(0));
        let port = serve(hits.clone()).await;
        let start = format!("http://127.0.0.1:{port}/start");

        let end = resolve(&start, &[], true).await.unwrap();
        assert_eq!(end, format!("http://127.0.0.1:{port}/end"));
        let after_first = hits.load(Ordering::SeqCst);
        assert_eq!(after_first, 3, "start, mid and end were each asked once");

        let again = resolve(&start, &[], true).await.unwrap();
        assert_eq!(again, end);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            after_first,
            "a cached address costs no request"
        );

        invalidate(&start);
        resolve(&start, &[], true).await.unwrap();
        assert!(
            hits.load(Ordering::SeqCst) > after_first,
            "after a playback error the address is resolved afresh"
        );
    }

    #[tokio::test]
    async fn a_hop_to_a_forbidden_address_is_refused_not_followed() {
        let hits = Arc::new(AtomicUsize::new(0));
        let port = serve(hits).await;
        let result = resolve(&format!("http://127.0.0.1:{port}/bad"), &[], true).await;
        assert!(result.is_err(), "the metadata address is never allowed");
    }

    #[tokio::test]
    async fn a_host_that_does_not_answer_is_left_for_the_player_and_remembered() {
        // Nothing listens on this port.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}/gone");
        assert_eq!(resolve(&url, &[], true).await.unwrap(), url);
        let started = Instant::now();
        assert_eq!(resolve(&url, &[], true).await.unwrap(), url);
        assert!(started.elapsed() < Duration::from_millis(100), "remembered");
    }

    #[tokio::test]
    async fn only_http_addresses_are_resolved() {
        assert_eq!(
            resolve("rtsp://example.test/live", &[], false).await.unwrap(),
            "rtsp://example.test/live"
        );
        assert_eq!(resolve("/home/u/movie.mkv", &[], false).await.unwrap(), "/home/u/movie.mkv");
    }
}
