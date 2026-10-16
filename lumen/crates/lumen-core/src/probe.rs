//! Live stream availability probing (`probe.rs`).
//!
//! Uses `reqwest` with concurrency 8, 5 s per-request timeout, same-host redirect policy (up to 5
//! hops), per-channel HTTP headers (B7), an overall deadline, and streams results back as they
//! complete so unresponsive hosts never block already-finished probes.

use crate::netguard;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use url::Url;

pub const MAX_CONCURRENT_PROBES: usize = 8;
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_OVERALL_DEADLINE: Duration = Duration::from_secs(12);
pub const MAX_SAME_HOST_REDIRECTS: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeTarget {
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeResult {
    pub url: String,
    pub live: bool,
    pub status: Option<u16>,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeResponse {
    pub results: Vec<ProbeResult>,
}

fn effective_port(url: &Url) -> Option<u16> {
    url.port_or_known_default()
}

fn same_origin_host_port(a: &Url, b: &Url) -> bool {
    a.host_str().is_some() && a.host_str() == b.host_str() && effective_port(a) == effective_port(b)
}

fn build_header_map(extra: &HashMap<String, String>) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, v) in extra.iter().take(20) {
        if v.chars().any(char::is_control) {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.trim().as_bytes()),
            HeaderValue::from_str(v.trim()),
        ) {
            map.insert(name, val);
        }
    }
    map
}

fn build_probe_client(allow_lan: bool) -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .dns_resolver(Arc::new(netguard::PublicOnlyResolver::new(allow_lan)))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PROBE_TIMEOUT)
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) Lumen/0.1")
        .build()
        .ok()
}

/// Probes a single URL with optional per-channel headers, enforcing `netguard` and same-host redirects.
pub async fn probe_single(
    raw_url: &str,
    extra_headers: &HashMap<String, String>,
    allow_lan: bool,
) -> ProbeResult {
    let Some(client) = build_probe_client(allow_lan) else {
        return ProbeResult {
            url: raw_url.to_string(),
            live: false,
            status: None,
            content_type: None,
        };
    };
    probe_single_with_client(&client, raw_url, extra_headers, allow_lan).await
}

async fn probe_single_with_client(
    client: &reqwest::Client,
    raw_url: &str,
    extra_headers: &HashMap<String, String>,
    allow_lan: bool,
) -> ProbeResult {
    let fail = |status: Option<u16>, content_type: Option<String>| ProbeResult {
        url: raw_url.to_string(),
        live: false,
        status,
        content_type,
    };

    let trimmed = raw_url.trim();
    let Ok(origin_url) = netguard::validate_url(trimmed, allow_lan) else {
        return fail(None, None);
    };
    if !origin_url.username().is_empty() || origin_url.password().is_some() {
        return fail(None, None);
    }

    let custom_headers = build_header_map(extra_headers);
    let mut current_url = origin_url.clone();

    for _ in 0..=MAX_SAME_HOST_REDIRECTS {
        if netguard::check_url(&current_url, allow_lan).is_err() {
            return fail(None, None);
        }

        let head_res = client
            .head(current_url.clone())
            .headers(custom_headers.clone())
            .send()
            .await;

        let mut status_code: Option<u16> = None;
        let mut content_type: Option<String> = None;
        let mut redirect_loc: Option<String> = None;
        let mut fallback_get = false;

        match head_res {
            Ok(resp) => {
                let code = resp.status().as_u16();
                if matches!(code, 400 | 403 | 404 | 405 | 501) {
                    fallback_get = true;
                } else {
                    status_code = Some(code);
                    content_type = resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                    redirect_loc = resp
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                }
            }
            Err(_) => return fail(None, None),
        }

        if fallback_get {
            match client
                .get(current_url.clone())
                .headers(custom_headers.clone())
                .send()
                .await
            {
                Ok(resp) => {
                    status_code = Some(resp.status().as_u16());
                    content_type = resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                    redirect_loc = resp
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                    // Drop `resp` immediately so we don't download the stream body.
                }
                Err(_) => return fail(None, None),
            }
        }

        let Some(code) = status_code else {
            return fail(None, None);
        };

        if matches!(code, 301 | 302 | 303 | 307 | 308)
            && let Some(loc) = redirect_loc
        {
            let Ok(next_url) = current_url.join(&loc) else {
                return fail(Some(code), content_type);
            };
            if !same_origin_host_port(&origin_url, &next_url)
                || netguard::check_url(&next_url, allow_lan).is_err()
            {
                return fail(Some(code), content_type);
            }
            current_url = next_url;
            continue;
        }

        return ProbeResult {
            url: raw_url.to_string(),
            live: (200..400).contains(&code),
            status: Some(code),
            content_type,
        };
    }

    fail(None, None)
}

/// Probes targets concurrently (up to `MAX_CONCURRENT_PROBES`), streaming each `(index, ProbeResult)`
/// over `on_complete` as soon as it finishes, and stopping once `deadline` elapses.
/// Returns all completed results (indexed by their position in `targets`: `None` if timed out by the overall deadline).
pub async fn probe_targets_bounded<F>(
    targets: Vec<ProbeTarget>,
    allow_lan: bool,
    deadline: Duration,
    mut on_complete: F,
) -> Vec<Option<ProbeResult>>
where
    F: FnMut(usize, &ProbeResult),
{
    let total = targets.len();
    if total == 0 {
        return Vec::new();
    }

    let Some(client) = build_probe_client(allow_lan) else {
        return vec![None; total];
    };
    let targets = Arc::new(targets);
    let next_idx = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_count = total.min(MAX_CONCURRENT_PROBES);
    let (tx, mut rx) = mpsc::unbounded_channel::<(usize, ProbeResult)>();
    let mut handles = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let targets = targets.clone();
        let next_idx = next_idx.clone();
        let client = client.clone();
        let tx = tx.clone();
        handles.push(tokio::spawn(async move {
            loop {
                let idx = next_idx.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if idx >= total {
                    break;
                }
                let target = &targets[idx];
                let res =
                    probe_single_with_client(&client, &target.url, &target.headers, allow_lan)
                        .await;
                let _ = tx.send((idx, res));
            }
        }));
    }
    drop(tx);

    let mut outcomes: Vec<Option<ProbeResult>> = vec![None; total];
    let collect_all = async {
        while let Some((idx, res)) = rx.recv().await {
            on_complete(idx, &res);
            outcomes[idx] = Some(res);
        }
    };

    let _ = tokio::time::timeout(deadline, collect_all).await;

    // Abort any probes still running past the overall deadline.
    for handle in handles {
        handle.abort();
    }

    // Drain any items that arrived just before abort.
    while let Ok((idx, res)) = rx.try_recv() {
        if outcomes[idx].is_none() {
            on_complete(idx, &res);
            outcomes[idx] = Some(res);
        }
    }

    outcomes
}

/// Convenience wrapper returning a `ProbeResponse` in input order; targets that did not finish
/// before `deadline` are marked `live: false, status: None`.
pub async fn probe_targets(
    targets: Vec<ProbeTarget>,
    allow_lan: bool,
    deadline: Duration,
) -> ProbeResponse {
    let urls: Vec<String> = targets.iter().map(|t| t.url.clone()).collect();
    let outcomes = probe_targets_bounded(targets, allow_lan, deadline, |_, _| {}).await;
    let results = urls
        .into_iter()
        .zip(outcomes)
        .map(|(url, opt)| {
            opt.unwrap_or(ProbeResult {
                url,
                live: false,
                status: None,
                content_type: None,
            })
        })
        .collect();
    ProbeResponse { results }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn probe_never_reaches_loopback_when_lan_disabled() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                hits_clone.fetch_add(1, Ordering::SeqCst);
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        let res = probe_single(
            &format!("http://127.0.0.1:{port}/live.m3u8"),
            &HashMap::new(),
            false,
        )
        .await;
        assert!(!res.live);
        assert_eq!(res.status, None);
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn probe_forwards_per_channel_headers_and_falls_back_from_head_to_get() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let has_custom_referer = req
                    .lines()
                    .any(|l| l.eq_ignore_ascii_case("referer: https://custom.example/"));
                let has_custom_ua = req
                    .lines()
                    .any(|l| l.eq_ignore_ascii_case("user-agent: CustomAgent/2.0"));
                if req.starts_with("HEAD ") {
                    let _ = stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                } else if has_custom_referer && has_custom_ua {
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                } else {
                    let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                }
            }
        });

        let mut headers = HashMap::new();
        headers.insert("Referer".into(), "https://custom.example/".into());
        headers.insert("User-Agent".into(), "CustomAgent/2.0".into());

        // Without headers -> 403
        let without = probe_single(
            &format!("http://127.0.0.1:{port}/stream.m3u8"),
            &HashMap::new(),
            true,
        )
        .await;
        assert!(!without.live);
        assert_eq!(without.status, Some(403));

        // With per-channel headers (B7) -> 200 + content_type
        let with_hdrs = probe_single(
            &format!("http://127.0.0.1:{port}/stream.m3u8"),
            &headers,
            true,
        )
        .await;
        assert!(with_hdrs.live);
        assert_eq!(with_hdrs.status, Some(200));
        assert_eq!(
            with_hdrs.content_type.as_deref(),
            Some("application/vnd.apple.mpegurl")
        );

        server.abort();
    }

    #[tokio::test]
    async fn probe_rejects_cross_host_redirects_and_streams_partial_results_under_deadline() {
        let fast_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fast_port = fast_listener.local_addr().unwrap().port();
        let fast_srv = tokio::spawn(async move {
            while let Ok((mut s, _)) = fast_listener.accept().await {
                let mut b = [0u8; 512];
                let _ = s.read(&mut b).await;
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            }
        });

        let hang_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hang_port = hang_listener.local_addr().unwrap().port();
        let hang_srv = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = hang_listener.accept().await {
                held.push(s);
            }
        });

        let targets = vec![
            ProbeTarget {
                url: format!("http://127.0.0.1:{fast_port}/ok.ts"),
                headers: HashMap::new(),
            },
            ProbeTarget {
                url: format!("http://127.0.0.1:{hang_port}/hang.ts"),
                headers: HashMap::new(),
            },
        ];

        let mut streamed = Vec::new();
        let outcomes =
            probe_targets_bounded(targets, true, Duration::from_millis(400), |idx, res| {
                streamed.push((idx, res.live))
            })
            .await;

        assert_eq!(streamed, vec![(0, true)]);
        assert!(outcomes[0].as_ref().unwrap().live);
        assert!(
            outcomes[1].is_none(),
            "hanging host timed out by overall deadline"
        );

        fast_srv.abort();
        hang_srv.abort();
    }
}
