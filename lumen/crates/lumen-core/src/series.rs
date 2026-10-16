//! Series state: hand-set watched marks, and TMDB's airing schedule for series you follow.
//!
//! What counts as watched is decided in the UI from two sources: these marks, which win, and
//! finished playback in history. Only the marks are stored here, so "mark unwatched" can override
//! a finished episode without rewriting history.

use serde::Serialize;
use serde_json::Value;
use sqlx::{Pool, Row, Sqlite};

use crate::error::{PlayerError, Result};
use crate::sync;

const MAX_SERIES_KEY_CHARS: usize = 200;
const MAX_EPISODES_PER_CALL: usize = 500;
const MAX_SEASON: i64 = 500;
const MAX_EPISODE: i64 = 10_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeMark {
    pub series_key: String,
    pub season: i64,
    pub episode: i64,
    pub watched: bool,
    pub updated_at: i64,
}

fn check_series(series_key: &str, season: i64, episodes: &[i64]) -> Result<()> {
    if series_key.trim().is_empty() || series_key.chars().count() > MAX_SERIES_KEY_CHARS {
        return Err(PlayerError::InvalidArg("That series has no usable name".into()));
    }
    if !(0..=MAX_SEASON).contains(&season) {
        return Err(PlayerError::InvalidArg("Season number out of range".into()));
    }
    if episodes.len() > MAX_EPISODES_PER_CALL || episodes.iter().any(|episode| !(0..=MAX_EPISODE).contains(episode)) {
        return Err(PlayerError::InvalidArg("Episode numbers out of range".into()));
    }
    Ok(())
}

/// Writes one hand-set mark and queues it for sync, on the caller's transaction.
pub(crate) async fn write_mark(
    conn: &mut sqlx::SqliteConnection,
    profile_id: i64,
    series_key: &str,
    season: i64,
    episode: i64,
    watched: bool,
    now: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO episode_state (profile_id, series_key, season, episode, watched, updated_at) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (profile_id, series_key, season, episode) DO UPDATE SET \
           watched = excluded.watched, updated_at = excluded.updated_at \
         WHERE excluded.updated_at >= episode_state.updated_at",
    )
    .bind(profile_id)
    .bind(series_key)
    .bind(season)
    .bind(episode)
    .bind(i64::from(watched))
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;
    sync::queue_episode(conn, profile_id, series_key, season, episode, watched, now).await
}

/// Marks episodes of one season watched or unwatched for a profile, all or nothing.
pub async fn set_episodes_watched(
    pool: &Pool<Sqlite>,
    profile_id: i64,
    series_key: &str,
    season: i64,
    episodes: &[i64],
    watched: bool,
    now: i64,
) -> Result<()> {
    check_series(series_key, season, episodes)?;
    let mut tx = pool.begin().await.map_err(|e| PlayerError::Sql(e.to_string()))?;
    for episode in episodes {
        write_mark(&mut tx, profile_id, series_key, season, *episode, watched, now).await?;
    }
    tx.commit().await.map_err(|e| PlayerError::Sql(e.to_string()))
}

pub async fn list_episode_marks(pool: &Pool<Sqlite>, profile_id: i64) -> Result<Vec<EpisodeMark>> {
    let rows = sqlx::query(
        "SELECT series_key, season, episode, watched, updated_at FROM episode_state WHERE profile_id = ? \
         ORDER BY series_key, season, episode",
    )
    .bind(profile_id)
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| EpisodeMark {
            series_key: row.get("series_key"),
            season: row.get("season"),
            episode: row.get("episode"),
            watched: row.get::<i64, _>("watched") != 0,
            updated_at: row.get("updated_at"),
        })
        .collect())
}

/// A series' schedule as TMDB reports it.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Airing {
    pub tmdb_id: i64,
    pub name: String,
    pub status: Option<String>,
    pub next_air_date: Option<String>,
    pub next_season: Option<i64>,
    pub next_episode: Option<i64>,
    pub next_name: Option<String>,
    pub last_air_date: Option<String>,
    pub last_season: Option<i64>,
    pub last_episode: Option<i64>,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Reads the schedule out of a TMDB `/tv/{id}` response. A missing or null episode is simply
/// absent: finished shows have no next episode.
pub fn parse_airing(tmdb_id: i64, payload: &Value) -> Airing {
    let episode = |key: &str| payload.get(key).filter(|value| value.is_object());
    let next = episode("next_episode_to_air");
    let last = episode("last_episode_to_air");
    Airing {
        tmdb_id,
        name: text(payload, "name").unwrap_or_default(),
        status: text(payload, "status"),
        next_air_date: next.and_then(|n| text(n, "air_date")),
        next_season: next.and_then(|n| n.get("season_number")).and_then(Value::as_i64),
        next_episode: next.and_then(|n| n.get("episode_number")).and_then(Value::as_i64),
        next_name: next.and_then(|n| text(n, "name")),
        last_air_date: last.and_then(|n| text(n, "air_date")),
        last_season: last.and_then(|n| n.get("season_number")).and_then(Value::as_i64),
        last_episode: last.and_then(|n| n.get("episode_number")).and_then(Value::as_i64),
    }
}

pub async fn store_airing(pool: &Pool<Sqlite>, airing: &Airing, now: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO series_airing (tmdb_id, name, status, next_air_date, next_season, next_episode, next_name, \
                                    last_air_date, last_season, last_episode, checked_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (tmdb_id) DO UPDATE SET \
           name = excluded.name, status = excluded.status, next_air_date = excluded.next_air_date, \
           next_season = excluded.next_season, next_episode = excluded.next_episode, next_name = excluded.next_name, \
           last_air_date = excluded.last_air_date, last_season = excluded.last_season, \
           last_episode = excluded.last_episode, checked_at = excluded.checked_at",
    )
    .bind(airing.tmdb_id)
    .bind(&airing.name)
    .bind(&airing.status)
    .bind(&airing.next_air_date)
    .bind(airing.next_season)
    .bind(airing.next_episode)
    .bind(&airing.next_name)
    .bind(&airing.last_air_date)
    .bind(airing.last_season)
    .bind(airing.last_episode)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(())
}

/// How long a stored schedule is trusted before TMDB is asked again.
pub const AIRING_MAX_AGE_MS: i64 = 12 * 60 * 60 * 1000;

/// TMDB series ids this profile follows (played or saved) whose schedule is missing or stale.
pub async fn series_needing_airing(pool: &Pool<Sqlite>, profile_id: i64, now: i64, limit: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT DISTINCT c.tmdb_id FROM channels c \
         LEFT JOIN series_airing a ON a.tmdb_id = c.tmdb_id \
         WHERE c.tmdb_type = 'tv' AND c.tmdb_id IS NOT NULL \
           AND (a.tmdb_id IS NULL OR a.checked_at < ?) \
           AND (c.id IN (SELECT channel_id FROM history WHERE profile_id = ? AND channel_id IS NOT NULL) \
                OR c.url IN (SELECT channel_url FROM favorites WHERE profile_id = ? AND active = 1)) \
         ORDER BY c.tmdb_id LIMIT ?",
    )
    .bind(now - AIRING_MAX_AGE_MS)
    .bind(profile_id)
    .bind(profile_id)
    .bind(limit.clamp(1, 50))
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))
}

/// Followed series with something to say: an episode still to air, or one that aired in the last
/// two weeks. `today` is `YYYY-MM-DD` (TMDB dates are plain calendar dates).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpcomingSeries {
    #[serde(flatten)]
    pub airing: Airing,
    /// One library entry of this series, so the card can open it.
    pub channel_id: Option<i64>,
}

pub async fn list_upcoming(pool: &Pool<Sqlite>, profile_id: i64, today: &str, recent_from: &str) -> Result<Vec<UpcomingSeries>> {
    let rows = sqlx::query(
        "SELECT a.tmdb_id, a.name, a.status, a.next_air_date, a.next_season, a.next_episode, a.next_name, \
                a.last_air_date, a.last_season, a.last_episode, \
                (SELECT MIN(c.id) FROM channels c WHERE c.tmdb_id = a.tmdb_id AND c.tmdb_type = 'tv') AS channel_id \
         FROM series_airing a \
         WHERE a.tmdb_id IN ( \
               SELECT c.tmdb_id FROM channels c WHERE c.tmdb_type = 'tv' AND c.tmdb_id IS NOT NULL AND \
                 (c.id IN (SELECT channel_id FROM history WHERE profile_id = ? AND channel_id IS NOT NULL) \
                  OR c.url IN (SELECT channel_url FROM favorites WHERE profile_id = ? AND active = 1))) \
           AND ((a.next_air_date IS NOT NULL AND a.next_air_date >= ?) \
                OR (a.last_air_date IS NOT NULL AND a.last_air_date >= ? AND a.last_air_date <= ?)) \
         ORDER BY COALESCE(CASE WHEN a.next_air_date >= ? THEN a.next_air_date END, a.last_air_date) ASC",
    )
    .bind(profile_id)
    .bind(profile_id)
    .bind(today)
    .bind(recent_from)
    .bind(today)
    .bind(today)
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| UpcomingSeries {
            airing: Airing {
                tmdb_id: row.get("tmdb_id"),
                name: row.get("name"),
                status: row.get("status"),
                next_air_date: row.get("next_air_date"),
                next_season: row.get("next_season"),
                next_episode: row.get("next_episode"),
                next_name: row.get("next_name"),
                last_air_date: row.get("last_air_date"),
                last_season: row.get("last_season"),
                last_episode: row.get("last_episode"),
            },
            channel_id: row.get("channel_id"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> Pool<Sqlite> {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::query("PRAGMA foreign_keys = ON;").execute(&pool).await.unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        crate::db::migrate_tmdb_columns(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn marks_are_stored_per_profile_and_unwatched_is_an_answer_not_a_deletion() {
        let pool = pool().await;
        set_episodes_watched(&pool, 1, "tmdb:tv:42", 1, &[1, 2, 3], true, 1_000).await.unwrap();
        set_episodes_watched(&pool, 1, "tmdb:tv:42", 1, &[2], false, 2_000).await.unwrap();
        let marks = list_episode_marks(&pool, 1).await.unwrap();
        assert_eq!(marks.iter().map(|m| (m.episode, m.watched)).collect::<Vec<_>>(), vec![(1, true), (2, false), (3, true)]);
        assert!(list_episode_marks(&pool, 2).await.unwrap().is_empty(), "another profile has its own marks");

        // An older write (a slow device) does not undo a newer one.
        set_episodes_watched(&pool, 1, "tmdb:tv:42", 1, &[2], true, 1_500).await.unwrap();
        assert!(!list_episode_marks(&pool, 1).await.unwrap()[1].watched);
    }

    #[tokio::test]
    async fn marks_are_validated_and_all_or_nothing() {
        let pool = pool().await;
        assert!(set_episodes_watched(&pool, 1, "  ", 1, &[1], true, 1).await.is_err());
        assert!(set_episodes_watched(&pool, 1, "s", -1, &[1], true, 1).await.is_err());
        assert!(set_episodes_watched(&pool, 1, "s", 1, &[1, 99_999], true, 1).await.is_err());
        assert!(set_episodes_watched(&pool, 1, "s", 1, &vec![1; 501], true, 1).await.is_err());
        assert!(list_episode_marks(&pool, 1).await.unwrap().is_empty(), "a refused call writes nothing");
    }

    #[tokio::test]
    async fn deleting_a_profile_removes_its_marks() {
        let pool = pool().await;
        let second = crate::db::create_profile(&pool, "Guest", None).await.unwrap();
        set_episodes_watched(&pool, second.id, "name:show", 1, &[1], true, 1).await.unwrap();
        crate::db::delete_profile(&pool, second.id).await.unwrap();
        assert!(list_episode_marks(&pool, second.id).await.unwrap().is_empty());
    }

    #[test]
    fn airing_is_read_from_a_tmdb_series_response() {
        let ongoing = json!({
            "name": "Severance", "status": "Returning Series",
            "next_episode_to_air": {"air_date": "2026-11-01", "season_number": 3, "episode_number": 4, "name": "Cold Harbor"},
            "last_episode_to_air": {"air_date": "2026-10-25", "season_number": 3, "episode_number": 3, "name": "Hello"},
        });
        let airing = parse_airing(7, &ongoing);
        assert_eq!((airing.next_season, airing.next_episode), (Some(3), Some(4)));
        assert_eq!(airing.next_air_date.as_deref(), Some("2026-11-01"));
        assert_eq!(airing.last_air_date.as_deref(), Some("2026-10-25"));
        assert_eq!(airing.status.as_deref(), Some("Returning Series"));

        let ended = parse_airing(8, &json!({"name": "Done", "status": "Ended", "next_episode_to_air": null, "last_episode_to_air": {"air_date": "2020-01-01", "season_number": 5, "episode_number": 10}}));
        assert_eq!(ended.next_air_date, None);
        assert_eq!(ended.last_episode, Some(10));

        let junk = parse_airing(9, &json!("not an object"));
        assert_eq!(junk, Airing { tmdb_id: 9, ..Airing::default() });
    }

    async fn follow(pool: &Pool<Sqlite>, channel_id: i64, tmdb_id: i64, how: &str) {
        sqlx::query("INSERT OR IGNORE INTO playlists (id, name, source, created_at, updated_at) VALUES (1, 'p', 'url', 0, 0)").execute(pool).await.unwrap();
        let url = format!("https://e.example/{channel_id}");
        sqlx::query("INSERT INTO channels (id, playlist_id, name, url, tmdb_id, tmdb_type) VALUES (?, 1, 'Show S01E01', ?, ?, 'tv')")
            .bind(channel_id).bind(&url).bind(tmdb_id).execute(pool).await.unwrap();
        match how {
            "history" => {
                sqlx::query("INSERT INTO history (profile_id, channel_id, path, position, duration, played_at) VALUES (1, ?, ?, 10, 100, 1)")
                    .bind(channel_id).bind(&url).execute(pool).await.unwrap();
            }
            "favorite" => {
                sqlx::query("INSERT INTO favorites (profile_id, channel_url, active, updated_at) VALUES (1, ?, 1, 1)")
                    .bind(&url).execute(pool).await.unwrap();
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn only_followed_series_with_missing_or_stale_schedules_are_refreshed() {
        let pool = pool().await;
        follow(&pool, 1, 100, "history").await; // followed, never checked
        follow(&pool, 2, 200, "favorite").await; // followed, checked just now
        follow(&pool, 3, 300, "history").await; // followed, checked long ago
        follow(&pool, 4, 400, "none").await; // in the library but not followed
        let now = 10 * AIRING_MAX_AGE_MS;
        store_airing(&pool, &Airing { tmdb_id: 200, name: "B".into(), ..Default::default() }, now - 1_000).await.unwrap();
        store_airing(&pool, &Airing { tmdb_id: 300, name: "C".into(), ..Default::default() }, now - AIRING_MAX_AGE_MS - 1).await.unwrap();
        assert_eq!(series_needing_airing(&pool, 1, now, 10).await.unwrap(), vec![100, 300]);
        assert_eq!(series_needing_airing(&pool, 1, now, 1).await.unwrap(), vec![100], "limit applies");
        assert!(series_needing_airing(&pool, 2, now, 10).await.unwrap().is_empty(), "another profile follows nothing");
    }

    #[tokio::test]
    async fn upcoming_lists_episodes_still_to_air_and_ones_that_just_aired() {
        let pool = pool().await;
        for (channel, tmdb, how) in [(1, 100, "history"), (2, 200, "favorite"), (3, 300, "history"), (4, 400, "history"), (5, 500, "none")] {
            follow(&pool, channel, tmdb, how).await;
        }
        let at = |tmdb_id, next: Option<&str>, last: Option<&str>| Airing {
            tmdb_id, name: format!("Show {tmdb_id}"), next_air_date: next.map(String::from), next_season: next.map(|_| 2), next_episode: next.map(|_| 5),
            last_air_date: last.map(String::from), last_season: last.map(|_| 2), last_episode: last.map(|_| 4), ..Default::default()
        };
        store_airing(&pool, &at(100, Some("2026-10-20"), Some("2026-10-13")), 1).await.unwrap(); // upcoming
        store_airing(&pool, &at(200, None, Some("2026-10-14")), 1).await.unwrap(); // aired two days ago
        store_airing(&pool, &at(300, None, Some("2026-08-01")), 1).await.unwrap(); // old news
        store_airing(&pool, &at(400, Some("2026-10-10"), Some("2026-09-30")), 1).await.unwrap(); // stale "next": already past, nothing new in two weeks
        store_airing(&pool, &at(500, Some("2026-10-22"), None), 1).await.unwrap(); // not followed
        let upcoming = list_upcoming(&pool, 1, "2026-10-16", "2026-10-02").await.unwrap();
        let ids: Vec<_> = upcoming.iter().map(|u| u.airing.tmdb_id).collect();
        assert_eq!(ids, vec![200, 100], "soonest first: what aired two days ago, then what airs on the 20th");
        assert_eq!(upcoming[1].channel_id, Some(1), "the card can open a library entry");
    }
}
