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
//! - `stream:<sha256_of_url>`

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

static EPISODE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bS(\d{1,2})[._\-\s]*E(\d{1,3})\b").expect("valid episode regex")
});

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
        let trimmed = tvg_id.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(Self(format!("tvg:{}", trimmed.to_lowercase())))
        }
    }

    pub fn for_file(path: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(path.trim().as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        Self(format!("file:{}", &hash[..16]))
    }

    pub fn for_stream_url(url: &str) -> Self {
        // Strip query and fragment for base stream canonical key
        let base = url.split(['?', '#']).next().unwrap_or(url).trim();
        let mut hasher = Sha256::new();
        hasher.update(base.as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        Self(format!("stream:{}", &hash[..16]))
    }

    /// Derives the best available content key from available metadata
    pub fn derive(
        tmdb_id: Option<i64>,
        tmdb_type: Option<&str>,
        title_or_name: &str,
        tvg_id: Option<&str>,
        url_or_path: &str,
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
        if let Some(tvg) = tvg_id.and_then(Self::for_tvg) {
            return tvg;
        }
        if !url_or_path.starts_with("http://") && !url_or_path.starts_with("https://") {
            return Self::for_file(url_or_path);
        }
        Self::for_stream_url(url_or_path)
    }
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
        assert!(stream_key.as_str().starts_with("stream:"));
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
