//! The subtitle feature as the app uses it: work out what is playing, search the enabled
//! providers, download and store the one the user picks, and remember the pick so replaying the
//! title loads it with no network.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqlx::{Pool, Row, Sqlite};

use super::ids::{self, ImdbLookup};
use super::lang;
use super::release::parse_release;
use super::{
    Provider, SearchOutcome, Searched, SubtitleCandidate, SubtitleQuery, gestdown, hash, mirror,
    search_all, store,
};
use crate::content_key::ContentKey;
use crate::db::{get_setting, set_setting};
use crate::error::{PlayerError, Result};

/// The master switch. Off until the user has read what a search sends and agreed.
pub const ONLINE_SETTING: &str = "subtitle_online";
const MAX_LANGUAGES: usize = 3;

/// The setting that turns one provider off (`false`); absent or anything else means on.
pub fn provider_setting(provider_id: &str) -> String {
    format!("subtitle_provider_{}", provider_id.replace('-', "_"))
}

/// What is playing, as the UI knows it.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayingItem {
    pub channel_id: Option<i64>,
    /// The address or file path being played.
    pub source: String,
    /// The title shown to the user, used when there is no library entry.
    pub name: Option<String>,
}

pub async fn online_enabled(pool: &Pool<Sqlite>) -> Result<bool> {
    Ok(get_setting(pool, ONLINE_SETTING).await?.as_deref() == Some("true"))
}

pub async fn set_online(pool: &Pool<Sqlite>, on: bool) -> Result<()> {
    set_setting(pool, ONLINE_SETTING, if on { "true" } else { "false" }).await
}

/// The providers the user has not switched off.
pub async fn enabled_providers(
    pool: &Pool<Sqlite>,
    allow_lan: bool,
) -> Result<Vec<Arc<dyn Provider>>> {
    let mut providers: Vec<Arc<dyn Provider>> = Vec::new();
    if get_setting(pool, &provider_setting(mirror::ID))
        .await?
        .as_deref()
        != Some("false")
    {
        providers.push(Arc::new(mirror::StremioMirror::new(allow_lan)?));
    }
    if get_setting(pool, &provider_setting(gestdown::ID))
        .await?
        .as_deref()
        != Some("false")
    {
        providers.push(Arc::new(gestdown::Gestdown::new(allow_lan)?));
    }
    Ok(providers)
}

/// Files worth fingerprinting: video containers. The hash of anything else is never computed or
/// sent, so a lookup cannot be used to fingerprint an arbitrary file.
fn is_video_file(path: &Path) -> bool {
    const VIDEO: &[&str] = &[
        "mkv", "mp4", "avi", "mov", "m4v", "webm", "ts", "m2ts", "wmv", "flv", "mpg", "mpeg",
    ];
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| VIDEO.contains(&extension.to_ascii_lowercase().as_str()))
}

fn is_remote(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

fn clean_languages(languages: &[String]) -> Vec<String> {
    let mut codes: Vec<String> = Vec::new();
    for code in languages {
        if let Some(code) = lang::normalize(code)
            && !codes.iter().any(|known| known == code)
        {
            codes.push(code.to_string());
        }
    }
    codes.truncate(MAX_LANGUAGES);
    if codes.is_empty() {
        codes.push("en".into());
    }
    codes
}

struct Resolved {
    query: SubtitleQuery,
    tmdb_type: Option<String>,
}

/// The library row for a channel, or `None` when the item is not in the library.
struct ChannelRow {
    name: String,
    url: String,
    tvg_id: Option<String>,
    is_live: bool,
    tmdb_id: Option<i64>,
    tmdb_type: Option<String>,
    release_year: Option<i64>,
}

async fn channel_row(pool: &Pool<Sqlite>, channel_id: i64) -> Result<Option<ChannelRow>> {
    let row = sqlx::query(
        "SELECT name, url, tvg_id, is_live, tmdb_id, tmdb_type, release_year FROM channels WHERE id = ?",
    )
    .bind(channel_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| ChannelRow {
        name: row.get("name"),
        url: row.get("url"),
        tvg_id: row.get("tvg_id"),
        is_live: row.get::<Option<i64>, _>("is_live").unwrap_or(0) != 0,
        tmdb_id: row.get("tmdb_id"),
        tmdb_type: row.get("tmdb_type"),
        release_year: row.get("release_year"),
    }))
}

/// The key the remembered subtitle of this item is stored under; the same key the library uses
/// for the title, so every source of a title shares it.
pub async fn content_key_for(pool: &Pool<Sqlite>, item: &PlayingItem) -> Result<String> {
    if let Some(id) = item.channel_id
        && let Some(row) = channel_row(pool, id).await?
    {
        return Ok(ContentKey::derive(
            row.tmdb_id,
            row.tmdb_type.as_deref(),
            &row.name,
            row.tvg_id.as_deref(),
            &row.url,
        )
        .into_string());
    }
    Ok(if is_remote(&item.source) {
        ContentKey::for_stream_url(&item.source)
    } else {
        ContentKey::for_file(&item.source)
    }
    .into_string())
}

async fn build_query(
    pool: &Pool<Sqlite>,
    item: &PlayingItem,
    languages: &[String],
) -> Result<Resolved> {
    let languages = clean_languages(languages);
    let mut query = SubtitleQuery {
        languages,
        ..Default::default()
    };
    let mut tmdb_type = None;
    let name;

    let row = match item.channel_id {
        Some(id) => channel_row(pool, id).await?,
        None => None,
    };
    if let Some(row) = row {
        if row.is_live {
            return Err(PlayerError::InvalidArg(
                "Live TV has no subtitles to search for".into(),
            ));
        }
        query.tmdb_id = row.tmdb_id;
        tmdb_type = row.tmdb_type.clone();
        query.year = row.release_year.and_then(|year| i32::try_from(year).ok());
        name = row.name;
    } else {
        name = item
            .name
            .clone()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| item.source.clone());
    }

    // The file name tells more than a library title when it has the release in it.
    let file_name = if is_remote(&item.source) {
        name.clone()
    } else {
        item.source.clone()
    };
    let parsed = parse_release(if is_remote(&item.source) {
        &name
    } else {
        &file_name
    });
    query.title = parsed.title;
    query.year = query.year.or(parsed.year);
    query.season = parsed.season;
    query.episode = parsed.episode;
    query.release_name = Some(
        Path::new(&file_name)
            .file_name()
            .and_then(|part| part.to_str())
            .unwrap_or(&file_name)
            .to_string(),
    );

    if !is_remote(&item.source) && is_video_file(Path::new(&item.source)) {
        let path = PathBuf::from(&item.source);
        if let Ok(Ok(Some((hash, size)))) =
            tokio::task::spawn_blocking(move || hash::opensubtitles_hash(&path)).await
        {
            query.file_hash = Some(hash);
            query.file_size = Some(size);
        }
    }
    Ok(Resolved { query, tmdb_type })
}

/// Searches with the given providers. Split from [`search`] so tests can supply their own.
pub async fn search_with(
    pool: &Pool<Sqlite>,
    providers: &[Arc<dyn Provider>],
    lookup: &ImdbLookup,
    item: &PlayingItem,
    languages: &[String],
) -> Result<SearchOutcome> {
    let Resolved {
        mut query,
        tmdb_type,
        ..
    } = build_query(pool, item, languages).await?;
    let mut hints = Vec::new();
    if query.title.trim().is_empty() {
        return Ok(SearchOutcome {
            hints: vec!["no_title".into()],
            searched: Searched {
                languages: query.languages,
                ..Default::default()
            },
            ..Default::default()
        });
    }
    if tmdb_type
        .as_deref()
        .is_some_and(|kind| kind == "tv" || kind == "series")
        && !query.is_episode()
    {
        hints.push("no_episode".to_string());
    }
    ids::resolve(pool, lookup, &mut query, tmdb_type.as_deref()).await?;
    if query.imdb_id.is_none() {
        hints.push("no_id".to_string());
    }
    let mut outcome = search_all(providers, &query).await;
    outcome.hints = hints;
    outcome.searched = Searched {
        title: query.title,
        year: query.year,
        season: query.season,
        episode: query.episode,
        languages: query.languages,
    };
    Ok(outcome)
}

/// The search the app runs: refuses until the user has turned online search on.
pub async fn search(
    pool: &Pool<Sqlite>,
    item: &PlayingItem,
    languages: &[String],
    allow_lan: bool,
) -> Result<SearchOutcome> {
    if !online_enabled(pool).await? {
        return Err(PlayerError::InvalidArg(
            "Online subtitle search is turned off".into(),
        ));
    }
    let providers = enabled_providers(pool, allow_lan).await?;
    if providers.is_empty() {
        return Err(PlayerError::InvalidArg(
            "Every subtitle service is switched off in Settings".into(),
        ));
    }
    let lookup = ImdbLookup::new(allow_lan)?;
    search_with(pool, &providers, &lookup, item, languages).await
}

/// Downloads a candidate, checks and converts it, stores it and remembers the choice. Returns the
/// path to give the player.
pub async fn fetch_and_store(
    pool: &Pool<Sqlite>,
    providers: &[Arc<dyn Provider>],
    app_dir: &Path,
    item: &PlayingItem,
    candidate: &SubtitleCandidate,
) -> Result<PathBuf> {
    let provider = providers
        .iter()
        .find(|provider| provider.id() == candidate.provider)
        .ok_or_else(|| PlayerError::InvalidArg("That subtitle service is not available".into()))?;
    let bytes = provider.download(candidate).await?;
    let text = store::normalise(&bytes)?;
    let key = content_key_for(pool, item).await?;
    let path = store::save(app_dir, &key, candidate, &text)?;
    remember_choice(pool, &key, candidate, &path).await?;
    Ok(path)
}

/// The app's entry point for applying a candidate.
pub async fn apply(
    pool: &Pool<Sqlite>,
    app_dir: &Path,
    item: &PlayingItem,
    candidate: &SubtitleCandidate,
    allow_lan: bool,
) -> Result<PathBuf> {
    if !online_enabled(pool).await? {
        return Err(PlayerError::InvalidArg(
            "Online subtitle search is turned off".into(),
        ));
    }
    let providers = enabled_providers(pool, allow_lan).await?;
    fetch_and_store(pool, &providers, app_dir, item, candidate).await
}

pub(super) async fn remember_choice(
    pool: &Pool<Sqlite>,
    key: &str,
    candidate: &SubtitleCandidate,
    path: &Path,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO subtitle_choices (content_key, language, path, provider, release, chosen_at) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(content_key, language) DO UPDATE SET path = excluded.path, provider = excluded.provider, \
         release = excluded.release, chosen_at = excluded.chosen_at",
    )
    .bind(key)
    .bind(&candidate.language)
    .bind(path.to_string_lossy().as_ref())
    .bind(&candidate.provider)
    .bind(&candidate.release)
    .bind(chrono::Utc::now().timestamp())
    .execute(pool)
    .await?;
    Ok(())
}

/// The subtitle remembered for a title: the first of `languages` that has a file still on disk,
/// or with none given the most recent choice. A choice whose file is gone is forgotten.
pub async fn chosen(
    pool: &Pool<Sqlite>,
    content_key: &str,
    languages: &[String],
) -> Result<Option<PathBuf>> {
    let rows = sqlx::query(
        "SELECT language, path FROM subtitle_choices WHERE content_key = ? ORDER BY chosen_at DESC",
    )
    .bind(content_key)
    .fetch_all(pool)
    .await?;
    let mut ordered: Vec<(String, String)> = rows
        .iter()
        .map(|row| (row.get("language"), row.get("path")))
        .collect();
    if !languages.is_empty() {
        let rank = |language: &str| languages.iter().position(|code| code == language);
        ordered.retain(|(language, _)| rank(language).is_some());
        ordered.sort_by_key(|(language, _)| rank(language));
    }
    for (language, path) in ordered {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(Some(path));
        }
        sqlx::query("DELETE FROM subtitle_choices WHERE content_key = ? AND language = ?")
            .bind(content_key)
            .bind(&language)
            .execute(pool)
            .await?;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitles::testing::{Canned, serve};
    use async_trait::async_trait;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Mutex;

    async fn pool() -> Pool<Sqlite> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        crate::db::migrate_tmdb_columns(&pool).await.unwrap();
        pool
    }

    /// Answers with a fixed list and remembers the query it was given.
    struct Fake {
        name: &'static str,
        answer: Result<Vec<SubtitleCandidate>>,
        body: Vec<u8>,
        seen: Mutex<Vec<SubtitleQuery>>,
    }

    impl Fake {
        fn new(name: &'static str, found: Vec<SubtitleCandidate>) -> Arc<Self> {
            Arc::new(Self {
                name,
                answer: Ok(found),
                body: SRT.as_bytes().to_vec(),
                seen: Mutex::new(Vec::new()),
            })
        }

        fn failing(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                answer: Err(PlayerError::Http("the service answered 503".into())),
                body: Vec::new(),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    const SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\nHello\n";

    #[async_trait]
    impl Provider for Fake {
        fn id(&self) -> &'static str {
            self.name
        }

        async fn search(&self, query: &SubtitleQuery) -> Result<Vec<SubtitleCandidate>> {
            self.seen.lock().unwrap().push(query.clone());
            match &self.answer {
                Ok(found) => Ok(found.clone()),
                Err(error) => Err(PlayerError::Http(error.to_string())),
            }
        }

        async fn download(&self, _: &SubtitleCandidate) -> Result<Vec<u8>> {
            Ok(self.body.clone())
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
            url: format!("https://example.test/{id}"),
        }
    }

    async fn lookup() -> (crate::subtitles::testing::Server, ImdbLookup) {
        let server = serve(vec![Canned::json(
            "/suggestion/s/severance.json",
            r#"{"d":[{"id":"tt11280740","l":"Severance","qid":"tvSeries","y":2022}]}"#,
        )])
        .await;
        let lookup = ImdbLookup::for_tests(&server.base, "127.0.0.1");
        (server, lookup)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-service-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_video_files_are_fingerprinted() {
        assert!(is_video_file(Path::new("/media/Heat.1995.MKV")));
        assert!(is_video_file(Path::new("movie.mp4")));
        for other in [
            "/home/me/.ssh/id_rsa",
            "/etc/passwd",
            "notes.txt",
            "archive.zip",
            "Heat",
        ] {
            assert!(!is_video_file(Path::new(other)), "{other}");
        }
    }

    #[test]
    fn languages_are_cleaned_deduplicated_and_defaulted() {
        let said = |codes: &[&str]| {
            clean_languages(&codes.iter().map(|c| c.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(
            said(&["fre", "FR", "en", "xx", "de", "es"]),
            ["fr", "en", "de"]
        );
        assert_eq!(said(&[]), ["en"]);
        assert_eq!(said(&["klingon"]), ["en"]);
    }

    #[tokio::test]
    async fn a_library_episode_is_searched_by_name_season_and_episode() {
        let pool = pool().await;
        sqlx::query("INSERT INTO playlists (id, name, source, created_at, updated_at) VALUES (1, 'p', 'url', 0, 0)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO channels (id, playlist_id, name, url, group_title, is_live) VALUES (7, 1, 'Severance S01E02 Half Loop', 'https://e.example/7', 'Series', 0)")
            .execute(&pool).await.unwrap();
        let (_server, lookup) = lookup().await;
        let fake = Fake::new(
            "fake",
            vec![candidate("fake", "a", "en", "Severance.S01E02")],
        );
        let providers: Vec<Arc<dyn Provider>> = vec![fake.clone()];

        let item = PlayingItem {
            channel_id: Some(7),
            source: "https://e.example/7".into(),
            name: None,
        };
        let outcome = search_with(&pool, &providers, &lookup, &item, &["en".to_string()])
            .await
            .unwrap();
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].title, "Severance");
        assert_eq!((seen[0].season, seen[0].episode), (Some(1), Some(2)));
        assert_eq!(seen[0].imdb_id.as_deref(), Some("tt11280740"));
        assert_eq!(outcome.candidates.len(), 1);
        assert!(outcome.hints.is_empty());
        assert_eq!(outcome.searched.season, Some(1));
    }

    #[tokio::test]
    async fn live_tv_has_nothing_to_search() {
        let pool = pool().await;
        sqlx::query("INSERT INTO playlists (id, name, source, created_at, updated_at) VALUES (1, 'p', 'url', 0, 0)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO channels (id, playlist_id, name, url, group_title, is_live) VALUES (9, 1, 'World News', 'https://e.example/9', 'News', 1)")
            .execute(&pool).await.unwrap();
        let (_server, lookup) = lookup().await;
        let item = PlayingItem {
            channel_id: Some(9),
            source: "https://e.example/9".into(),
            name: None,
        };
        assert!(search_with(&pool, &[], &lookup, &item, &[]).await.is_err());
    }

    #[tokio::test]
    async fn one_failing_service_does_not_hide_the_other() {
        let pool = pool().await;
        let (_server, lookup) = lookup().await;
        let good = Fake::new(
            "good",
            vec![candidate("good", "g1", "en", "Severance.S01E01")],
        );
        let bad = Fake::failing("bad");
        let providers: Vec<Arc<dyn Provider>> = vec![bad, good];
        let item = PlayingItem {
            channel_id: None,
            source: "/videos/Severance.S01E01.1080p.mkv".into(),
            name: None,
        };
        let outcome = search_with(&pool, &providers, &lookup, &item, &["en".to_string()])
            .await
            .unwrap();
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.problems.len(), 1);
        assert_eq!(outcome.problems[0].provider, "bad");
        assert!(outcome.problems[0].message.contains("503"));
    }

    #[tokio::test]
    async fn a_name_with_no_title_or_no_episode_says_so() {
        let pool = pool().await;
        let (_server, lookup) = lookup().await;
        let fake = Fake::new("fake", vec![]);
        let providers: Vec<Arc<dyn Provider>> = vec![fake];
        let nameless = PlayingItem {
            channel_id: None,
            source: "/videos/2019-1080p.mkv".into(),
            name: Some("".into()),
        };
        let outcome = search_with(&pool, &providers, &lookup, &nameless, &[])
            .await
            .unwrap();
        assert!(outcome.candidates.is_empty());

        let untitled = PlayingItem {
            channel_id: None,
            source: "https://e.example/x".into(),
            name: Some("1080p".into()),
        };
        let outcome = search_with(&pool, &providers, &lookup, &untitled, &[])
            .await
            .unwrap();
        assert_eq!(outcome.hints, ["no_title"]);
    }

    #[tokio::test]
    async fn applying_stores_the_file_and_the_choice_is_remembered_until_the_file_goes() {
        let pool = pool().await;
        let dir = scratch("apply");
        let fake = Fake::new("fake", vec![]);
        let providers: Vec<Arc<dyn Provider>> = vec![fake];
        let item = PlayingItem {
            channel_id: None,
            source: "/videos/Heat.1995.mkv".into(),
            name: None,
        };
        let pick = candidate("fake", "abc123", "en", "Heat.1995");

        let path = fetch_and_store(&pool, &providers, &dir, &item, &pick)
            .await
            .unwrap();
        assert!(path.is_file());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SRT);

        let key = content_key_for(&pool, &item).await.unwrap();
        assert_eq!(
            chosen(&pool, &key, &["en".to_string()]).await.unwrap(),
            Some(path.clone())
        );
        assert_eq!(
            chosen(&pool, &key, &[]).await.unwrap(),
            Some(path.clone()),
            "any language when none is asked for"
        );
        assert_eq!(
            chosen(&pool, &key, &["fr".to_string()]).await.unwrap(),
            None,
            "only the languages asked for"
        );

        std::fs::remove_file(&path).unwrap();
        assert_eq!(chosen(&pool, &key, &[]).await.unwrap(), None);
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subtitle_choices")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "a choice whose file is gone is forgotten");

        let stranger = candidate("nobody", "x", "en", "");
        assert!(
            fetch_and_store(&pool, &providers, &dir, &item, &stranger)
                .await
                .is_err()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn nothing_is_searched_until_online_search_is_turned_on() {
        let pool = pool().await;
        let item = PlayingItem {
            channel_id: None,
            source: "/videos/Heat.1995.mkv".into(),
            name: None,
        };
        let error = search(&pool, &item, &["en".to_string()], false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("turned off"), "{error}");
        assert!(!online_enabled(&pool).await.unwrap());
        set_online(&pool, true).await.unwrap();
        assert!(online_enabled(&pool).await.unwrap());

        // Switching a provider off removes it from the list.
        assert_eq!(enabled_providers(&pool, false).await.unwrap().len(), 2);
        set_setting(&pool, &provider_setting(gestdown::ID), "false")
            .await
            .unwrap();
        let left = enabled_providers(&pool, false).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id(), mirror::ID);
    }
}
