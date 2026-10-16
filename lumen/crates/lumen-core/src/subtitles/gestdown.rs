//! Gestdown (`api.gestdown.info`), a community mirror of Addic7ed for TV episodes. It needs no
//! key: find the show by name, ask for an episode in a language, download by id. Movies are not
//! covered.

use async_trait::async_trait;
use serde::Deserialize;
use url::Url;

use super::http::{self, Guard};
use super::lang;
use super::store::MAX_SUBTITLE_BYTES;
use super::{Provider, SubtitleCandidate, SubtitleQuery};
use crate::error::{PlayerError, Result};
use crate::tmdb::normalize_title;

pub const ID: &str = "gestdown";
const BASE: &str = "https://api.gestdown.info";
const HOSTS: &[&str] = &["gestdown.info"];
/// Languages asked for at most, so one search is a handful of requests.
const MAX_LANGUAGES: usize = 3;

pub struct Gestdown {
    base: String,
    client: reqwest::Client,
    guard: Guard,
}

impl Gestdown {
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

    fn url(&self, segments: &[&str]) -> Result<Url> {
        let mut url = Url::parse(&self.base)
            .map_err(|_| PlayerError::InvalidArg("bad subtitle service address".into()))?;
        url.path_segments_mut()
            .map_err(|_| PlayerError::InvalidArg("bad subtitle service address".into()))?
            .extend(segments);
        Ok(url)
    }
}

#[derive(Deserialize)]
struct Shows {
    #[serde(default)]
    shows: Vec<Show>,
}

#[derive(Deserialize)]
struct Show {
    id: String,
    name: String,
    #[serde(rename = "tmdbId", default)]
    tmdb_id: Option<i64>,
}

/// The show that is the one asked for: the same TMDB id, otherwise the same normalised name.
fn pick_show<'a>(shows: &'a [Show], query: &SubtitleQuery) -> Option<&'a Show> {
    if let Some(tmdb) = query.tmdb_id
        && let Some(found) = shows.iter().find(|show| show.tmdb_id == Some(tmdb))
    {
        return Some(found);
    }
    let wanted = normalize_title(&query.title);
    shows
        .iter()
        .find(|show| normalize_title(&show.name) == wanted)
}

#[derive(Deserialize)]
struct Episode {
    #[serde(rename = "matchingSubtitles", default)]
    subtitles: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(rename = "subtitleId")]
    id: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    completed: bool,
    #[serde(rename = "hearingImpaired", default)]
    hearing_impaired: bool,
    #[serde(rename = "downloadUri")]
    download_uri: String,
    #[serde(rename = "downloadCount", default)]
    downloads: Option<u64>,
    #[serde(default)]
    qualities: Vec<String>,
    #[serde(default)]
    release: Option<String>,
}

fn candidates(base: &str, language: &str, episode: Episode) -> Vec<SubtitleCandidate> {
    episode
        .subtitles
        .into_iter()
        .filter(|item| item.completed)
        .map(|item| {
            let mut release = item.release.clone().unwrap_or_default();
            for part in item.version.iter().chain(item.qualities.iter()) {
                if !part.is_empty() {
                    if !release.is_empty() {
                        release.push(' ');
                    }
                    release.push_str(part);
                }
            }
            SubtitleCandidate {
                provider: ID.into(),
                id: item.id,
                language: language.into(),
                release,
                hearing_impaired: item.hearing_impaired,
                downloads: item.downloads,
                fps: None,
                hash_match: false,
                score: 0,
                url: format!("{base}{}", item.download_uri),
            }
        })
        .collect()
}

#[async_trait]
impl Provider for Gestdown {
    fn id(&self) -> &'static str {
        ID
    }

    async fn search(&self, query: &SubtitleQuery) -> Result<Vec<SubtitleCandidate>> {
        let (Some(season), Some(episode)) = (query.season, query.episode) else {
            return Ok(Vec::new());
        };
        if query.title.trim().is_empty() {
            return Ok(Vec::new());
        }
        let shows: Shows = http::get_json(
            &self.client,
            &self.guard,
            &self.url(&["shows", "search", query.title.trim()])?,
        )
        .await?;
        let Some(show) = pick_show(&shows.shows, query) else {
            return Ok(Vec::new());
        };

        let (season, episode) = (season.to_string(), episode.to_string());
        let mut found = Vec::new();
        let mut last_error = None;
        for code in query.languages.iter().take(MAX_LANGUAGES) {
            let Some(name) = lang::english_name(code) else {
                continue;
            };
            let url = self.url(&["subtitles", "get", &show.id, &season, &episode, name])?;
            match http::get_json::<Episode>(&self.client, &self.guard, &url).await {
                Ok(answer) => found.extend(candidates(
                    &self.base,
                    lang::normalize(code).unwrap_or(code),
                    answer,
                )),
                Err(PlayerError::NotFound(_)) => {}
                Err(error) => last_error = Some(error),
            }
        }
        match (found.is_empty(), last_error) {
            (true, Some(error)) => Err(error),
            _ => Ok(found),
        }
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

    /// Trimmed from real answers of the service.
    const SHOWS: &str = r#"{"shows":[
      {"id":"aaaa","name":"Severance Special","nbSeasons":1,"seasons":[1],"tvDbId":1,"tmdbId":1},
      {"id":"31437de3-eca9-4f14-a156-2308582f2424","name":"Severance","nbSeasons":2,"seasons":[1,2],"tvDbId":371980,"tmdbId":95396,"slug":"severance"}]}"#;
    const EPISODE: &str = r#"{"matchingSubtitles":[
      {"subtitleId":"57dd12a4","version":"TEPES","completed":true,"hearingImpaired":false,"corrected":true,"hd":true,"downloadUri":"/subtitles/download/57dd12a4","language":"English","downloadCount":507,"source":"Addic7ed","qualities":["720p","1080p"],"release":null},
      {"subtitleId":"b2","version":"WEB","completed":false,"hearingImpaired":false,"downloadUri":"/subtitles/download/b2","qualities":[]},
      {"subtitleId":"c3","version":"KILLERS","completed":true,"hearingImpaired":true,"downloadUri":"/subtitles/download/c3","downloadCount":3,"qualities":["HDTV"],"release":"Severance.S01E01"}]}"#;

    fn query() -> SubtitleQuery {
        SubtitleQuery {
            title: "Severance".into(),
            season: Some(1),
            episode: Some(1),
            tmdb_id: Some(95396),
            languages: vec!["en".into()],
            ..Default::default()
        }
    }

    #[test]
    fn the_show_is_chosen_by_tmdb_id_then_by_name() {
        let shows: Shows = serde_json::from_str(SHOWS).unwrap();
        assert_eq!(
            pick_show(&shows.shows, &query()).unwrap().id,
            "31437de3-eca9-4f14-a156-2308582f2424"
        );
        let mut by_name = query();
        by_name.tmdb_id = None;
        assert_eq!(pick_show(&shows.shows, &by_name).unwrap().name, "Severance");
        by_name.title = "Sever".into();
        assert!(
            pick_show(&shows.shows, &by_name).is_none(),
            "a partial name is not the show"
        );
    }

    #[test]
    fn unfinished_subtitles_are_dropped_and_release_names_are_assembled() {
        let episode: Episode = serde_json::from_str(EPISODE).unwrap();
        let found = candidates("https://api.gestdown.info", "en", episode);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].release, "TEPES 720p 1080p");
        assert_eq!(
            found[0].url,
            "https://api.gestdown.info/subtitles/download/57dd12a4"
        );
        assert_eq!(found[0].downloads, Some(507));
        assert_eq!(found[1].release, "Severance.S01E01 KILLERS HDTV");
        assert!(found[1].hearing_impaired);
    }

    #[tokio::test]
    async fn finds_the_show_then_the_episode_then_downloads() {
        let server = serve(vec![
            Canned::json("/shows/search/Severance", SHOWS),
            Canned::json(
                "/subtitles/get/31437de3-eca9-4f14-a156-2308582f2424/1/1/English",
                EPISODE,
            ),
            Canned::text(
                "/subtitles/download/57dd12a4",
                "1\r\n00:00:01,000 --> 00:00:02,000\r\nHi\r\n",
            ),
        ])
        .await;
        let provider = Gestdown::for_tests(&server.base, "127.0.0.1");
        let mut found = provider.search(&query()).await.unwrap();
        assert_eq!(found.len(), 2, "{found:#?}");
        // Candidates carry absolute addresses on the service's own host.
        found[0].url = found[0]
            .url
            .replace("https://api.gestdown.info", &server.base);
        let text = String::from_utf8(provider.download(&found[0]).await.unwrap()).unwrap();
        assert!(text.contains("Hi"));
    }

    #[tokio::test]
    async fn movies_and_unknown_shows_cost_nothing_or_nothing_found() {
        let server = serve(vec![Canned::json(
            "/shows/search/Severance",
            r#"{"shows":[]}"#,
        )])
        .await;
        let provider = Gestdown::for_tests(&server.base, "127.0.0.1");

        let mut movie = query();
        movie.season = None;
        movie.episode = None;
        assert!(provider.search(&movie).await.unwrap().is_empty());
        assert_eq!(server.hits(), 0, "a movie is not searched at all");

        assert!(provider.search(&query()).await.unwrap().is_empty());
        assert_eq!(server.hits(), 1);
    }

    #[tokio::test]
    async fn a_language_with_no_subtitles_is_skipped_and_a_broken_service_is_reported() {
        let server = serve(vec![
            Canned::json("/shows/search/Severance", SHOWS),
            Canned::status(
                "/subtitles/get/31437de3-eca9-4f14-a156-2308582f2424/1/1/English",
                500,
            ),
        ])
        .await;
        let provider = Gestdown::for_tests(&server.base, "127.0.0.1");
        let mut two = query();
        two.languages = vec!["fr".into(), "en".into()];
        // French answers 404 (skipped), English answers 500: nothing found plus an error is an error.
        let error = provider.search(&two).await.unwrap_err().to_string();
        assert!(error.contains("500"), "{error}");
    }
}
