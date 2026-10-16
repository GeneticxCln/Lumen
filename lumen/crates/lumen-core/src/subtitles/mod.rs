//! Finding subtitles for what is playing, without anyone's API key.
//!
//! Two public services are used (both answer plain requests): Stremio's OpenSubtitles mirror,
//! which knows movies and series by IMDb id, and Gestdown, which mirrors Addic7ed for TV. Neither
//! is an official interface for other apps, so each sits behind the [`Provider`] trait, can be
//! switched off, and failing never stops the other from answering.
//!
//! Nothing here runs by itself: a search happens when the user asks for one, and a downloaded file
//! is checked, converted to UTF-8 and stored before the player is given its path.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

pub mod extractor;
pub mod gestdown;
pub mod hash;
pub mod http;
pub mod ids;
pub mod lang;
pub mod mirror;
pub mod ranking;
pub mod release;
pub mod service;
pub mod store;

#[cfg(test)]
mod testing;

/// How long one provider gets to answer a search.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);

/// What to look for. Built from the playing item by [`service::build_query`].
#[derive(Debug, Clone, Default)]
pub struct SubtitleQuery {
    pub title: String,
    pub year: Option<i32>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    /// IMDb id of the movie or, for an episode, of the series (`tt0111161`).
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<i64>,
    /// ISO 639-1 codes, best first.
    pub languages: Vec<String>,
    /// OpenSubtitles hash and size of a local file.
    pub file_hash: Option<String>,
    pub file_size: Option<u64>,
    /// File or release name of what is playing, used to rank by how close a release is.
    pub release_name: Option<String>,
}

impl SubtitleQuery {
    pub fn is_episode(&self) -> bool {
        self.season.is_some() && self.episode.is_some()
    }
}

/// One subtitle a provider offers. Sent to the UI and sent back unchanged to download, so the
/// download address is checked again against the provider's own hosts before it is used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleCandidate {
    pub provider: String,
    pub id: String,
    /// ISO 639-1.
    pub language: String,
    /// Release or file name the subtitle was made for.
    pub release: String,
    pub hearing_impaired: bool,
    pub downloads: Option<u64>,
    pub fps: Option<f64>,
    /// The service says it matches this exact file, not just this title.
    pub hash_match: bool,
    pub score: i32,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProblem {
    pub provider: String,
    pub message: String,
}

/// What the search was for, so the UI can say it ("Severance, season 1 episode 2").
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Searched {
    pub title: String,
    pub year: Option<i32>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchOutcome {
    pub searched: Searched,
    pub candidates: Vec<SubtitleCandidate>,
    pub problems: Vec<ProviderProblem>,
    /// Why a search could not be more precise, as codes the UI turns into text:
    /// `no_title`, `no_episode`, `no_id`.
    pub hints: Vec<String>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable name used in settings, candidates and the stored file name.
    fn id(&self) -> &'static str;
    async fn search(&self, query: &SubtitleQuery) -> Result<Vec<SubtitleCandidate>>;
    /// The raw bytes of the subtitle. Refuses an address outside the provider's own hosts.
    async fn download(&self, candidate: &SubtitleCandidate) -> Result<Vec<u8>>;
}

/// Asks every provider at once. A slow or failing provider is reported in `problems` and the
/// others' results still come back, ranked.
pub async fn search_all(providers: &[Arc<dyn Provider>], query: &SubtitleQuery) -> SearchOutcome {
    let query = Arc::new(query.clone());
    let mut tasks = tokio::task::JoinSet::new();
    for provider in providers {
        let provider = Arc::clone(provider);
        let query = Arc::clone(&query);
        tasks.spawn(async move {
            let name = provider.id();
            let result = match tokio::time::timeout(SEARCH_TIMEOUT, provider.search(&query)).await {
                Ok(result) => result,
                Err(_) => Err(crate::error::PlayerError::Http(
                    "it did not answer in time".into(),
                )),
            };
            (name, result)
        });
    }

    let mut outcome = SearchOutcome::default();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((_, Ok(found))) => outcome.candidates.extend(found),
            Ok((name, Err(error))) => outcome.problems.push(ProviderProblem {
                provider: name.to_string(),
                message: error.to_string(),
            }),
            Err(_) => {}
        }
    }
    for candidate in &mut outcome.candidates {
        candidate.score = ranking::score(&query, candidate);
    }
    outcome
        .candidates
        .sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    outcome.problems.sort_by(|a, b| a.provider.cmp(&b.provider));
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static str, Result<Vec<SubtitleCandidate>>);

    #[async_trait]
    impl Provider for Fixed {
        fn id(&self) -> &'static str {
            self.0
        }

        async fn search(&self, _: &SubtitleQuery) -> Result<Vec<SubtitleCandidate>> {
            match &self.1 {
                Ok(list) => Ok(list.clone()),
                Err(error) => Err(crate::error::PlayerError::Http(error.to_string())),
            }
        }

        async fn download(&self, _: &SubtitleCandidate) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    fn candidate(provider: &str, id: &str, language: &str, release: &str) -> SubtitleCandidate {
        SubtitleCandidate {
            provider: provider.into(),
            id: id.into(),
            language: language.into(),
            release: release.into(),
            hearing_impaired: false,
            downloads: None,
            fps: None,
            hash_match: false,
            score: 0,
            url: String::new(),
        }
    }

    #[tokio::test]
    async fn results_from_every_provider_are_ranked_together() {
        let query = SubtitleQuery {
            languages: vec!["en".into(), "fr".into()],
            release_name: Some("Heat.1995.1080p.BluRay.x264-GRP.mkv".into()),
            ..Default::default()
        };
        let providers: Vec<Arc<dyn Provider>> = vec![
            Arc::new(Fixed(
                "a",
                Ok(vec![
                    candidate("a", "french", "fr", "Heat.1995.1080p.BluRay.x264-GRP"),
                    candidate("a", "english-loose", "en", "Heat"),
                ]),
            )),
            Arc::new(Fixed(
                "b",
                Ok(vec![candidate(
                    "b",
                    "english-exact",
                    "en",
                    "Heat.1995.1080p.BluRay.x264-GRP",
                )]),
            )),
            Arc::new(Fixed(
                "c",
                Err(crate::error::PlayerError::Http("down".into())),
            )),
        ];
        let outcome = search_all(&providers, &query).await;
        let order: Vec<&str> = outcome.candidates.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(order, ["english-exact", "english-loose", "french"]);
        assert!(
            outcome
                .candidates
                .windows(2)
                .all(|pair| pair[0].score >= pair[1].score)
        );
        assert_eq!(outcome.problems.len(), 1);
        assert_eq!(outcome.problems[0].provider, "c");
    }

    /// Talks to the real services. Run on purpose: `cargo test -p lumen-core live_ -- --ignored`.
    #[tokio::test]
    #[ignore = "talks to the real subtitle services"]
    async fn live_the_services_find_and_serve_a_subtitle() {
        // IMDb's suggestion list resolves a name to the id.
        let lookup = ids::ImdbLookup::new(false).unwrap();
        assert_eq!(
            lookup
                .find("Heat", Some(1995), false)
                .await
                .unwrap()
                .as_deref(),
            Some("tt0113277")
        );

        // The mirror knows a movie by its id and serves real, convertible text.
        let mirror = mirror::StremioMirror::new(false).unwrap();
        let movie = SubtitleQuery {
            title: "The Shawshank Redemption".into(),
            imdb_id: Some("tt0111161".into()),
            languages: vec!["en".into()],
            ..Default::default()
        };
        let found = mirror.search(&movie).await.unwrap();
        assert!(
            !found.is_empty(),
            "the mirror returned nothing for a famous film"
        );
        let text = store::normalise(&mirror.download(&found[0]).await.unwrap()).unwrap();
        assert!(text.contains("-->"));

        // Gestdown finds a TV episode by show name and serves it.
        let gestdown = gestdown::Gestdown::new(false).unwrap();
        let episode = SubtitleQuery {
            title: "Severance".into(),
            season: Some(1),
            episode: Some(1),
            tmdb_id: Some(95396),
            languages: vec!["en".into()],
            ..Default::default()
        };
        let found = gestdown.search(&episode).await.unwrap();
        assert!(
            !found.is_empty(),
            "Gestdown returned nothing for a popular episode"
        );
        let text = store::normalise(&gestdown.download(&found[0]).await.unwrap()).unwrap();
        assert!(text.contains("-->"));
    }
}
