//! Stremio's public OpenSubtitles mirror (`opensubtitles-v3.strem.io`). It answers plain GET
//! requests, no key, for a movie or a series episode by IMDb id, and optionally narrows by the
//! file's OpenSubtitles hash. Its download addresses serve UTF-8 text on `*.strem.io`.
//!
//! This is the same service Stremio's own app uses, but nothing promises it to other programs.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use super::http::{self, Guard};
use super::lang;
use super::store::MAX_SUBTITLE_BYTES;
use super::{Provider, SubtitleCandidate, SubtitleQuery};
use crate::error::{PlayerError, Result};

pub const ID: &str = "opensubtitles-mirror";
const BASE: &str = "https://opensubtitles-v3.strem.io";
const HOSTS: &[&str] = &["strem.io"];

pub struct StremioMirror {
    base: String,
    client: reqwest::Client,
    guard: Guard,
}

impl StremioMirror {
    pub fn new(allow_lan: bool) -> Result<Self> {
        Ok(Self {
            base: BASE.into(),
            client: http::client(allow_lan)?,
            guard: Guard::new(HOSTS),
        })
    }

    #[cfg(test)]
    fn for_tests(base: &str, host: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').into(),
            client: http::client(true).unwrap(),
            guard: Guard::insecure_for_tests(&[host]),
        }
    }
}

fn valid_imdb_id(id: &str) -> bool {
    id.strip_prefix("tt").is_some_and(|digits| {
        (6..=10).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit())
    })
}

/// `…/subtitles/movie/tt0111161.json`, or for an episode
/// `…/subtitles/series/tt11280740:1:1/videoHash=…&videoSize=….json`. Only validated pieces go
/// into the address, so nothing from a file name can change its shape.
fn search_url(base: &str, query: &SubtitleQuery) -> Option<String> {
    let imdb = query.imdb_id.as_deref().filter(|id| valid_imdb_id(id))?;
    let mut path = if query.is_episode() {
        format!(
            "{base}/subtitles/series/{imdb}:{}:{}",
            query.season?, query.episode?
        )
    } else {
        format!("{base}/subtitles/movie/{imdb}")
    };
    if let (Some(hash), Some(size)) = (&query.file_hash, query.file_size)
        && hash.len() == 16
        && hash.chars().all(|c| c.is_ascii_hexdigit())
    {
        path.push_str(&format!("/videoHash={hash}&videoSize={size}"));
    }
    path.push_str(".json");
    Some(path)
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    subtitles: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: Value,
    url: String,
    lang: String,
    /// How the service matched it; `f` is taken to mean the file hash (only `i`, the IMDb id,
    /// has been seen). If that guess is wrong the only cost is ranking.
    #[serde(default)]
    m: Option<String>,
    #[serde(rename = "subtitleFileName", default)]
    file_name: Option<String>,
    #[serde(rename = "movieReleaseName", default)]
    release_name: Option<String>,
    #[serde(rename = "fpsMilli", default)]
    fps_milli: Option<u32>,
    #[serde(default)]
    season: Option<u32>,
    #[serde(default)]
    episode: Option<u32>,
}

fn candidates(answer: Answer, query: &SubtitleQuery) -> Vec<SubtitleCandidate> {
    answer
        .subtitles
        .into_iter()
        .filter_map(|item| {
            let language = lang::normalize(&item.lang)?;
            if !query.languages.is_empty() && !query.languages.iter().any(|code| code == language) {
                return None;
            }
            // An episode search can come back with other episodes' files; a movie has season 0.
            if let (Some(season), Some(episode)) = (query.season, query.episode)
                && item.season.unwrap_or(0) != 0
                && (item.season != Some(season) || item.episode != Some(episode))
            {
                return None;
            }
            let id = match &item.id {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                _ => return None,
            };
            Some(SubtitleCandidate {
                provider: ID.into(),
                id,
                language: language.into(),
                release: item.release_name.or(item.file_name).unwrap_or_default(),
                hearing_impaired: false,
                downloads: None,
                fps: item
                    .fps_milli
                    .filter(|fps| *fps > 0)
                    .map(|fps| fps as f64 / 1000.0),
                hash_match: item.m.as_deref() == Some("f"),
                score: 0,
                url: item.url,
            })
        })
        .collect()
}

#[async_trait]
impl Provider for StremioMirror {
    fn id(&self) -> &'static str {
        ID
    }

    async fn search(&self, query: &SubtitleQuery) -> Result<Vec<SubtitleCandidate>> {
        let Some(url) = search_url(&self.base, query) else {
            return Ok(Vec::new());
        };
        let url = Url::parse(&url)
            .map_err(|_| PlayerError::InvalidArg("bad subtitle search address".into()))?;
        let answer: Answer = http::get_json(&self.client, &self.guard, &url).await?;
        Ok(candidates(answer, query))
    }

    async fn download(&self, candidate: &SubtitleCandidate) -> Result<Vec<u8>> {
        let url = self.guard.parse(&candidate.url)?;
        http::get_bytes(&self.client, &self.guard, &url, MAX_SUBTITLE_BYTES).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitles::testing::{Canned, serve};

    /// Trimmed from real answers of the service (the second item is from a series lookup).
    const FIXTURE: &str = r#"{"subtitles":[
      {"id":"30949","url":"__BASE__/en/download/subencoding-stremio-utf8/src-api/file/36919","SubEncoding":"CP1252","lang":"eng","m":"i","g":"11","subtitleFileName":"sawshank_redemption_engl.srt","movieReleaseName":"Shawshank Redemption, The (1994)","fpsMilli":23980,"season":0,"episode":0},
      {"id":9136316,"url":"__BASE__/x/9136316","SubEncoding":"UTF-8","lang":"fre","m":"f","subtitleFileName":"Severance.S01E01.srt","movieReleaseName":"Severance.S01E01.Good.News.About.Hell.2160p-TEPES","fpsMilli":23976,"season":1,"episode":1},
      {"id":"1","url":"__BASE__/x/1","lang":"ger","season":1,"episode":2,"movieReleaseName":"other episode"},
      {"id":"2","url":"__BASE__/x/2","lang":"tlh","movieReleaseName":"unknown language"}
    ]}"#;

    fn movie() -> SubtitleQuery {
        SubtitleQuery {
            title: "The Shawshank Redemption".into(),
            imdb_id: Some("tt0111161".into()),
            languages: vec!["en".into(), "fr".into()],
            ..Default::default()
        }
    }

    #[test]
    fn addresses_are_built_from_validated_pieces_only() {
        let base = "https://opensubtitles-v3.strem.io";
        assert_eq!(
            search_url(base, &movie()).unwrap(),
            "https://opensubtitles-v3.strem.io/subtitles/movie/tt0111161.json"
        );
        let mut episode = movie();
        episode.imdb_id = Some("tt11280740".into());
        episode.season = Some(1);
        episode.episode = Some(2);
        episode.file_hash = Some("8e245d9679d31e12".into());
        episode.file_size = Some(12909756);
        assert_eq!(
            search_url(base, &episode).unwrap(),
            "https://opensubtitles-v3.strem.io/subtitles/series/tt11280740:1:2/videoHash=8e245d9679d31e12&videoSize=12909756.json"
        );
        // Nothing is searched without a valid id, and a bad hash is left out, never passed on.
        for bad in [
            None,
            Some("tt1"),
            Some("../../etc"),
            Some("tt0111161/../x"),
            Some("0111161"),
        ] {
            let mut query = movie();
            query.imdb_id = bad.map(str::to_string);
            assert!(search_url(base, &query).is_none(), "{bad:?}");
        }
        let mut hostile = movie();
        hostile.file_hash = Some("zz/../..".into());
        hostile.file_size = Some(1);
        assert_eq!(
            search_url(base, &hostile).unwrap(),
            "https://opensubtitles-v3.strem.io/subtitles/movie/tt0111161.json"
        );
    }

    #[test]
    fn answers_are_filtered_to_the_language_and_episode_asked_for() {
        let answer: Answer =
            serde_json::from_str(&FIXTURE.replace("__BASE__", "https://subs5.strem.io")).unwrap();
        let mut query = movie();
        query.season = Some(1);
        query.episode = Some(1);
        let found = candidates(answer, &query);
        assert_eq!(
            found.len(),
            2,
            "other episode and unknown language dropped: {found:#?}"
        );
        assert_eq!(
            (found[0].id.as_str(), found[0].language.as_str()),
            ("30949", "en")
        );
        assert_eq!(found[0].fps, Some(23.98));
        assert!(!found[0].hash_match);
        assert_eq!(
            (found[1].id.as_str(), found[1].language.as_str()),
            ("9136316", "fr")
        );
        assert!(found[1].hash_match);
    }

    #[tokio::test]
    async fn searches_and_downloads_through_the_guard() {
        let server = serve(vec![
            Canned::json("/subtitles/movie/tt0111161.json", FIXTURE),
            Canned::text(
                "/en/download/subencoding-stremio-utf8/src-api/file/36919",
                "1\n00:00:01,000 --> 00:00:02,000\nHello\n",
            ),
        ])
        .await;
        let provider = StremioMirror::for_tests(&server.base, "127.0.0.1");
        let found = provider.search(&movie()).await.unwrap();
        assert_eq!(found.len(), 2, "{found:#?}");
        let bytes = provider.download(&found[0]).await.unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("Hello"));
        assert_eq!(server.hits(), 2);

        // A candidate that points anywhere else is refused before any request is made.
        let mut forged = found[0].clone();
        forged.url = "https://evil.example/steal".into();
        assert!(provider.download(&forged).await.is_err());
        assert_eq!(server.hits(), 2, "the forged address was never requested");
    }

    #[tokio::test]
    async fn a_missing_id_means_no_request_at_all() {
        let server = serve(vec![]).await;
        let provider = StremioMirror::for_tests(&server.base, "127.0.0.1");
        let mut query = movie();
        query.imdb_id = None;
        assert!(provider.search(&query).await.unwrap().is_empty());
        assert_eq!(server.hits(), 0);
    }

    #[tokio::test]
    async fn a_service_error_is_an_error_not_an_empty_list() {
        let server = serve(vec![Canned::status("/subtitles/movie/tt0111161.json", 503)]).await;
        let provider = StremioMirror::for_tests(&server.base, "127.0.0.1");
        let error = provider.search(&movie()).await.unwrap_err().to_string();
        assert!(error.contains("503"), "{error}");
    }
}
