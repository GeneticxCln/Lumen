//! Search engine (`search.rs`): indexed O(n) deduplication, library + user-JSON providers via
//! `netguard`, and bounded availability probing that returns partial results with
//! `available = null` when the probe deadline expires (fixes A4).

use crate::error::{PlayerError, Result};
use crate::netguard;
use crate::playlist::import::read_capped_body;
use crate::probe::{probe_targets_bounded, ProbeTarget};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

pub const MAX_PROVIDERS: usize = 12;
pub const MAX_PROVIDER_RESULTS: usize = 40;
pub const MAX_SEARCH_RESULTS: usize = 80;
pub const MAX_LIBRARY_PAYLOAD: usize = 5000;
pub const MAX_QUERY_LENGTH: usize = 160;
pub const DEFAULT_PROVIDER_TIMEOUT_SECS: f64 = 8.0;
pub const MAX_PROVIDER_TIMEOUT_SECS: f64 = 15.0;
pub const MAX_API_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const SEARCH_CACHE_TTL: Duration = Duration::from_secs(25);
pub const MAX_CACHE_ENTRIES: usize = 128;
pub const MAX_CONCURRENT_SEARCHES: usize = 4;
pub const DEFAULT_AVAILABILITY_DEADLINE: Duration = Duration::from_secs(6);

static QUALITY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(4320|2160|1440|1080|720|576|480|360)\s*p?\b|\b(8k|4k|uhd|fhd|hd)\b")
        .expect("valid quality regex")
});

static HEADER_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,80}$").expect("valid header name regex")
});

static LANGUAGE_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z]{2,3}(?:[-_][a-z0-9]{2,8})*$").expect("valid language tag regex")
});

static PAREN_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\(.*\)$").expect("valid paren suffix regex"));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub library: Vec<Value>,
    #[serde(default)]
    pub providers: Vec<Value>,
    #[serde(default = "default_true")]
    pub check_availability: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_language: Option<String>,
    #[serde(default)]
    pub allow_lan: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderSource {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResultItem {
    pub id: String,
    pub title: String,
    pub url: String,
    pub provider: String,
    pub providers: Vec<String>,
    pub provider_sources: Vec<ProviderSource>,
    pub aliases: Vec<String>,
    pub year: Option<i64>,
    pub media_type: Option<String>,
    pub quality: Option<String>,
    pub language: Option<String>,
    pub headers: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_title: Option<String>,
    pub available: Option<bool>,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    pub score: i32,
    #[serde(skip)]
    pub context: Vec<String>,
    #[serde(skip)]
    pub normalized_title: String,
    #[serde(skip)]
    pub normalized_aliases: Vec<String>,
    #[serde(skip)]
    pub quality_rank: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderStatus {
    pub id: String,
    pub name: String,
    pub status: String, // "ok" | "error" | "timeout"
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResponse {
    pub query: String,
    pub results: Vec<Value>,
    pub providers: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ValidatedProviderConfig {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub token: String,
    pub timeout: f64,
    pub results_path: String,
    pub title_field: String,
    pub url_field: String,
    pub aliases_field: String,
    pub year_field: String,
    pub media_type_field: String,
    pub quality_field: String,
    pub language_field: String,
    pub headers_field: String,
}

/// Normalizes text once: lowercases ASCII/Unicode and collapses non-alphanumeric runs to single spaces.
pub fn normalize_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_space = true;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

pub fn normalize_language(raw: Option<&str>) -> Option<String> {
    let text = raw?.trim();
    if text.is_empty() {
        return None;
    }
    let mapped = |code: &str| -> Option<&'static str> {
        match code {
            "english" | "eng" => Some("en"),
            "french" | "fra" | "fre" => Some("fr"),
            "german" | "deu" | "ger" => Some("de"),
            "spanish" | "spa" => Some("es"),
            "italian" | "ita" => Some("it"),
            "portuguese" | "por" => Some("pt"),
            "arabic" | "ara" => Some("ar"),
            "hindi" | "hin" => Some("hi"),
            "japanese" | "jpn" => Some("ja"),
            "korean" | "kor" => Some("ko"),
            "chinese" | "zho" | "chi" => Some("zh"),
            "russian" | "rus" => Some("ru"),
            "turkish" | "tur" => Some("tr"),
            _ => None,
        }
    };
    if text.len() <= 16
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        let lower = text.to_ascii_lowercase();
        if let Some(code) = mapped(&lower) {
            return Some(code.to_string());
        }
        let primary = lower.split(['-', '_']).next().unwrap_or(&lower);
        if (2..=3).contains(&primary.len()) && primary.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Some(mapped(primary).unwrap_or(primary).to_string());
        }
    }

    let truncated: String = text.chars().take(40).collect();
    let without_parens = PAREN_SUFFIX_RE
        .replace(&truncated.to_lowercase(), "")
        .trim()
        .to_string();
    if without_parens.is_empty() {
        return None;
    }
    if let Some(code) = mapped(&without_parens) {
        return Some(code.to_string());
    }
    if !LANGUAGE_TAG_RE.is_match(&without_parens) {
        return None;
    }
    let primary = without_parens
        .split(['-', '_'])
        .next()
        .unwrap_or(&without_parens);
    Some(mapped(primary).unwrap_or(primary).to_string())
}

fn quality_rank(quality: Option<&str>) -> i32 {
    match quality.map(|q| q.to_ascii_lowercase()).as_deref() {
        Some("4320p" | "8k") => 8,
        Some("2160p" | "4k" | "uhd") => 7,
        Some("1440p") => 6,
        Some("1080p" | "fhd") => 5,
        Some("720p") => 4,
        Some("hd" | "576p") => 3,
        Some("480p") => 2,
        Some("360p") => 1,
        _ => 0,
    }
}

pub fn extract_quality(explicit: Option<&str>, title: &str) -> Option<String> {
    if let Some(exp) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(exp.chars().take(40).collect());
    }
    let caps = QUALITY_RE.captures(title)?;
    let val = caps
        .get(1)
        .or_else(|| caps.get(2))
        .map(|m| m.as_str().to_ascii_lowercase())?;
    if val.chars().all(|c| c.is_ascii_digit()) {
        Some(format!("{val}p"))
    } else {
        Some(val)
    }
}

fn nested_field<'a>(row: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(row);
    }
    let mut current = row;
    for part in path.split('.') {
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}

fn parse_aliases(value: Option<&Value>) -> Vec<String> {
    let mut raw_list = Vec::new();
    match value {
        Some(Value::String(s)) => {
            for part in s.split([';', ',', '|']) {
                let t = part.trim();
                if !t.is_empty() {
                    raw_list.push(t.to_string());
                }
            }
        }
        Some(Value::Array(arr)) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    let t = s.trim();
                    if !t.is_empty() {
                        raw_list.push(t.to_string());
                    }
                }
            }
        }
        _ => {}
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in raw_list {
        if seen.insert(item.clone()) {
            out.push(item);
            if out.len() >= 20 {
                break;
            }
        }
    }
    out
}

fn metadata_value(value: Option<&Value>, max_chars: usize) -> Option<String> {
    let v = value?;
    let text = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if text.is_empty() {
        None
    } else {
        Some(text.chars().take(max_chars).collect())
    }
}

fn safe_headers(value: Option<&Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, v) in obj.iter().take(20) {
        let Some(s) = v.as_str() else { continue };
        if HEADER_NAME_RE.is_match(k) && !s.contains('\r') && !s.contains('\n') {
            out.insert(k.clone(), s.chars().take(2048).collect());
        }
    }
    out
}

fn validate_http_url_basic(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let parsed = Url::parse(trimmed)
        .map_err(|_| PlayerError::InvalidArg("URL must use http:// or https://".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(PlayerError::InvalidArg(
            "URL must use http:// or https://".into(),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn validate_provider_config(config: &Value) -> Result<ValidatedProviderConfig> {
    let obj = config
        .as_object()
        .ok_or_else(|| PlayerError::InvalidArg("Each search provider must be an object".into()))?;

    let get_str = |key: &str, default: &str, max_len: usize| -> String {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .trim()
            .chars()
            .take(max_len)
            .collect()
    };

    let id = get_str("id", "", 200);
    let name = get_str("name", "", 200);
    let endpoint = obj
        .get("endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    let validated_endpoint = validate_http_url_basic(endpoint).map_err(|e| {
        let label = if name.is_empty() { "Provider" } else { &name };
        PlayerError::InvalidArg(format!("{label}: {e}"))
    })?;
    let parsed = Url::parse(&validated_endpoint).map_err(|_| {
        PlayerError::InvalidArg("Endpoint is not a valid URL".into())
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        let label = if name.is_empty() { "Provider" } else { &name };
        return Err(PlayerError::InvalidArg(format!(
            "{label}: endpoint must not contain credentials or a fragment"
        )));
    }
    if id.is_empty() || id.chars().count() > 80 {
        return Err(PlayerError::InvalidArg(
            "Provider id is missing or too long".into(),
        ));
    }
    if name.is_empty() || name.chars().count() > 80 {
        return Err(PlayerError::InvalidArg(
            "Provider name must be between 1 and 80 characters".into(),
        ));
    }

    let timeout = obj
        .get("timeout")
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_i64().map(|n| n as f64))
                .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
        })
        .map(|t| t.clamp(1.0, MAX_PROVIDER_TIMEOUT_SECS))
        .unwrap_or(DEFAULT_PROVIDER_TIMEOUT_SECS);

    let token = obj
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(4096)
        .collect();

    Ok(ValidatedProviderConfig {
        id,
        name,
        endpoint: validated_endpoint,
        token,
        timeout,
        results_path: get_str("results_path", "results", 120),
        title_field: get_str("title_field", "title", 120),
        url_field: get_str("url_field", "url", 120),
        aliases_field: get_str("aliases_field", "aliases", 120),
        year_field: get_str("year_field", "year", 120),
        media_type_field: get_str("media_type_field", "media_type", 120),
        quality_field: get_str("quality_field", "quality", 120),
        language_field: get_str("language_field", "language", 120),
        headers_field: get_str("headers_field", "headers", 120),
    })
}

/// Validates a list of provider configs (max 12, unique ids, safe HTTP endpoints).
pub fn validate_search_providers(configs: &[Value]) -> Result<Vec<ValidatedProviderConfig>> {
    if configs.len() > MAX_PROVIDERS {
        return Err(PlayerError::InvalidArg(format!(
            "At most {MAX_PROVIDERS} external providers can be enabled"
        )));
    }
    let mut seen_ids: HashSet<String> = HashSet::from(["library".to_string()]);
    let mut out = Vec::with_capacity(configs.len());
    for raw in configs {
        let cfg = validate_provider_config(raw)?;
        if !seen_ids.insert(cfg.id.clone()) {
            return Err(PlayerError::InvalidArg(format!(
                "Duplicate provider id: {}",
                cfg.id
            )));
        }
        out.push(cfg);
    }
    Ok(out)
}

/// Searches the local library slice (capped at 5,000 entries).
pub fn search_library(entries: &[Value], query: &str) -> Vec<SearchResultItem> {
    let norm_query = normalize_text(query);
    let terms: Vec<&str> = norm_query.split_whitespace().collect();
    let mut found = Vec::new();

    for entry in entries.iter().take(MAX_LIBRARY_PAYLOAD) {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let title = obj
            .get("name")
            .or_else(|| obj.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let url = obj
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if title.is_empty() || url.is_empty() {
            continue;
        }

        let aliases = parse_aliases(obj.get("aliases"));
        let mut context = Vec::new();
        for field in ["group_title", "tvg_id"] {
            if let Some(val) = obj.get(field) {
                for a in parse_aliases(Some(val)) {
                    if !context.contains(&a) {
                        context.push(a);
                    }
                }
            }
        }

        let norm_title = normalize_text(title);
        let norm_aliases: Vec<String> = aliases.iter().map(|a| normalize_text(a)).collect();
        let norm_context: Vec<String> = context.iter().map(|c| normalize_text(c)).collect();

        if !terms.is_empty() {
            let matches_all = terms.iter().all(|term| {
                norm_title.contains(term)
                    || norm_aliases.iter().any(|a| a.contains(term))
                    || norm_context.iter().any(|c| c.contains(term))
            });
            if !matches_all {
                continue;
            }
        }

        let Ok(validated_url) = validate_http_url_basic(url) else {
            continue;
        };
        let Ok(parsed_url) = Url::parse(&validated_url) else {
            continue;
        };
        if !parsed_url.username().is_empty() || parsed_url.password().is_some() {
            continue;
        }

        let headers = match obj.get("extra_headers") {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s)
                .ok()
                .map(|v| safe_headers(Some(&v)))
                .unwrap_or_default(),
            Some(v) => safe_headers(Some(v)),
            None => HashMap::new(),
        };

        let media_type = match obj.get("tmdb_type").and_then(|v| v.as_str()) {
            Some("movie") => Some("movie".to_string()),
            Some("tv" | "series") => Some("series".to_string()),
            _ => None,
        };

        let year = obj.get("release_year").and_then(|v| v.as_i64());
        let language = normalize_language(obj.get("language").and_then(|v| v.as_str()));
        let channel_id = obj.get("id").and_then(|v| v.as_i64());
        let playlist_id = obj.get("playlist_id").and_then(|v| v.as_i64());
        let group_title = obj
            .get("group_title")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);

        let id_suffix = channel_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| found.len().to_string());
        let truncated_title: String = title.chars().take(240).collect();

        found.push(SearchResultItem {
            id: format!("library:{id_suffix}"),
            title: truncated_title,
            url: validated_url,
            provider: "My library".to_string(),
            providers: Vec::new(),
            provider_sources: Vec::new(),
            aliases,
            year,
            media_type,
            quality: None,
            language,
            headers,
            channel_id,
            playlist_id,
            group_title,
            available: None,
            status: None,
            content_type: None,
            score: 0,
            context,
            normalized_title: norm_title,
            normalized_aliases: norm_aliases,
            quality_rank: 0,
        });
    }

    found
}

/// Parses the JSON payload returned by a user-configured external JSON provider.
pub fn parse_user_json_provider_payload(
    cfg: &ValidatedProviderConfig,
    payload: &Value,
) -> Result<Vec<SearchResultItem>> {
    let rows_val = if cfg.results_path.is_empty() {
        Some(payload)
    } else {
        nested_field(payload, &cfg.results_path)
    };
    let rows = rows_val.and_then(|v| v.as_array()).ok_or_else(|| {
        PlayerError::Parse(format!(
            "{}: response field '{}' must be an array",
            cfg.name, cfg.results_path
        ))
    })?;

    let mut results = Vec::new();
    for (idx, row) in rows.iter().take(MAX_PROVIDER_RESULTS).enumerate() {
        let Some(row_obj) = row.as_object() else {
            continue;
        };
        let title = nested_field(row, &cfg.title_field)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let url_raw = nested_field(row, &cfg.url_field)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let (Some(title), Some(url_raw)) = (title, url_raw) else {
            continue;
        };

        let Ok(validated_url) = validate_http_url_basic(url_raw) else {
            continue;
        };
        let Ok(parsed_url) = Url::parse(&validated_url) else {
            continue;
        };
        if !parsed_url.username().is_empty() || parsed_url.password().is_some() {
            continue;
        }

        let year = nested_field(row, &cfg.year_field)
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
            })
            .filter(|&y| (1870..=2200).contains(&y));

        let media_type = metadata_value(nested_field(row, &cfg.media_type_field), 32).map(|m| {
            let lower = m.to_lowercase();
            match lower.as_str() {
                "tv" | "show" => "series".to_string(),
                "film" => "movie".to_string(),
                _ => lower,
            }
        });

        let aliases = parse_aliases(nested_field(row, &cfg.aliases_field));
        let norm_title = normalize_text(title);
        let norm_aliases = aliases.iter().map(|a| normalize_text(a)).collect();
        let raw_lang = metadata_value(nested_field(row, &cfg.language_field), 40);
        let language = normalize_language(raw_lang.as_deref());
        let quality = metadata_value(nested_field(row, &cfg.quality_field), 40);
        let headers = safe_headers(nested_field(row, &cfg.headers_field));

        let item_id = row_obj
            .get("id")
            .and_then(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            })
            .unwrap_or_else(|| idx.to_string());

        results.push(SearchResultItem {
            id: format!("{}:{item_id}", cfg.id),
            title: title.chars().take(240).collect(),
            url: validated_url,
            provider: cfg.name.clone(),
            providers: Vec::new(),
            provider_sources: Vec::new(),
            aliases,
            year,
            media_type,
            quality,
            language,
            headers,
            channel_id: None,
            playlist_id: None,
            group_title: None,
            available: None,
            status: None,
            content_type: None,
            score: 0,
            context: Vec::new(),
            normalized_title: norm_title,
            normalized_aliases: norm_aliases,
            quality_rank: 0,
        });
    }

    Ok(results)
}

async fn query_user_json_provider(
    cfg: &ValidatedProviderConfig,
    query: &str,
    allow_lan: bool,
) -> std::result::Result<Vec<SearchResultItem>, (String, String)> {
    let target = netguard::validate_url(&cfg.endpoint, allow_lan)
        .map_err(|e| ("error".to_string(), String::from(e)))?;

    let client = netguard::guarded_client(allow_lan)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs_f64(cfg.timeout))
        .user_agent("Lumen/0.1")
        .build()
        .map_err(|_| ("error".to_string(), "Provider request failed".to_string()))?;

    let mut req = client
        .get(target)
        .query(&[("q", query)])
        .header(reqwest::header::ACCEPT, "application/json");
    if !cfg.token.is_empty() {
        req = req.bearer_auth(&cfg.token);
    }

    let send_fut = async {
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                ("timeout".to_string(), "Search timed out".to_string())
            } else {
                ("error".to_string(), "Provider request failed".to_string())
            }
        })?;
        if !resp.status().is_success() {
            return Err((
                "error".to_string(),
                format!("HTTP {}", resp.status().as_u16()),
            ));
        }
        let body = read_capped_body(resp, MAX_API_RESPONSE_BYTES)
            .await
            .map_err(|_| ("error".to_string(), "Provider response too large".to_string()))?;
        let payload: Value = serde_json::from_str(&body)
            .map_err(|_| ("error".to_string(), "Invalid provider JSON".to_string()))?;
        parse_user_json_provider_payload(cfg, &payload)
            .map_err(|e| ("error".to_string(), String::from(e)))
    };

    match tokio::time::timeout(Duration::from_secs_f64(cfg.timeout), send_fut).await {
        Ok(res) => res,
        Err(_) => Err(("timeout".to_string(), "Search timed out".to_string())),
    }
}

/// Computes relevance score for a candidate against a pre-normalized query.
pub fn score_candidate(item: &SearchResultItem, norm_query: &str, terms: &[&str]) -> i32 {
    if norm_query.is_empty() {
        return 0;
    }
    let owned_title;
    let title: &str = if item.normalized_title.is_empty() {
        owned_title = normalize_text(&item.title);
        &owned_title
    } else {
        &item.normalized_title
    };
    if title == norm_query {
        return 100;
    }
    if title.contains(norm_query) {
        return 90;
    }

    let owned_aliases: Vec<String>;
    let aliases: &[String] = if item.normalized_aliases.is_empty() && !item.aliases.is_empty() {
        owned_aliases = item.aliases.iter().map(|a| normalize_text(a)).collect();
        &owned_aliases
    } else {
        &item.normalized_aliases
    };
    let context: Vec<String> = if item.context.is_empty() {
        Vec::new()
    } else {
        item.context.iter().map(|c| normalize_text(c)).collect()
    };

    let in_haystack = |term: &str| -> bool {
        title.contains(term)
            || aliases.iter().any(|a| a.contains(term))
            || context.iter().any(|c| c.contains(term))
    };

    if terms.iter().all(|t| in_haystack(t)) {
        let bonus: i32 = terms
            .iter()
            .map(|t| if title.contains(t) { 3 } else { 0 })
            .sum();
        return 75 + bonus;
    }

    terms.iter().filter(|t| in_haystack(t)).count() as i32
}

fn compatible_year_and_type(a: &SearchResultItem, b: &SearchResultItem) -> bool {
    if let (Some(ya), Some(yb)) = (a.year, b.year)
        && ya != yb
    {
        return false;
    }
    if let (Some(ta), Some(tb)) = (a.media_type.as_deref(), b.media_type.as_deref())
        && ta != tb
    {
        return false;
    }
    true
}

/// Indexed O(n) deduplication across library and external provider results.
///
/// Normalizes titles and aliases once, then indexes merged entries by:
/// - `(normalized_name, year, media_type)` plus wildcard buckets so an item without `year` or
///   `media_type` still merges with a matching item that has one, and
/// - URL without fragment (`url.split('#').next()`).
pub fn deduplicate_results(
    mut results: Vec<SearchResultItem>,
    query: &str,
) -> Vec<SearchResultItem> {
    let norm_query = normalize_text(query);
    let terms: Vec<&str> = norm_query.split_whitespace().collect();

    let mut merged: Vec<SearchResultItem> = Vec::with_capacity(results.len());
    // Map from normalized title/alias -> indices in `merged` (typically 1-2 entries per title).
    let mut name_index: HashMap<String, Vec<usize>> = HashMap::with_capacity(results.len() * 2);
    // Exact hash index on `(normalized_title, year, media_type)` for O(1) primary lookup.
    let mut exact_index: HashMap<(String, Option<i64>, Option<String>), usize> =
        HashMap::with_capacity(results.len());
    // Map from base URL (without `#fragment`) -> indices in `merged`.
    let mut url_index: HashMap<String, Vec<usize>> = HashMap::with_capacity(results.len());

    for mut item in results.drain(..) {
        if item.normalized_title.is_empty() {
            item.normalized_title = normalize_text(&item.title);
        }
        if item.normalized_aliases.is_empty() && !item.aliases.is_empty() {
            item.normalized_aliases = item
                .aliases
                .iter()
                .map(|a| normalize_text(a))
                .filter(|s| !s.is_empty())
                .collect();
        }
        item.language = normalize_language(item.language.as_deref());
        item.score = score_candidate(&item, &norm_query, &terms);
        if item.score <= 0 {
            continue;
        }
        item.quality = extract_quality(item.quality.as_deref(), &item.title);
        item.quality_rank = quality_rank(item.quality.as_deref());

        let base_url = item.url.split('#').next().unwrap_or(&item.url).to_string();

        // 1. Fast path: exact `(normalized_title, year, media_type)` hash hit.
        let mut dup_idx: Option<usize> = exact_index
            .get(&(
                item.normalized_title.clone(),
                item.year,
                item.media_type.clone(),
            ))
            .copied();

        // 2. Check normalized title & aliases in `name_index` (handles alias cross-match and `None` year/type).
        if dup_idx.is_none() {
            let mut candidate_indices: Vec<usize> = Vec::new();
            if !item.normalized_title.is_empty()
                && let Some(idxs) = name_index.get(&item.normalized_title)
            {
                candidate_indices.extend_from_slice(idxs);
            }
            for alias in &item.normalized_aliases {
                if !alias.is_empty()
                    && let Some(idxs) = name_index.get(alias)
                {
                    for &idx in idxs {
                        if !candidate_indices.contains(&idx) {
                            candidate_indices.push(idx);
                        }
                    }
                }
            }
            candidate_indices.sort_unstable();
            dup_idx = candidate_indices
                .into_iter()
                .find(|&idx| compatible_year_and_type(&merged[idx], &item));
        }

        // 3. Check URL index (without `#fragment`).
        if dup_idx.is_none()
            && let Some(idxs) = url_index.get(&base_url)
        {
            dup_idx = idxs
                .iter()
                .copied()
                .find(|&idx| compatible_year_and_type(&merged[idx], &item));
        }

        let Some(idx) = dup_idx else {
            let new_idx = merged.len();
            item.providers = vec![item.provider.clone()];
            item.provider_sources = vec![ProviderSource {
                id: Some(item.id.clone()),
                name: item.provider.clone(),
            }];

            if !item.normalized_title.is_empty() {
                exact_index.insert(
                    (
                        item.normalized_title.clone(),
                        item.year,
                        item.media_type.clone(),
                    ),
                    new_idx,
                );
                name_index
                    .entry(item.normalized_title.clone())
                    .or_default()
                    .push(new_idx);
            }
            for alias in &item.normalized_aliases {
                if !alias.is_empty() {
                    let list = name_index.entry(alias.clone()).or_default();
                    if !list.contains(&new_idx) {
                        list.push(new_idx);
                    }
                }
            }
            url_index.entry(base_url).or_default().push(new_idx);
            merged.push(item);
            continue;
        };

        // Merge into `merged[idx]`
        let dup = &mut merged[idx];
        if !dup.providers.contains(&item.provider) {
            dup.providers.push(item.provider.clone());
        }
        let source = ProviderSource {
            id: Some(item.id.clone()),
            name: item.provider.clone(),
        };
        if !dup.provider_sources.contains(&source) {
            dup.provider_sources.push(source);
        }
        for (raw_alias, norm_alias) in item
            .aliases
            .iter()
            .zip(item.normalized_aliases.iter().chain(std::iter::repeat(&String::new())))
        {
            if dup.aliases.len() < 30 && !dup.aliases.contains(raw_alias) {
                dup.aliases.push(raw_alias.clone());
            }
            if !norm_alias.is_empty() && !dup.normalized_aliases.contains(norm_alias) {
                dup.normalized_aliases.push(norm_alias.clone());
                let list = name_index.entry(norm_alias.clone()).or_default();
                if !list.contains(&idx) {
                    list.push(idx);
                }
            }
        }
        if dup.year.is_none() {
            dup.year = item.year;
        }
        if dup.media_type.is_none() {
            dup.media_type = item.media_type.clone();
        }
        if dup.quality.is_none() {
            dup.quality = item.quality.clone();
            dup.quality_rank = item.quality_rank;
        }
        if dup.language.is_none() {
            dup.language = item.language.clone();
        }
        if dup.content_type.is_none() {
            dup.content_type = item.content_type.clone();
        }
        if dup.status.is_none() {
            dup.status = item.status;
        }
        if !dup.available.unwrap_or(false) && item.available == Some(true) {
            dup.available = Some(true);
        } else if dup.available.is_none() && item.available.is_some() {
            dup.available = item.available;
        }
        if dup.headers.is_empty() && !item.headers.is_empty() {
            dup.headers = item.headers.clone();
        }
        let item_score = item.score;
        if item_score > dup.score {
            let saved_channel = dup.channel_id.or(item.channel_id);
            let saved_playlist = if dup.channel_id.is_some() {
                dup.playlist_id
            } else {
                item.playlist_id
            };
            let providers = std::mem::take(&mut dup.providers);
            let provider_sources = std::mem::take(&mut dup.provider_sources);
            let aliases = std::mem::take(&mut dup.aliases);
            let norm_aliases = std::mem::take(&mut dup.normalized_aliases);
            let year = item.year.or(dup.year);
            let media_type = item.media_type.clone().or_else(|| dup.media_type.clone());
            let quality = item.quality.clone().or_else(|| dup.quality.clone());
            let language = item.language.clone().or_else(|| dup.language.clone());
            let headers = if !item.headers.is_empty() {
                item.headers.clone()
            } else {
                std::mem::take(&mut dup.headers)
            };

            *dup = item;
            dup.providers = providers;
            dup.provider_sources = provider_sources;
            dup.aliases = aliases;
            dup.normalized_aliases = norm_aliases;
            dup.year = year;
            dup.media_type = media_type;
            dup.quality_rank = quality_rank(quality.as_deref());
            dup.quality = quality;
            dup.language = language;
            dup.headers = headers;
            dup.channel_id = saved_channel;
            dup.playlist_id = saved_playlist;
        } else if dup.channel_id.is_none() && item.channel_id.is_some() {
            dup.channel_id = item.channel_id;
            dup.playlist_id = item.playlist_id;
        }
        dup.score = dup.score.max(item_score);
    }

    merged
}

/// Sorts deduplicated results by availability, relevance score, preferred language match,
/// quality rank, presence of release year, and title.
pub fn sort_results(results: &mut [SearchResultItem], preferred_language: Option<&str>) {
    results.sort_by(|a, b| {
        let avail_a = a.available == Some(true);
        let avail_b = b.available == Some(true);
        let lang_a = preferred_language.is_some_and(|p| a.language.as_deref() == Some(p));
        let lang_b = preferred_language.is_some_and(|p| b.language.as_deref() == Some(p));
        let year_a = a.year.is_some();
        let year_b = b.year.is_some();

        avail_b
            .cmp(&avail_a)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| lang_b.cmp(&lang_a))
            .then_with(|| b.quality_rank.cmp(&a.quality_rank))
            .then_with(|| year_b.cmp(&year_a))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
}

struct CacheEntry {
    expires_at: Instant,
    response: SearchResponse,
}

static SEARCH_CACHE: LazyLock<Mutex< Vec<(String, CacheEntry)> >> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static SEARCH_SLOTS: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(MAX_CONCURRENT_SEARCHES));

fn cache_key(req: &SearchRequest, preferred: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(normalize_text(&req.query).as_bytes());
    hasher.update(b":");
    hasher.update(serde_json::to_string(&req.providers).unwrap_or_default().as_bytes());
    hasher.update(b":");
    hasher.update(req.library.len().to_le_bytes());
    if let Some(first) = req.library.first() {
        hasher.update(first.to_string().as_bytes());
    }
    if let Some(last) = req.library.last() {
        hasher.update(last.to_string().as_bytes());
    }
    hasher.update(b":");
    hasher.update([u8::from(req.check_availability), u8::from(req.allow_lan)]);
    hasher.update(preferred.unwrap_or("").as_bytes());
    format!("{:x}", hasher.finalize())
}

pub async fn clear_search_cache() {
    SEARCH_CACHE.lock().await.clear();
}

/// Runs a full search across the local library and configured user-JSON providers,
/// with bounded availability probing (`availability_deadline`). Any candidate whose probe does
/// not finish before the deadline keeps `available: None` (`null` in JSON) instead of failing
/// the whole search (fixes A4).
pub async fn execute_search_with_deadline(
    req: &SearchRequest,
    availability_deadline: Duration,
) -> Result<SearchResponse> {
    let query = req.query.trim().to_string();
    if query.chars().count() < 2 {
        return Ok(SearchResponse {
            query,
            results: Vec::new(),
            providers: Vec::new(),
        });
    }
    if query.chars().count() > MAX_QUERY_LENGTH {
        return Err(PlayerError::InvalidArg(format!(
            "Search query must be at most {MAX_QUERY_LENGTH} characters"
        )));
    }
    if req.library.len() > MAX_LIBRARY_PAYLOAD {
        return Err(PlayerError::InvalidArg(format!(
            "Library search payload exceeds {MAX_LIBRARY_PAYLOAD} entries"
        )));
    }

    let preferred = normalize_language(req.preferred_language.as_deref());
    let key = cache_key(req, preferred.as_deref());

    {
        let mut cache = SEARCH_CACHE.lock().await;
        let now = Instant::now();
        cache.retain(|(_, entry)| entry.expires_at > now);
        if let Some(pos) = cache.iter().position(|(k, _)| k == &key) {
            let (_, entry) = cache.remove(pos);
            let resp = entry.response.clone();
            cache.push((key, entry));
            return Ok(resp);
        }
    }

    let _permit = SEARCH_SLOTS
        .acquire()
        .await
        .map_err(|e| PlayerError::Io(e.to_string()))?;

    let validated_providers = validate_search_providers(&req.providers)?;

    // 1. Library provider
    let lib_items = search_library(&req.library, &query);
    let mut statuses = vec![json!({
        "id": "library",
        "name": "My library",
        "status": "ok",
        "count": lib_items.len(),
    })];
    let mut raw_results = lib_items;

    // 2. External user-JSON providers in parallel
    let mut tasks = Vec::with_capacity(validated_providers.len());
    for cfg in validated_providers {
        let q = query.clone();
        let allow_lan = req.allow_lan;
        tasks.push(tokio::spawn(async move {
            let outcome = query_user_json_provider(&cfg, &q, allow_lan).await;
            (cfg, outcome)
        }));
    }

    for task in tasks {
        if let Ok((cfg, outcome)) = task.await {
            match outcome {
                Ok(items) => {
                    statuses.push(json!({
                        "id": cfg.id,
                        "name": cfg.name,
                        "status": "ok",
                        "count": items.len(),
                    }));
                    raw_results.extend(items);
                }
                Err((status_kind, err_msg)) => {
                    statuses.push(json!({
                        "id": cfg.id,
                        "name": cfg.name,
                        "status": status_kind,
                        "count": 0,
                        "error": err_msg,
                    }));
                }
            }
        }
    }

    let mut deduped = deduplicate_results(raw_results, &query);

    // Pre-sort before availability probing so the top MAX_SEARCH_RESULTS candidates get probed.
    sort_results(&mut deduped, preferred.as_deref());

    if req.check_availability && !deduped.is_empty() {
        let probe_count = deduped.len().min(MAX_SEARCH_RESULTS);
        let targets: Vec<ProbeTarget> = deduped[..probe_count]
            .iter()
            .map(|r| ProbeTarget {
                url: r.url.clone(),
                headers: r.headers.clone(),
            })
            .collect();

        let outcomes =
            probe_targets_bounded(targets, req.allow_lan, availability_deadline, |_, _| {}).await;

        for (item, probe_opt) in deduped[..probe_count].iter_mut().zip(outcomes) {
            match probe_opt {
                Some(probe) => {
                    item.available = Some(probe.live);
                    item.status = probe.status;
                    item.content_type = probe.content_type;
                    if item.quality.is_none()
                        && let Some(ref ct) = item.content_type
                    {
                        let lower = ct.to_ascii_lowercase();
                        if lower.contains("mpegurl") || lower.contains("vnd.apple") {
                            item.quality = Some("adaptive".to_string());
                        }
                    }
                }
                None => {
                    // Overall availability deadline expired for this host: keep available = None (null in JSON)
                    item.available = None;
                    item.status = None;
                    item.content_type = None;
                }
            }
        }
    }

    sort_results(&mut deduped, preferred.as_deref());
    deduped.truncate(MAX_SEARCH_RESULTS);

    let results_json: Vec<Value> = deduped
        .into_iter()
        .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
        .collect();

    let response = SearchResponse {
        query,
        results: results_json,
        providers: statuses,
    };

    {
        let mut cache = SEARCH_CACHE.lock().await;
        cache.push((
            key,
            CacheEntry {
                expires_at: Instant::now() + SEARCH_CACHE_TTL,
                response: response.clone(),
            },
        ));
        while cache.len() > MAX_CACHE_ENTRIES {
            cache.remove(0);
        }
    }

    Ok(response)
}

pub async fn execute_search(req: &SearchRequest) -> Result<SearchResponse> {
    execute_search_with_deadline(req, DEFAULT_AVAILABILITY_DEADLINE).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn provider_config_requires_safe_http_endpoint_and_validates_limits() {
        assert!(validate_provider_config(&json!({
            "id": "bad",
            "name": "Bad",
            "endpoint": "file:///etc/passwd"
        }))
        .is_err());
        assert!(validate_provider_config(&json!({
            "id": "bad",
            "name": "Bad",
            "endpoint": "https://user:pass@example.com/search"
        }))
        .is_err());

        let valid = validate_provider_config(&json!({
            "id": "json",
            "name": "JSON",
            "endpoint": "https://example.com/search",
            "timeout": 100
        }))
        .unwrap();
        assert_eq!(valid.timeout, 15.0);
    }

    #[test]
    fn provider_registry_rejects_duplicates_and_caps_count() {
        let cfg = json!({
            "id": "remote",
            "name": "Remote",
            "endpoint": "https://example.test/search"
        });
        assert!(validate_search_providers(&[cfg.clone(), cfg.clone()]).is_err());
        let too_many: Vec<Value> = (0..13)
            .map(|i| {
                json!({
                    "id": format!("p{i}"),
                    "name": "Remote",
                    "endpoint": "https://example.test/search"
                })
            })
            .collect();
        assert!(validate_search_providers(&too_many).is_err());
    }

    #[test]
    fn library_provider_matches_title_aliases_and_preserves_metadata() {
        let entries = vec![json!({
            "id": 5,
            "playlist_id": 3,
            "name": "The Example Movie (1080p)",
            "url": "https://cdn.example/movie.m3u8",
            "group_title": "Films; Drama",
            "release_year": 2024,
            "tmdb_type": "movie",
            "extra_headers": "{\"Referer\":\"https://example.com\"}"
        })];
        let results = search_library(&entries, "drama example");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].channel_id, Some(5));
        assert_eq!(results[0].media_type.as_deref(), Some("movie"));
        assert_eq!(
            results[0].headers.get("Referer").map(String::as_str),
            Some("https://example.com")
        );
    }

    #[test]
    fn title_deduplication_merges_provider_alias_and_metadata() {
        let items = vec![
            SearchResultItem {
                id: "library:1".into(),
                title: "Example Film".into(),
                url: "https://a.test/movie.m3u8".into(),
                provider: "Library".into(),
                providers: vec![],
                provider_sources: vec![],
                aliases: vec!["Example".into()],
                year: Some(2020),
                media_type: Some("movie".into()),
                quality: None,
                language: None,
                headers: HashMap::new(),
                channel_id: Some(1),
                playlist_id: Some(2),
                group_title: None,
                available: None,
                status: None,
                content_type: None,
                score: 0,
                context: vec![],
                normalized_title: String::new(),
                normalized_aliases: vec![],
                quality_rank: 0,
            },
            SearchResultItem {
                id: "json:2".into(),
                title: "Example".into(),
                url: "https://b.test/movie.m3u8".into(),
                provider: "Remote".into(),
                providers: vec![],
                provider_sources: vec![],
                aliases: vec!["Example Film".into()],
                year: Some(2020),
                media_type: Some("movie".into()),
                quality: Some("1080p".into()),
                language: Some("en".into()),
                headers: HashMap::from([("Referer".into(), "https://source.test".into())]),
                channel_id: None,
                playlist_id: None,
                group_title: None,
                available: None,
                status: None,
                content_type: None,
                score: 0,
                context: vec![],
                normalized_title: String::new(),
                normalized_aliases: vec![],
                quality_rank: 0,
            },
        ];

        let merged = deduplicate_results(items, "Example Film");
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].providers, vec!["Library", "Remote"]);
        assert_eq!(merged[0].channel_id, Some(1));
        assert_eq!(merged[0].quality.as_deref(), Some("1080p"));
        assert_eq!(merged[0].language.as_deref(), Some("en"));
        assert_eq!(
            merged[0].headers.get("Referer").map(String::as_str),
            Some("https://source.test")
        );
    }

    #[test]
    fn shared_group_title_does_not_merge_unrelated_channels() {
        let entries = vec![
            json!({"id": 1, "name": "Alpha Channel", "url": "https://a.test/1.m3u8", "group_title": "UK"}),
            json!({"id": 2, "name": "Beta Channel", "url": "https://b.test/2.m3u8", "group_title": "UK"}),
        ];
        let rows = search_library(&entries, "channel");
        let deduped = deduplicate_results(rows, "channel");
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn same_title_different_year_or_media_type_is_not_merged() {
        let make = |url: &str, provider: &str, year: i64, mtype: &str| SearchResultItem {
            id: format!("{provider}:1"),
            title: "Example".into(),
            url: url.into(),
            provider: provider.into(),
            providers: vec![],
            provider_sources: vec![],
            aliases: vec![],
            year: Some(year),
            media_type: Some(mtype.into()),
            quality: None,
            language: None,
            headers: HashMap::new(),
            channel_id: None,
            playlist_id: None,
            group_title: None,
            available: None,
            status: None,
            content_type: None,
            score: 0,
            context: vec![],
            normalized_title: String::new(),
            normalized_aliases: vec![],
            quality_rank: 0,
        };
        let rows = deduplicate_results(
            vec![
                make("https://a.test/1", "A", 2020, "movie"),
                make("https://b.test/2", "B", 2021, "movie"),
                make("https://c.test/3", "C", 2020, "series"),
            ],
            "Example",
        );
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn language_labels_normalize_to_primary_codes() {
        assert_eq!(normalize_language(Some("English")).as_deref(), Some("en"));
        assert_eq!(normalize_language(Some("en-US")).as_deref(), Some("en"));
        assert_eq!(normalize_language(Some("fre")).as_deref(), Some("fr"));
        assert_eq!(normalize_language(Some("English (US)")).as_deref(), Some("en"));
        assert_eq!(normalize_language(Some("pt_BR")).as_deref(), Some("pt"));
        assert_eq!(normalize_language(Some("")), None);
        assert_eq!(normalize_language(None), None);
        assert_eq!(normalize_language(Some("??? <b>junk</b>")), None);
    }

    #[test]
    fn user_json_provider_maps_configured_fields() {
        let cfg = validate_provider_config(&json!({
            "id": "remote",
            "name": "Remote",
            "endpoint": "https://provider.test/search",
            "results_path": "data",
            "title_field": "name",
            "url_field": "stream",
            "aliases_field": "aka",
            "year_field": "released",
            "quality_field": "resolution",
            "language_field": "lang"
        }))
        .unwrap();
        let payload = json!({
            "data": [{
                "name": "Example Film",
                "stream": "https://cdn.example/movie.m3u8",
                "aka": ["Example"],
                "released": "2021",
                "resolution": "1080p",
                "lang": "en"
            }]
        });
        let found = parse_user_json_provider_payload(&cfg, &payload).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].year, Some(2021));
        assert_eq!(found[0].quality.as_deref(), Some("1080p"));
        assert_eq!(found[0].language.as_deref(), Some("en"));
    }

    #[test]
    fn benchmark_dedupe_5000_results_under_200ms() {
        let mut input = Vec::with_capacity(5000);
        for i in 0..5000 {
            // 2,500 distinct titles, each appearing twice across providers
            let title_num = i % 2500;
            let title = format!("Cinema Feature Title {title_num} (1080p)");
            let norm = normalize_text(&title);
            input.push(SearchResultItem {
                id: format!("prov:{i}"),
                title,
                url: format!("https://cdn.example.org/stream/{i}.m3u8"),
                provider: if i < 2500 { "Library".into() } else { "Remote".into() },
                providers: Vec::new(),
                provider_sources: Vec::new(),
                aliases: Vec::new(),
                year: Some(2000 + (title_num as i64 % 25)),
                media_type: Some("movie".into()),
                quality: Some("1080p".into()),
                language: Some("en".into()),
                headers: HashMap::new(),
                channel_id: if i < 2500 { Some(i as i64) } else { None },
                playlist_id: Some(1),
                group_title: Some("Movies".into()),
                available: None,
                status: None,
                content_type: None,
                score: 0,
                context: Vec::new(),
                normalized_title: norm,
                normalized_aliases: Vec::new(),
                quality_rank: 0,
            });
        }

        let started = Instant::now();
        let deduped = deduplicate_results(input, "cinema feature");
        let elapsed = started.elapsed();

        assert_eq!(deduped.len(), 2500);
        assert!(
            elapsed < Duration::from_millis(200),
            "5,000 results deduped in {elapsed:?}, expected < 200ms"
        );
    }

    #[tokio::test]
    async fn search_with_80_unresponsive_hosts_returns_partial_results_within_deadline() {
        clear_search_cache().await;

        // One blackhole TCP listener that accepts connections and never responds.
        let blackhole = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let blackhole_port = blackhole.local_addr().unwrap().port();
        let bh_task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = blackhole.accept().await {
                held.push(s);
            }
        });

        // One fast local HTTP server that answers 200 OK immediately.
        let live_srv = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live_port = live_srv.local_addr().unwrap().port();
        let live_task = tokio::spawn(async move {
            while let Ok((mut s, _)) = live_srv.accept().await {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });

        let mut library = Vec::with_capacity(81);
        // 1 responsive entry with an exact title match (score 100)
        library.push(json!({
            "id": 1,
            "playlist_id": 1,
            "name": "Target Stream",
            "url": format!("http://127.0.0.1:{live_port}/live.m3u8"),
        }));
        // 80 unresponsive hosts
        for i in 0..80 {
            library.push(json!({
                "id": 100 + i,
                "playlist_id": 1,
                "name": format!("Target Stream Mirror {i}"),
                "url": format!("http://127.0.0.1:{blackhole_port}/dead-{i}.m3u8"),
            }));
        }

        let req = SearchRequest {
            query: "Target Stream".into(),
            library,
            providers: vec![],
            check_availability: true,
            preferred_language: None,
            allow_lan: true,
        };

        let started = Instant::now();
        let resp = execute_search_with_deadline(&req, Duration::from_millis(500))
            .await
            .expect("search should succeed with partial availability");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "search finished within bounded deadline ({elapsed:?})"
        );
        assert_eq!(resp.results.len(), 80);
        // The responsive stream finished its probe and ranks first with available = true.
        assert_eq!(resp.results[0]["available"], json!(true));
        assert_eq!(resp.results[0]["quality"], json!("adaptive"));
        // Unresponsive streams timed out at the overall deadline and have available = null.
        assert!(
            resp.results.iter().skip(1).any(|r| r["available"].is_null()),
            "unresponsive hosts should return available = null"
        );

        bh_task.abort();
        live_task.abort();
    }
}
