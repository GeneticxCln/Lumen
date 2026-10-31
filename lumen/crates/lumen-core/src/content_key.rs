//! Title identity and canonical content keys (Stremio core model).
//!
//! Separates title identity from disposable stream URLs so favorites, history,
//! and resume state survive stream replacements, source changes, and cross-device sync.
//!
//! Formats:
//! - `tmdb:movie:<id>`
//! - `tmdb:tv:<id>:S<season>E<episode>`
//! - `tvg:<tvg_id>`
//! - `file:<fingerprint>`
//! - `stream2:<sha256_of_url_without_volatile_query_parameters>`

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

static EPISODE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bS(\d{1,2})[._\-\s]*E(\d{1,3})\b").expect("valid episode regex")
});

/// `tvg-id` values that providers use as "no id". Treating them as real identities merged
/// unrelated channels (audit issue 7).
const PLACEHOLDER_TVG_IDS: &[&str] = &[
    "none",
    "null",
    "nil",
    "n/a",
    "na",
    "-",
    "unknown",
    "undefined",
];

/// Query parameters whose value changes per session and must not become part of the identity.
/// Everything else in the query string *does* distinguish streams (`?id=1` != `?id=2`).
const VOLATILE_QUERY_MARKERS: &[&str] = &[
    "token",
    "secret",
    "signature",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "expires",
    "expiry",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ContentKey(String);

impl ContentKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    pub fn for_movie(tmdb_id: i64) -> Self {
        Self(format!("tmdb:movie:{tmdb_id}"))
    }

    pub fn for_series_episode(tmdb_id: i64, season: u32, episode: u32) -> Self {
        Self(format!("tmdb:tv:{tmdb_id}:S{season:02}E{episode:02}"))
    }

    /// Falls back to this when a title carries no parseable `SxxExx` marker, so episodes that
    /// can't be numbered still get distinct keys instead of collapsing onto a single slot.
    pub fn for_series_episode_by_title(tmdb_id: i64, title: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(title.trim().to_lowercase().as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        Self(format!("tmdb:tv:{tmdb_id}:ep:{}", &hash[..16]))
    }

    pub fn for_tvg(tvg_id: &str) -> Option<Self> {
        Self::for_tvg_impl(tvg_id, false)
    }

    fn for_tvg_impl(tvg_id: &str, allow_placeholder: bool) -> Option<Self> {
        let trimmed = tvg_id.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_lowercase();
        if !allow_placeholder && PLACEHOLDER_TVG_IDS.contains(&lower.as_str()) {
            return None;
        }
        Some(Self(format!("tvg:{lower}")))
    }

    pub fn for_file(path: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(path.trim().as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        Self(format!("file:{}", &hash[..16]))
    }

    /// The v2 stream fallback. Query parameters are part of the identity except for clearly
    /// volatile/credential ones, which are stripped so a rotating `?token=` does not look like a
    /// new stream. Fragments never reach a server and are ignored.
    pub fn for_stream_url(url: &str) -> Self {
        Self::stream_key(&canonical_stream_url(url), "stream2")
    }

    /// Reproduces the pre-v2 stream key exactly. Only the migration and its tests may use this.
    pub(crate) fn for_stream_url_legacy(url: &str) -> Self {
        let base = url.split(['?', '#']).next().unwrap_or(url).trim();
        Self::stream_key(base, "stream")
    }

    fn stream_key(base: &str, prefix: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(base.as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        Self(format!("{prefix}:{}", &hash[..16]))
    }

    /// Derives the best available content key from available metadata.
    pub fn derive(
        tmdb_id: Option<i64>,
        tmdb_type: Option<&str>,
        title_or_name: &str,
        tvg_id: Option<&str>,
        url_or_path: &str,
    ) -> Self {
        Self::derive_with(
            tmdb_id,
            tmdb_type,
            title_or_name,
            tvg_id,
            url_or_path,
            false,
        )
    }

    /// The exact pre-v2 derivation, for rekeying rows that were written before the migration.
    pub(crate) fn derive_legacy(
        tmdb_id: Option<i64>,
        tmdb_type: Option<&str>,
        title_or_name: &str,
        tvg_id: Option<&str>,
        url_or_path: &str,
    ) -> Self {
        Self::derive_with(tmdb_id, tmdb_type, title_or_name, tvg_id, url_or_path, true)
    }

    fn derive_with(
        tmdb_id: Option<i64>,
        tmdb_type: Option<&str>,
        title_or_name: &str,
        tvg_id: Option<&str>,
        url_or_path: &str,
        legacy: bool,
    ) -> Self {
        if let Some(id) = tmdb_id {
            match tmdb_type {
                Some("movie") => return Self::for_movie(id),
                Some("tv" | "series") => {
                    return match parse_season_episode(title_or_name) {
                        Some((season, ep)) => Self::for_series_episode(id, season, ep),
                        None => Self::for_series_episode_by_title(id, title_or_name),
                    };
                }
                _ => return Self::for_movie(id),
            }
        }
        if let Some(tvg) = tvg_id.and_then(|id| Self::for_tvg_impl(id, legacy)) {
            return tvg;
        }
        if !url_or_path.starts_with("http://") && !url_or_path.starts_with("https://") {
            return Self::for_file(url_or_path);
        }
        if legacy {
            Self::for_stream_url_legacy(url_or_path)
        } else {
            Self::for_stream_url(url_or_path)
        }
    }
}

/// Drops the fragment and volatile query parameters while preserving every parameter that can
/// identify a stream. Deliberately string-based: re-serializing through a URL parser would
/// normalize escaping and change keys for URLs the provider minted byte-for-byte.
fn canonical_stream_url(url: &str) -> String {
    let raw = url.trim();
    let without_fragment = raw.split('#').next().unwrap_or(raw);
    let Some((base, query)) = without_fragment.split_once('?') else {
        return without_fragment.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or("");
            !pair.is_empty() && !is_volatile_query_param(name)
        })
        .collect();
    if kept.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

fn is_volatile_query_param(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    matches!(name.as_str(), "key" | "auth" | "sig" | "exp" | "pass")
        || name.ends_with("_key")
        || name.ends_with("_auth")
        || VOLATILE_QUERY_MARKERS
            .iter()
            .any(|marker| name.contains(marker))
}

pub fn parse_season_episode(text: &str) -> Option<(u32, u32)> {
    let caps = EPISODE_RE.captures(text)?;
    let s: u32 = caps.get(1)?.as_str().parse().ok()?;
    let e: u32 = caps.get(2)?.as_str().parse().ok()?;
    Some((s, e))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleSource {
    pub id: Option<i64>,
    pub content_key: String,
    pub channel_id: Option<i64>,
    pub stream_url: String,
    pub rank: i32,
    pub quality: Option<String>,
    pub headers: Option<String>,
    pub is_live: Option<bool>,
    pub last_verified: Option<i64>,
}

/// A user's manual Kind/Genre correction for a title TMDB never matched. See
/// `migrations/011_category_overrides.sql`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryOverride {
    pub content_key: String,
    pub kind: Option<String>,
    pub genres: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Title {
    pub content_key: String,
    pub kind: String,
    pub title: String,
    pub tmdb_id: Option<i64>,
    pub tmdb_type: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub release_year: Option<i64>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub overview: Option<String>,
    pub genres: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_key_derivation() {
        let movie_key = ContentKey::derive(
            Some(550),
            Some("movie"),
            "Fight Club (1999)",
            None,
            "https://cdn.example.com/movie.m3u8",
        );
        assert_eq!(movie_key.as_str(), "tmdb:movie:550");

        let series_key = ContentKey::derive(
            Some(1399),
            Some("tv"),
            "Game of Thrones S01E03 Lord Snow",
            None,
            "https://cdn.example.com/got.m3u8",
        );
        assert_eq!(series_key.as_str(), "tmdb:tv:1399:S01E03");

        let tvg_key = ContentKey::derive(
            None,
            None,
            "BBC One HD",
            Some("bbc.one.uk"),
            "https://live.example.com/bbc1.m3u8",
        );
        assert_eq!(tvg_key.as_str(), "tvg:bbc.one.uk");

        let stream_key = ContentKey::derive(
            None,
            None,
            "Random Stream",
            None,
            "https://cdn.example.com/stream.m3u8?token=secret#hdr",
        );
        assert!(stream_key.as_str().starts_with("stream2:"));
    }

    #[test]
    fn query_parameters_distinguish_streams() {
        let one = ContentKey::derive(
            None,
            None,
            "Channel",
            None,
            "https://cdn.example/play.php?id=1",
        );
        let two = ContentKey::derive(
            None,
            None,
            "Channel",
            None,
            "https://cdn.example/play.php?id=2",
        );
        assert_ne!(one, two, "?id=1 and ?id=2 are different streams");
        assert!(one.as_str().starts_with("stream2:"));
    }

    #[test]
    fn volatile_query_parameters_do_not_change_the_key() {
        let first = ContentKey::derive(
            None,
            None,
            "Channel",
            None,
            "https://cdn.example/live.m3u8?id=7&token=aaa&expires=100#t=10",
        );
        let second = ContentKey::derive(
            None,
            None,
            "Channel",
            None,
            "https://cdn.example/live.m3u8?id=7&token=bbb&expires=200#t=99",
        );
        assert_eq!(first, second, "rotating credentials must keep one identity");

        let other_id = ContentKey::derive(
            None,
            None,
            "Channel",
            None,
            "https://cdn.example/live.m3u8?id=8&token=aaa",
        );
        assert_ne!(
            first, other_id,
            "a stable parameter still identifies the stream"
        );
    }

    #[test]
    fn placeholder_tvg_ids_fall_through_to_the_stream() {
        for placeholder in ["None", "null", " N/A ", "-", "unknown"] {
            let key = ContentKey::derive(
                None,
                None,
                "Channel",
                Some(placeholder),
                "https://cdn.example/a.m3u8",
            );
            assert!(
                key.as_str().starts_with("stream2:"),
                "{placeholder:?} must not become a shared tvg identity"
            );
        }
        assert_eq!(
            ContentKey::derive(
                None,
                None,
                "BBC One",
                Some("bbc.one.uk"),
                "https://cdn.example/a.m3u8",
            )
            .as_str(),
            "tvg:bbc.one.uk"
        );
    }

    #[test]
    fn legacy_key_reproduces_the_pre_migration_derivation() {
        let legacy = ContentKey::derive_legacy(
            None,
            None,
            "Channel",
            None,
            "https://cdn.example/play.php?id=1&token=abc#frag",
        );
        assert!(legacy.as_str().starts_with("stream:"));
        assert!(!legacy.as_str().starts_with("stream2:"));

        let legacy_tvg = ContentKey::derive_legacy(
            None,
            None,
            "Channel",
            Some("None"),
            "https://cdn.example/play.php?id=1",
        );
        assert_eq!(legacy_tvg.as_str(), "tvg:none");
    }

    #[test]
    fn tv_episodes_without_a_season_marker_get_distinct_keys() {
        let a = ContentKey::derive(
            Some(1399),
            Some("tv"),
            "Game of Thrones - Winter Is Coming",
            None,
            "https://cdn.example.com/got1.m3u8",
        );
        let b = ContentKey::derive(
            Some(1399),
            Some("tv"),
            "Game of Thrones - The Kingsroad",
            None,
            "https://cdn.example.com/got2.m3u8",
        );
        assert_ne!(
            a, b,
            "two different unnumbered episodes must not collapse to one key"
        );
        assert!(a.as_str().starts_with("tmdb:tv:1399:ep:"));

        let a_again = ContentKey::derive(
            Some(1399),
            Some("tv"),
            "Game of Thrones - Winter Is Coming",
            None,
            "https://cdn.example.com/got1-mirror.m3u8",
        );
        assert_eq!(
            a, a_again,
            "the same title must still resolve to a stable key"
        );
    }

    #[test]
    fn test_parse_season_episode() {
        assert_eq!(parse_season_episode("Show S02E05 Title"), Some((2, 5)));
        assert_eq!(parse_season_episode("Show.s01.e10.720p"), Some((1, 10)));
        assert_eq!(parse_season_episode("Movie Title (2020)"), None);
    }
}
