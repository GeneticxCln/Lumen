//! Finding the IMDb id a subtitle service needs. Order: what is already known, the id cached from
//! an earlier lookup, TMDB (when the user has set a TMDB token), then IMDb's public suggestion
//! list. The suggestion list is an unofficial but long-standing endpoint, like the services that
//! use the id.

use serde::Deserialize;
use sqlx::{Pool, Row, Sqlite};
use url::Url;

use super::SubtitleQuery;
use super::http::{self, Guard};
use crate::error::{PlayerError, Result};
use crate::tmdb::{self, normalize_title};

const BASE: &str = "https://v3.sg.media-imdb.com";
const HOSTS: &[&str] = &["media-imdb.com"];

pub struct ImdbLookup {
    base: String,
    client: reqwest::Client,
    guard: Guard,
}

impl ImdbLookup {
    pub fn new(allow_lan: bool) -> Result<Self> {
        Ok(Self {
            base: BASE.into(),
            client: http::client(allow_lan)?,
            guard: Guard::new(HOSTS),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_tests(base: &str, host: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').into(),
            client: http::client(true).unwrap(),
            guard: Guard::insecure_for_tests(&[host]),
        }
    }

    /// The best title match on IMDb's suggestion list.
    pub async fn find(
        &self,
        title: &str,
        year: Option<i32>,
        series: bool,
    ) -> Result<Option<String>> {
        let wanted = title.trim().to_lowercase();
        let Some(first) = wanted.chars().find(|c| c.is_ascii_alphanumeric()) else {
            return Ok(None);
        };
        let mut url = Url::parse(&self.base)
            .map_err(|_| PlayerError::InvalidArg("bad IMDb address".into()))?;
        url.path_segments_mut()
            .map_err(|_| PlayerError::InvalidArg("bad IMDb address".into()))?
            .extend(["suggestion", &first.to_string(), &format!("{wanted}.json")]);
        let answer: Suggestions = http::get_json(&self.client, &self.guard, &url).await?;
        Ok(best_match(&answer.d, title, year, series))
    }
}

#[derive(Deserialize)]
struct Suggestions {
    #[serde(default)]
    d: Vec<Suggestion>,
}

#[derive(Deserialize)]
struct Suggestion {
    #[serde(default)]
    id: String,
    #[serde(default)]
    l: String,
    #[serde(default)]
    qid: Option<String>,
    #[serde(default)]
    y: Option<i32>,
}

fn kind_matches(qid: &str, series: bool) -> bool {
    if series {
        matches!(qid, "tvSeries" | "tvMiniSeries")
    } else {
        matches!(qid, "movie" | "tvMovie" | "video" | "short")
    }
}

/// The first suggestion (they come most popular first) that is the right kind of title with the
/// same normalised name, preferring one from the year asked for.
fn best_match(list: &[Suggestion], title: &str, year: Option<i32>, series: bool) -> Option<String> {
    let wanted = normalize_title(title);
    let fits: Vec<&Suggestion> = list
        .iter()
        .filter(|item| {
            item.id.starts_with("tt")
                && item
                    .qid
                    .as_deref()
                    .is_some_and(|qid| kind_matches(qid, series))
                && normalize_title(&item.l) == wanted
        })
        .collect();
    let by_year = year.and_then(|year| {
        fits.iter().find(|item| {
            item.y
                .is_some_and(|candidate| (candidate - year).abs() <= 1)
        })
    });
    by_year.or(fits.first()).map(|item| item.id.clone())
}

/// The key a found id is cached under: the TMDB identity when there is one, else name and year.
pub fn lookup_key(
    tmdb: Option<(i64, &str)>,
    title: &str,
    year: Option<i32>,
    series: bool,
) -> String {
    match tmdb {
        Some((id, kind)) => format!("tmdb:{kind}:{id}"),
        None => format!(
            "name:{}:{}:{}",
            if series { "tv" } else { "movie" },
            normalize_title(title),
            year.unwrap_or(0)
        ),
    }
}

pub async fn cached(pool: &Pool<Sqlite>, key: &str) -> Result<Option<String>> {
    Ok(
        sqlx::query("SELECT imdb_id FROM imdb_ids WHERE lookup_key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?
            .map(|row| row.get("imdb_id")),
    )
}

pub async fn remember(pool: &Pool<Sqlite>, key: &str, imdb_id: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO imdb_ids (lookup_key, imdb_id, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(lookup_key) DO UPDATE SET imdb_id = excluded.imdb_id, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(imdb_id)
    .bind(chrono::Utc::now().timestamp())
    .execute(pool)
    .await?;
    Ok(())
}

/// Fills in `query.imdb_id` when it can be found. A lookup that fails or finds nothing leaves it
/// empty: the providers that need an id then return nothing, and the ones that do not still run.
pub async fn resolve(
    pool: &Pool<Sqlite>,
    lookup: &ImdbLookup,
    query: &mut SubtitleQuery,
    tmdb_type: Option<&str>,
) -> Result<()> {
    if query.imdb_id.is_some() || query.title.trim().is_empty() {
        return Ok(());
    }
    let series = query.is_episode();
    let tmdb = query.tmdb_id.zip(tmdb_type);
    let key = lookup_key(tmdb, &query.title, query.year, series);
    if let Some(found) = cached(pool, &key).await? {
        query.imdb_id = Some(found);
        return Ok(());
    }

    let mut found = None;
    if let Some((id, kind)) = tmdb
        && let Ok(Some((token, _))) = tmdb::api_key(pool).await
    {
        found = tmdb::imdb_id_for(&token, kind, id).await.ok().flatten();
    }
    if found.is_none() {
        found = lookup
            .find(&query.title, query.year, series)
            .await
            .ok()
            .flatten();
    }
    if let Some(id) = &found {
        remember(pool, &key, id).await?;
    }
    query.imdb_id = found;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitles::testing::{Canned, serve};
    use sqlx::sqlite::SqlitePoolOptions;

    /// Trimmed from IMDb's answer for "severance".
    const SEVERANCE: &str = r#"{"d":[
      {"id":"tt11280740","l":"Severance","qid":"tvSeries","q":"TV series","y":2022},
      {"id":"tt0464196","l":"Severance","qid":"movie","q":"feature","y":2006},
      {"id":"nm0000220","l":"Joan Severance"},
      {"id":"tt10963038","l":"Severance","qid":"tvSeries","q":"TV series","y":2019}]}"#;

    async fn pool() -> Pool<Sqlite> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        pool
    }

    #[test]
    fn the_right_kind_and_year_are_chosen() {
        let list: Suggestions = serde_json::from_str(SEVERANCE).unwrap();
        assert_eq!(
            best_match(&list.d, "Severance", None, true).as_deref(),
            Some("tt11280740")
        );
        assert_eq!(
            best_match(&list.d, "Severance", Some(2019), true).as_deref(),
            Some("tt10963038")
        );
        assert_eq!(
            best_match(&list.d, "Severance", None, false).as_deref(),
            Some("tt0464196")
        );
        assert_eq!(
            best_match(&list.d, "Joan Severance", None, false),
            None,
            "people are not titles"
        );
        assert_eq!(
            best_match(&list.d, "Sever", None, true),
            None,
            "a partial name is not the title"
        );
    }

    #[test]
    fn cache_keys_prefer_the_tmdb_identity() {
        assert_eq!(
            lookup_key(Some((603, "movie")), "The Matrix", Some(1999), false),
            "tmdb:movie:603"
        );
        assert_eq!(
            lookup_key(None, "The Matrix", Some(1999), false),
            "name:movie:matrix:1999"
        );
        assert_eq!(
            lookup_key(None, "Severance", None, true),
            "name:tv:severance:0"
        );
    }

    #[tokio::test]
    async fn a_found_id_is_cached_so_the_next_lookup_asks_nobody() {
        let pool = pool().await;
        let server = serve(vec![Canned::json(
            "/suggestion/s/severance.json",
            SEVERANCE,
        )])
        .await;
        let lookup = ImdbLookup::for_tests(&server.base, "127.0.0.1");
        let mut query = SubtitleQuery {
            title: "Severance".into(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        };
        resolve(&pool, &lookup, &mut query, None).await.unwrap();
        assert_eq!(query.imdb_id.as_deref(), Some("tt11280740"));
        assert_eq!(server.hits(), 1);

        let mut again = SubtitleQuery {
            title: "Severance".into(),
            season: Some(2),
            episode: Some(3),
            ..Default::default()
        };
        resolve(&pool, &lookup, &mut again, None).await.unwrap();
        assert_eq!(again.imdb_id.as_deref(), Some("tt11280740"));
        assert_eq!(server.hits(), 1, "answered from the cache");
    }

    #[tokio::test]
    async fn a_known_id_a_dead_service_and_an_empty_title_are_all_harmless() {
        let pool = pool().await;
        let server = serve(vec![Canned::status("/suggestion/h/heat.json", 500)]).await;
        let lookup = ImdbLookup::for_tests(&server.base, "127.0.0.1");

        let mut known = SubtitleQuery {
            title: "Heat".into(),
            imdb_id: Some("tt0113277".into()),
            ..Default::default()
        };
        resolve(&pool, &lookup, &mut known, None).await.unwrap();
        assert_eq!(known.imdb_id.as_deref(), Some("tt0113277"));
        assert_eq!(server.hits(), 0);

        let mut failing = SubtitleQuery {
            title: "Heat".into(),
            ..Default::default()
        };
        resolve(&pool, &lookup, &mut failing, None).await.unwrap();
        assert_eq!(failing.imdb_id, None);

        let mut empty = SubtitleQuery::default();
        resolve(&pool, &lookup, &mut empty, None).await.unwrap();
        assert_eq!(empty.imdb_id, None);
    }
}
