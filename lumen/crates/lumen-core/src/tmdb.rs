//! TMDB (The Movie Database) metadata enrichment.
//!
//! Lumen's library is mostly live IPTV channels, which TMDB cannot describe. So
//! enrichment is best-effort by design: a channel is only matched when its cleaned
//! title really corresponds to a TMDB movie or TV show. Every candidate is stamped
//! with `enriched_at` whether or not it matched, so a re-run never re-queries the
//! API for rows we already tried.
//!
//! TMDB requires attribution: <https://www.themoviedb.org/>

use crate::db::{delete_setting, get_setting};
use crate::error::{PlayerError, Result};
use crate::secrets;
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use sqlx::{Pool, Sqlite};
use std::sync::OnceLock;
use std::time::Duration;

pub const TMDB_BASE: &str = "https://api.themoviedb.org/3";
pub const IMAGE_BASE: &str = "https://image.tmdb.org/t/p/";
pub const POSTER_SIZE: &str = "w342";
pub const BACKDROP_SIZE: &str = "w780";
pub const API_KEY_SETTING: &str = "tmdb_api_key";

/// TMDB tolerates bursts far above this; we keep one request in flight and a small
/// gap between them so a 1000-row library cannot trip the abuse limiter.
const REQUEST_GAP_MS: u64 = 120;
const HTTP_TIMEOUT_SECS: u64 = 20;
/// Titles returned by one browse call. Bounded so the Discovery shelves stay light.
pub const BROWSE_LIMIT: usize = 40;

#[derive(Debug)]
pub struct EnrichOutcome {
    pub enriched: i64,
    pub matched: i64,
    pub skipped: i64,
    pub remaining: i64,
    pub requests: i64,
}

#[derive(Debug)]
struct Found {
    tmdb_id: i64,
    tmdb_type: &'static str,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    overview: Option<String>,
    release_year: Option<i64>,
    rating: Option<f64>,
    genre_names: Option<String>,
}

/// Distinguishes a rejected credential (fatal, abort the whole run) from a
/// transient failure on one candidate (skip it and keep going).
enum TmdbFailure {
    Unauthorized,
    Other(String),
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchItem>,
}

#[derive(Deserialize)]
struct SearchItem {
    id: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct DetailsResponse {
    id: i64,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    backdrop_path: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    vote_average: Option<f64>,
    #[serde(default)]
    genres: Vec<Genre>,
}

#[derive(Deserialize)]
struct Genre {
    name: String,
}

fn qualifier_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\(\[\{][^\)\]\}]*[\)\]\}]").expect("valid qualifier regex"))
}

fn resolution_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(480p|504p|540p|576p|720p|1080p|1280p|1440p|2160p|4k|8k|uhd|fhd|hd|sd|hq|h264|hevc|mp4)\b",
        )
        .expect("valid resolution regex")
    })
}

/// Turn a playlist title into something worth searching TMDB for.
///
/// "BFM Business (1080p)" -> "BFM Business"; "Big Buck Bunny (Open Movie HLS)" ->
/// "Big Buck Bunny"; "Online TV Nusantara (720p) [Not 24/7]" -> "Online TV Nusantara".
pub fn guess_title(name: &str) -> Option<String> {
    let without_qualifiers = qualifier_regex().replace_all(name, " ");
    let without_resolution = resolution_regex().replace_all(&without_qualifiers, " ");
    let collapsed = without_resolution
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = collapsed
        .trim_end_matches(|c: char| c == '-' || c == '|' || c == ':' || c.is_whitespace())
        .trim();

    if trimmed.chars().count() < 2 {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Lowercase, keep only `[a-z0-9 ]`, collapse spaces, and drop a leading article so
/// "The Batman" and "Batman" agree.
pub fn normalize_title(value: &str) -> String {
    let cleaned: String = value
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .strip_prefix("the ")
        .unwrap_or(&collapsed)
        .to_string()
}

/// Accept only matches we can defend: an exact normalized title, or a top hit that
/// contains a reasonably long query. Anything weaker is a live channel, not a film.
pub fn titles_match(guess: &str, candidate: &str) -> bool {
    let guess_norm = normalize_title(guess);
    let candidate_norm = normalize_title(candidate);
    if guess_norm.is_empty() || candidate_norm.is_empty() {
        return false;
    }
    if guess_norm == candidate_norm {
        return true;
    }
    guess_norm.chars().count() >= 4 && candidate_norm.contains(&guess_norm)
}

/// Trims a key and treats blank input as no key.
pub fn normalize_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Resolve the API key. `LUMEN_TMDB_API_KEY` wins over the keychain copy.
///
/// A key saved by an older build sits in plaintext in the settings table. It is moved
/// into the keychain and the row is deleted, but only after the keychain write succeeds,
/// so a keychain failure leaves the original row in place.
pub async fn api_key(pool: &Pool<Sqlite>) -> Result<Option<(String, &'static str)>> {
    if let Some(key) = std::env::var("LUMEN_TMDB_API_KEY")
        .ok()
        .and_then(|value| normalize_key(&value))
    {
        return Ok(Some((key, "env")));
    }
    if let Some(key) = secrets::load_tmdb_key()?.and_then(|value| normalize_key(&value)) {
        return Ok(Some((key, "keychain")));
    }
    if let Some(legacy) = get_setting(pool, API_KEY_SETTING).await? {
        let key = normalize_key(&legacy);
        if let Some(key) = &key {
            secrets::store_tmdb_key(key)?;
        }
        delete_setting(pool, API_KEY_SETTING).await?;
        return Ok(key.map(|key| (key, "keychain")));
    }
    Ok(None)
}

/// A read-access token is a JWT; a v3 API key is not. Support both so whichever
/// credential the user copies out of their TMDB settings page just works.
fn is_bearer(key: &str) -> bool {
    key.starts_with("eyJ")
}

async fn tmdb_get(
    http: &reqwest::Client,
    key: &str,
    path: &str,
    params: &[(&str, String)],
    requests: &mut i64,
) -> std::result::Result<serde_json::Value, TmdbFailure> {
    let url = format!("{}{}", TMDB_BASE, path);

    let mut query: Vec<(String, String)> = vec![("language".to_string(), "en-US".to_string())];
    if !is_bearer(key) {
        query.push(("api_key".to_string(), key.to_string()));
    }
    for (name, value) in params {
        query.push(((*name).to_string(), value.clone()));
    }

    let mut request = http.get(&url);
    if is_bearer(key) {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    *requests += 1;
    let response = request
        .query(&query)
        .send()
        .await
        .map_err(|e| TmdbFailure::Other(crate::error::redacted(e)))?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(TmdbFailure::Unauthorized);
    }
    if !status.is_success() {
        return Err(TmdbFailure::Other(format!(
            "TMDB responded {}",
            status.as_u16()
        )));
    }

    read_capped_json(response, MAX_API_RESPONSE_BYTES).await
}

/// Largest JSON body read from TMDB. A bigger response is refused, never buffered.
const MAX_API_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Parses a TMDB body, refusing it as soon as its size passes `limit`.
async fn read_capped_json(
    mut response: reqwest::Response,
    limit: usize,
) -> std::result::Result<serde_json::Value, TmdbFailure> {
    if let Some(len) = response.content_length()
        && len > limit as u64 {
            return Err(TmdbFailure::Other(format!(
                "TMDB response is {len} bytes, over the {limit} byte limit"
            )));
        }
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| TmdbFailure::Other(crate::error::redacted(e)))?
    {
        if buffer.len() + chunk.len() > limit {
            return Err(TmdbFailure::Other(format!(
                "TMDB response is over the {limit} byte limit"
            )));
        }
        buffer.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&buffer).map_err(|e| TmdbFailure::Other(e.to_string()))
}

async fn search(
    http: &reqwest::Client,
    key: &str,
    media_type: &str,
    guess: &str,
    requests: &mut i64,
) -> std::result::Result<Option<i64>, TmdbFailure> {
    let payload = tmdb_get(
        http,
        key,
        &format!("/search/{}", media_type),
        &[
            ("query", guess.to_string()),
            ("include_adult", "false".to_string()),
        ],
        requests,
    )
    .await?;

    let parsed: SearchResponse =
        serde_json::from_value(payload).map_err(|e| TmdbFailure::Other(e.to_string()))?;

    let mut top: Option<(i64, String)> = None;
    for item in parsed.results {
        let label = item
            .title
            .or(item.name)
            .unwrap_or_default();
        if titles_match(guess, &label) {
            return Ok(Some(item.id));
        }
        if top.is_none() {
            top = Some((item.id, label));
        }
    }

    // Containment is only trusted for the top hit, and only for a usefully long query.
    if let Some((id, label)) = top {
        let guess_norm = normalize_title(guess);
        if guess_norm.chars().count() >= 4 && normalize_title(&label).contains(guess_norm.as_str()) {
            return Ok(Some(id));
        }
    }

    Ok(None)
}

async fn details(
    http: &reqwest::Client,
    key: &str,
    media_type: &str,
    id: i64,
    requests: &mut i64,
) -> std::result::Result<DetailsResponse, TmdbFailure> {
    let payload = tmdb_get(
        http,
        key,
        &format!("/{}/{}", media_type, id),
        &[],
        requests,
    )
    .await?;
    serde_json::from_value(payload).map_err(|e| TmdbFailure::Other(e.to_string()))
}

fn year_from(date: Option<&str>) -> Option<i64> {
    let text = date?.trim();
    // Slice by characters, not bytes: a malformed date must never panic the run.
    let digits: String = text.chars().take(4).collect();
    if digits.chars().count() < 4 {
        return None;
    }
    digits.parse::<i64>().ok()
}

async fn enrich_one(
    http: &reqwest::Client,
    key: &str,
    name: &str,
    requests: &mut i64,
) -> std::result::Result<Option<Found>, TmdbFailure> {
    let guess = match guess_title(name) {
        Some(value) => value,
        None => return Ok(None),
    };

    // Movies first — the open-movie catalogue is what this library can actually match.
    let mut media_type = "movie";
    let mut matched = search(http, key, "movie", &guess, requests).await?;
    if matched.is_none() {
        media_type = "tv";
        matched = search(http, key, "tv", &guess, requests).await?;
    }

    let Some(tmdb_id) = matched else {
        return Ok(None);
    };

    let detail = details(http, key, media_type, tmdb_id, requests).await?;

    let genre_names = if detail.genres.is_empty() {
        None
    } else {
        Some(
            detail
                .genres
                .iter()
                .map(|genre| genre.name.clone())
                .collect::<Vec<_>>()
                .join(";"),
        )
    };

    let release_year = year_from(
        detail
            .first_air_date
            .as_deref()
            .or(detail.release_date.as_deref()),
    );

    Ok(Some(Found {
        tmdb_id: detail.id,
        tmdb_type: if media_type == "movie" { "movie" } else { "tv" },
        poster_path: detail.poster_path,
        backdrop_path: detail.backdrop_path,
        overview: detail.overview.filter(|text| !text.trim().is_empty()),
        release_year,
        rating: detail.vote_average.map(|value| (value * 10.0).round() / 10.0),
        genre_names,
    }))
}

pub async fn enrich(pool: &Pool<Sqlite>, limit: i64, force: bool) -> Result<EnrichOutcome> {
    let (key, _source) = api_key(pool).await?.ok_or_else(|| {
        PlayerError::InvalidArg("TMDB API key not configured".into())
    })?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| PlayerError::Http(e.to_string()))?;

    let candidates: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, name FROM channels WHERE (enriched_at IS NULL OR ?) ORDER BY id LIMIT ?",
    )
    .bind(if force { 1_i64 } else { 0_i64 })
    .bind(limit.max(0))
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let now = Utc::now().timestamp();
    let mut requests = 0_i64;
    let mut matched = 0_i64;
    let mut skipped = 0_i64;
    let mut failures = 0_i64;

    for (index, (channel_id, name)) in candidates.iter().enumerate() {
        if index > 0 {
            tokio::time::sleep(Duration::from_millis(REQUEST_GAP_MS)).await;
        }

        match enrich_one(&http, &key, name, &mut requests).await {
            Err(TmdbFailure::Unauthorized) => {
                return Err(PlayerError::InvalidArg(
                    "TMDB rejected the API key (HTTP 401)".into(),
                ));
            }
            Err(TmdbFailure::Other(message)) => {
                // Report the first transport/parse failure so an outage or a changed
                // response shape is diagnosable instead of silently reading as
                // "TMDB has no match for this title".
                if failures == 0 {
                    eprintln!("TMDB enrich failed for channel {}: {}", channel_id, message);
                }
                failures += 1;
                skipped += 1;
            }
            Ok(None) => skipped += 1,
            Ok(Some(found)) => {
                sqlx::query(
                    "UPDATE channels SET tmdb_id = ?, tmdb_type = ?, poster_path = ?, backdrop_path = ?, \
                     overview = ?, release_year = ?, rating = ?, genre_names = ?, enriched_at = ? WHERE id = ?",
                )
                .bind(found.tmdb_id)
                .bind(found.tmdb_type)
                .bind(found.poster_path)
                .bind(found.backdrop_path)
                .bind(found.overview)
                .bind(found.release_year)
                .bind(found.rating)
                .bind(found.genre_names)
                .bind(now)
                .bind(channel_id)
                .execute(pool)
                .await
                .map_err(|e| PlayerError::Sql(e.to_string()))?;
                matched += 1;
            }
        }

        // Stamp every attempted row, matched or not, so re-runs move forward instead
        // of hammering the API for channels TMDB will never know about.
        sqlx::query("UPDATE channels SET enriched_at = ? WHERE id = ?")
            .bind(now)
            .bind(channel_id)
            .execute(pool)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
    }

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE enriched_at IS NULL")
        .fetch_one(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(EnrichOutcome {
        enriched: matched,
        matched,
        skipped,
        remaining,
        requests,
    })
}

pub async fn clear_enrichment(pool: &Pool<Sqlite>) -> Result<i64> {
    let affected = sqlx::query(
        "UPDATE channels SET tmdb_id = NULL, tmdb_type = NULL, poster_path = NULL, backdrop_path = NULL, \
         overview = NULL, release_year = NULL, rating = NULL, genre_names = NULL, enriched_at = NULL",
    )
    .execute(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?
    .rows_affected();

    Ok(affected as i64)
}

pub async fn counts(pool: &Pool<Sqlite>) -> Result<(i64, i64)> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    let enriched: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE tmdb_id IS NOT NULL")
        .fetch_one(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok((enriched, total))
}

/// One title from TMDB's browse lists. It is not necessarily in the local library,
/// so the UI can describe it but must not offer playback.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowseTitle {
    pub tmdb_id: i64,
    pub tmdb_type: &'static str,
    pub name: String,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub overview: Option<String>,
    pub release_year: Option<i64>,
    pub rating: Option<f64>,
    pub genre_names: Option<String>,
}

#[derive(Deserialize, Default)]
struct ListResponse {
    #[serde(default)]
    results: Vec<ListItem>,
}

#[derive(Deserialize)]
struct ListItem {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    backdrop_path: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    vote_average: Option<f64>,
    #[serde(default)]
    genre_ids: Vec<i64>,
}

#[derive(Deserialize, Default)]
struct GenreListResponse {
    #[serde(default)]
    genres: Vec<GenreEntry>,
}

#[derive(Deserialize)]
struct GenreEntry {
    id: i64,
    name: String,
}

/// Turns the raw TMDB list payloads into de-duplicated catalogue rows, best first.
/// Trending entries that are people or other non-title kinds are dropped.
fn browse_rows(
    trending: ListResponse,
    movies: ListResponse,
    shows: ListResponse,
    movie_genres: GenreListResponse,
    tv_genres: GenreListResponse,
    limit: usize,
) -> Vec<BrowseTitle> {
    let names_by_id = |list: &GenreListResponse| -> HashMap<i64, String> {
        list.genres.iter().map(|g| (g.id, g.name.clone())).collect()
    };
    let movie_names = names_by_id(&movie_genres);
    let tv_names = names_by_id(&tv_genres);

    let mut sources: Vec<(&'static str, ListItem)> = Vec::new();
    for item in trending.results {
        match item.media_type.as_deref() {
            Some("movie") => sources.push(("movie", item)),
            Some("tv") => sources.push(("tv", item)),
            _ => {}
        }
    }
    sources.extend(movies.results.into_iter().map(|item| ("movie", item)));
    sources.extend(shows.results.into_iter().map(|item| ("tv", item)));

    let mut seen: HashSet<(&'static str, i64)> = HashSet::new();
    let mut rows = Vec::new();
    for (media_type, item) in sources {
        if rows.len() >= limit {
            break;
        }
        let Some(tmdb_id) = item.id else { continue };
        let name = [item.title, item.name]
            .into_iter()
            .flatten()
            .find(|value| !value.trim().is_empty());
        let Some(name) = name else { continue };
        if !seen.insert((media_type, tmdb_id)) {
            continue;
        }

        let (primary, fallback) = if media_type == "movie" {
            (item.release_date.as_deref(), item.first_air_date.as_deref())
        } else {
            (item.first_air_date.as_deref(), item.release_date.as_deref())
        };
        let names = if media_type == "movie" { &movie_names } else { &tv_names };
        let genres: Vec<&str> = item
            .genre_ids
            .iter()
            .filter_map(|id| names.get(id).map(String::as_str))
            .collect();

        rows.push(BrowseTitle {
            tmdb_id,
            tmdb_type: media_type,
            name,
            poster_path: item.poster_path,
            backdrop_path: item.backdrop_path,
            overview: item.overview.filter(|text| !text.is_empty()),
            release_year: year_from(primary.or(fallback)),
            rating: item.vote_average.map(|vote| (vote * 10.0).round() / 10.0),
            genre_names: if genres.is_empty() { None } else { Some(genres.join(";")) },
        });
    }
    rows
}

fn browse_failure(failure: TmdbFailure) -> PlayerError {
    match failure {
        TmdbFailure::Unauthorized => {
            PlayerError::InvalidArg("TMDB rejected the API key (HTTP 401)".into())
        }
        TmdbFailure::Other(message) => PlayerError::Http(message),
    }
}

async fn browse_list<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    key: &str,
    path: &str,
    requests: &mut i64,
) -> Result<T> {
    let payload = tmdb_get(http, key, path, &[], requests)
        .await
        .map_err(browse_failure)?;
    serde_json::from_value(payload).map_err(|e| PlayerError::Json(e.to_string()))
}

/// Trending and top-rated titles, for browsing beyond the local library. Makes five requests.
pub async fn browse(key: &str) -> Result<Vec<BrowseTitle>> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| PlayerError::Http(e.to_string()))?;
    let mut requests = 0_i64;

    let trending: ListResponse = browse_list(&http, key, "/trending/all/week", &mut requests).await?;
    let movies: ListResponse = browse_list(&http, key, "/movie/top_rated", &mut requests).await?;
    let shows: ListResponse = browse_list(&http, key, "/tv/top_rated", &mut requests).await?;
    let movie_genres: GenreListResponse =
        browse_list(&http, key, "/genre/movie/list", &mut requests).await?;
    let tv_genres: GenreListResponse =
        browse_list(&http, key, "/genre/tv/list", &mut requests).await?;

    Ok(browse_rows(
        trending,
        movies,
        shows,
        movie_genres,
        tv_genres,
        BROWSE_LIMIT,
    ))
}

/// Fetches one series' schedule from TMDB.
async fn fetch_airing(
    http: &reqwest::Client,
    key: &str,
    tmdb_id: i64,
    requests: &mut i64,
) -> std::result::Result<crate::series::Airing, TmdbFailure> {
    let payload = tmdb_get(http, key, &format!("/tv/{tmdb_id}"), &[], requests).await?;
    Ok(crate::series::parse_airing(tmdb_id, &payload))
}

/// Refreshes the schedules of series this profile follows that have none yet or a stale one, at
/// most `limit` per call and politely spaced. Returns how many were refreshed. A series TMDB has
/// no record of is skipped, not retried in a loop: its failure is recorded as "checked".
pub async fn refresh_airing(pool: &Pool<Sqlite>, profile_id: i64, limit: i64) -> Result<i64> {
    let Some((key, _source)) = api_key(pool).await? else {
        return Ok(0);
    };
    let now = chrono::Utc::now().timestamp_millis();
    let ids = crate::series::series_needing_airing(pool, profile_id, now, limit).await?;
    if ids.is_empty() {
        return Ok(0);
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| PlayerError::Http(e.to_string()))?;
    let mut requests = 0_i64;
    let mut refreshed = 0;
    for id in ids {
        match fetch_airing(&http, &key, id, &mut requests).await {
            Ok(airing) => {
                crate::series::store_airing(pool, &airing, now).await?;
                refreshed += 1;
            }
            Err(TmdbFailure::Unauthorized) => {
                return Err(PlayerError::InvalidArg("TMDB rejected the API key (HTTP 401)".into()));
            }
            // Offline or a single bad id must not stop the rest, and must not be retried at once.
            Err(TmdbFailure::Other(_)) => {}
        }
        tokio::time::sleep(Duration::from_millis(REQUEST_GAP_MS)).await;
    }
    Ok(refreshed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_rows_orders_dedupes_skips_and_limits() {
        let parse = |value: serde_json::Value| -> ListResponse {
            serde_json::from_value(value).unwrap()
        };
        let trending = parse(serde_json::json!({"results": [
            {"id": 1, "media_type": "movie", "title": "Alpha", "release_date": "2024-03-01",
             "vote_average": 7.456, "genre_ids": [28, 999], "overview": "Trending film"},
            {"id": 50, "media_type": "person", "name": "Not a title"},
            {"id": 2, "media_type": "tv", "name": "Beta", "first_air_date": "2021-09-10",
             "vote_average": 8.0, "genre_ids": [10759]},
        ]}));
        let movies = parse(serde_json::json!({"results": [
            {"id": 1, "title": "Alpha", "release_date": "2024-03-01"},
            {"id": 3, "title": "Gamma", "release_date": "", "genre_ids": []},
            {"title": "No id"},
            {"id": 9, "title": "   "},
        ]}));
        let shows = parse(serde_json::json!({"results": [
            {"id": 2, "name": "Beta duplicate"},
            {"id": 4, "name": "Delta", "first_air_date": "1999-01-01"},
        ]}));
        let genres = |value: serde_json::Value| -> GenreListResponse {
            serde_json::from_value(value).unwrap()
        };
        let movie_genres = genres(serde_json::json!({"genres": [{"id": 28, "name": "Action"}]}));
        let tv_genres = genres(serde_json::json!({"genres": [{"id": 10759, "name": "Action & Adventure"}]}));

        let rows = browse_rows(trending, movies, shows, movie_genres, tv_genres, 10);
        let keys: Vec<(&str, i64)> = rows.iter().map(|r| (r.tmdb_type, r.tmdb_id)).collect();
        assert_eq!(keys, vec![("movie", 1), ("tv", 2), ("movie", 3), ("tv", 4)]);

        assert_eq!(rows[0].name, "Alpha");
        assert_eq!(rows[0].release_year, Some(2024));
        assert_eq!(rows[0].rating, Some(7.5));
        assert_eq!(rows[0].genre_names.as_deref(), Some("Action"));
        assert_eq!(rows[0].overview.as_deref(), Some("Trending film"));
        assert_eq!(rows[1].genre_names.as_deref(), Some("Action & Adventure"));
        assert_eq!(rows[2].release_year, None);
        assert_eq!(rows[2].rating, None);
        assert_eq!(rows[2].genre_names, None);
        assert_eq!(rows[3].release_year, Some(1999));
    }

    #[test]
    fn browse_rows_respects_limit() {
        let movies: ListResponse = serde_json::from_value(serde_json::json!({"results": [
            {"id": 1, "title": "One"}, {"id": 2, "title": "Two"}, {"id": 3, "title": "Three"},
        ]})).unwrap();
        let rows = browse_rows(
            ListResponse::default(),
            movies,
            ListResponse::default(),
            GenreListResponse::default(),
            GenreListResponse::default(),
            2,
        );
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn normalize_key_trims_and_rejects_blank_input() {
        assert_eq!(normalize_key("  abc123 \n").as_deref(), Some("abc123"));
        assert_eq!(normalize_key(""), None);
        assert_eq!(normalize_key(" \t\n"), None);
    }

    #[test]
    fn test_guess_title_strips_qualifiers_and_resolution() {
        assert_eq!(guess_title("BFM Business (1080p)").as_deref(), Some("BFM Business"));
        assert_eq!(
            guess_title("Big Buck Bunny (Open Movie HLS)").as_deref(),
            Some("Big Buck Bunny")
        );
        assert_eq!(
            guess_title("Tears of Steel (Blender Open Movie)").as_deref(),
            Some("Tears of Steel")
        );
        assert_eq!(
            guess_title("Online TV Nusantara (720p) [Not 24/7]").as_deref(),
            Some("Online TV Nusantara")
        );
        assert_eq!(guess_title("HD").as_deref(), None, "quality-only names are not searchable");
    }

    #[test]
    fn test_titles_match_requires_a_defensible_match() {
        assert!(titles_match("Big Buck Bunny", "Big Buck Bunny"));
        assert!(titles_match("Tears of Steel", "tears of steel"));
        assert!(titles_match("Big Buck Bunny", "Big Buck Bunny 2"), "superset passes containment");
        assert!(!titles_match("BFM Business", "The Business"), "unrelated titles do not match");
        assert!(!titles_match("DW", "DW News"), "queries shorter than 4 chars need an exact hit");
        assert!(!titles_match("NHK", "NHK World"));
    }

    #[test]
    fn test_normalize_title_drops_articles_and_punctuation() {
        assert_eq!(normalize_title("The Batman"), "batman");
        assert_eq!(normalize_title("E! Latin America"), "e latin america");
        assert_eq!(normalize_title("  BBC   News  "), "bbc news");
    }

    /// Accepts one connection on loopback, reads the request, and answers with `response`.
    async fn serve_fixed(body: &'static [u8]) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body).await;
        });
        format!("http://{addr}/")
    }

    /// Accepts one connection and streams chunked data until the client disconnects.
    async fn serve_endless() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await;
            let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            if stream.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let chunk = [b'x'; 16384];
            loop {
                let frame = format!("{:x}\r\n", chunk.len());
                if stream.write_all(frame.as_bytes()).await.is_err()
                    || stream.write_all(&chunk).await.is_err()
                    || stream.write_all(b"\r\n").await.is_err()
                {
                    break;
                }
            }
        });
        format!("http://{addr}/")
    }

    fn failure_message(result: std::result::Result<serde_json::Value, TmdbFailure>) -> String {
        match result {
            Err(TmdbFailure::Other(message)) => message,
            Err(TmdbFailure::Unauthorized) => "unauthorized".to_string(),
            Ok(value) => panic!("expected a refused body, got {value}"),
        }
    }

    #[tokio::test]
    async fn capped_json_parses_a_body_within_the_limit() {
        let url = serve_fixed(br#"{"results":[{"id":7}]}"#).await;
        let response = reqwest::Client::new().get(&url).send().await.unwrap();
        let value = match read_capped_json(response, 1024).await {
            Ok(value) => value,
            Err(TmdbFailure::Other(message)) => panic!("unexpected refusal: {message}"),
            Err(TmdbFailure::Unauthorized) => panic!("unexpected unauthorized"),
        };
        assert_eq!(value["results"][0]["id"], 7);
    }

    #[tokio::test]
    async fn declared_length_over_the_limit_is_refused() {
        let url = serve_fixed(br#"{"results":[]}"#).await;
        let response = reqwest::Client::new().get(&url).send().await.unwrap();
        let message = failure_message(read_capped_json(response, 10).await);
        assert!(message.contains("over the 10 byte limit"), "{message}");
    }

    #[tokio::test]
    async fn endless_stream_is_cut_off_at_the_limit() {
        let url = serve_endless().await;
        let response = reqwest::Client::new().get(&url).send().await.unwrap();
        let message = failure_message(read_capped_json(response, 64 * 1024).await);
        assert!(message.contains("over the 65536 byte limit"), "{message}");
    }
}
