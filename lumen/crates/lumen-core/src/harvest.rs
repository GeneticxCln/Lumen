//! HTML stream harvester using `scraper`.
//!
//! Fetches a user-supplied page via `netguard` with `read_capped_body` from `import.rs`,
//! then scans:
//! 1. `<a|video|source|iframe|link>` attributes (`href`, `src`, `data-src`, `data-url`, `data-stream`),
//! 2. `<script type="application/ld+json">` JSON-LD objects (`contentUrl`, `embedUrl`, `url`, or `.m3u/.m3u8`),
//! 3. Raw HTML / inline script text via regex for `.m3u` / `.m3u8` URLs.

use crate::error::{PlayerError, Result};
use crate::netguard;
use crate::playlist::import::read_capped_body;
use regex::Regex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;
use url::Url;

pub const MAX_HARVEST_BYTES: usize = 5 * 1024 * 1024;
pub const HARVEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HarvestResponse {
    pub urls: Vec<String>,
}

static STREAM_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)https?://[^\s"'<>\\]+?\.(?:m3u8|m3u)(?:\?[^\s"'<>\\]*)?"#)
        .expect("valid stream URL regex")
});

static RELATIVE_M3U_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(?:m3u8|m3u)(?:\?|$)").expect("valid relative m3u regex")
});

static TAG_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("a, video, source, iframe, link").expect("valid tag selector")
});

static SCRIPT_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("script").expect("valid script selector")
});

fn resolve_candidate(base: &Url, raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let resolved = base.join(trimmed).ok()?;
    if !matches!(resolved.scheme(), "http" | "https") {
        return None;
    }
    let url_str = resolved.to_string();
    if RELATIVE_M3U_RE.is_match(&url_str) {
        Some(url_str)
    } else {
        None
    }
}

fn collect_from_json_ld(data: &serde_json::Value, base: &Url, found: &mut Vec<String>) {
    match data {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    if (matches!(k.as_str(), "contentUrl" | "embedUrl" | "url") || RELATIVE_M3U_RE.is_match(s))
                        && let Some(resolved) = resolve_candidate(base, s)
                    {
                        found.push(resolved);
                    }
                } else {
                    collect_from_json_ld(v, base, found);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_from_json_ld(item, base, found);
            }
        }
        _ => {}
    }
}

/// Extracts `.m3u` and `.m3u8` stream URLs from an HTML document and its base URL.
pub fn extract_streams_from_html(base_url: &Url, html_text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let base_str = base_url.to_string();
    if RELATIVE_M3U_RE.is_match(&base_str) {
        found.push(base_str);
    }

    let document = Html::parse_document(html_text);

    // 1. <a|video|source|iframe|link> attributes
    for element in document.select(&TAG_SELECTOR) {
        for attr in ["href", "src", "data-src", "data-url", "data-stream"] {
            if let Some(val) = element.value().attr(attr)
                && let Some(resolved) = resolve_candidate(base_url, val)
            {
                found.push(resolved);
            }
        }
    }

    // 2. JSON-LD blocks (<script type="application/ld+json">)
    for script in document.select(&SCRIPT_SELECTOR) {
        let is_ld_json = script
            .value()
            .attr("type")
            .is_some_and(|t| t.to_ascii_lowercase().contains("ld+json"));
        if !is_ld_json {
            continue;
        }
        let raw: String = script.text().collect();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
            collect_from_json_ld(&parsed, base_url, &mut found);
        }
    }

    // 3. Regex scan over raw HTML / inline scripts
    for mat in STREAM_URL_RE.find_iter(html_text) {
        let cleaned = mat
            .as_str()
            .trim_end_matches(['.', ',', ';', ')', ']', '}', '\'', '"']);
        found.push(cleaned.to_string());
    }

    // Deduplicate while preserving discovery order
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for item in found {
        if seen.insert(item.clone()) {
            unique.push(item);
        }
    }
    unique
}

/// Fetches a page through `netguard` (bounded by `MAX_HARVEST_BYTES` via `read_capped_body`)
/// and extracts any `.m3u` / `.m3u8` links found.
pub async fn harvest_streams(url: &str, allow_lan: bool) -> Result<HarvestResponse> {
    let target = netguard::validate_url(url, allow_lan)?;

    let client = netguard::guarded_client(allow_lan)
        .timeout(HARVEST_TIMEOUT)
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Lumen/0.1")
        .build()
        .map_err(PlayerError::from)?;

    let response = client
        .get(target.clone())
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .send()
        .await
        .map_err(PlayerError::from)?;

    if !response.status().is_success() {
        return Err(PlayerError::Http(format!(
            "HTTP {} when fetching page to harvest",
            response.status()
        )));
    }

    let final_url = response.url().clone();
    let body = read_capped_body(response, MAX_HARVEST_BYTES).await?;
    let urls = extract_streams_from_html(&final_url, &body);
    Ok(HarvestResponse { urls })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harvests_tags_json_ld_and_inline_regex_in_order_without_duplicates() {
        let base = Url::parse("https://tv.example.com/watch/live.html").unwrap();
        let html = r#"
        <!doctype html>
        <html>
        <head>
          <link rel="alternate" href="/streams/main.m3u8" />
          <script type="application/ld+json">
          {
            "@context": "https://schema.org",
            "@type": "VideoObject",
            "contentUrl": "https://cdn.example.com/live/jsonld.m3u8?token=1",
            "embedUrl": "../playlist/list.m3u"
          }
          </script>
        </head>
        <body>
          <video src="relative/video.m3u8"></video>
          <source data-stream="https://cdn.example.com/live/source.m3u8" />
          <a href="https://cdn.example.com/live/jsonld.m3u8?token=1">Duplicate link</a>
          <script>
            const backup = "https://backup.example.org/hls/index.m3u8?hd=1";
          </script>
        </body>
        </html>
        "#;

        let urls = extract_streams_from_html(&base, html);
        assert_eq!(
            urls,
            vec![
                "https://tv.example.com/streams/main.m3u8",
                "https://tv.example.com/watch/relative/video.m3u8",
                "https://cdn.example.com/live/source.m3u8",
                "https://cdn.example.com/live/jsonld.m3u8?token=1",
                "https://tv.example.com/playlist/list.m3u",
                "https://backup.example.org/hls/index.m3u8?hd=1",
            ]
        );
    }

    #[tokio::test]
    async fn harvest_refuses_loopback_by_default() {
        let err = harvest_streams("http://127.0.0.1:8080/page.html", false).await;
        assert!(err.is_err());
    }
}
