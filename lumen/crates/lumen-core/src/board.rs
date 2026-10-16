//! Data behind the Home board: when this profile last visited, and which titles are new since.
//!
//! "New" is judged on `titles.created_at`, which is set once when a title first appears and kept
//! across playlist refreshes, so re-importing a playlist does not make the whole library look new.

use std::collections::HashMap;
use std::sync::Mutex;

use sqlx::{Pool, Sqlite};

use crate::db::{get_setting, set_setting};
use crate::error::{PlayerError, Result};

/// What this process already told each profile about its previous visit. "Since your last visit"
/// must stay the same for the whole session, so the stored time is replaced once per process and
/// every later call returns what was read the first time.
static SESSION_VISITS: Mutex<Option<HashMap<i64, Option<i64>>>> = Mutex::new(None);

fn visit_key(profile_id: i64) -> String {
    format!("last_visit_{profile_id}")
}

/// Starts this session's visit for a profile: returns when the profile last visited (Unix
/// milliseconds), or `None` on the first ever visit, and records `now_ms` as the new visit time.
/// Repeated calls in one process return the same previous time and do not move the stored one.
pub async fn begin_visit(pool: &Pool<Sqlite>, profile_id: i64, now_ms: i64) -> Result<Option<i64>> {
    if let Some(known) = SESSION_VISITS
        .lock()
        .expect("visit lock")
        .get_or_insert_with(HashMap::new)
        .get(&profile_id)
    {
        return Ok(*known);
    }
    let previous = get_setting(pool, &visit_key(profile_id))
        .await?
        .and_then(|value| value.parse::<i64>().ok());
    set_setting(pool, &visit_key(profile_id), &now_ms.to_string()).await?;
    SESSION_VISITS
        .lock()
        .expect("visit lock")
        .get_or_insert_with(HashMap::new)
        .insert(profile_id, previous);
    Ok(previous)
}

/// Library entries whose title first appeared after `since_ms`, newest first, one per title.
/// Live channels are left out: they are not "new releases".
pub async fn new_title_channel_ids(
    pool: &Pool<Sqlite>,
    since_ms: i64,
    limit: i64,
) -> Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT channel_id FROM ( \
           SELECT ts.channel_id AS channel_id, t.created_at AS created_at, \
                  ROW_NUMBER() OVER (PARTITION BY t.content_key ORDER BY ts.rank DESC, ts.id ASC) AS pick \
           FROM titles t JOIN title_sources ts ON ts.content_key = t.content_key \
           WHERE t.created_at > ? AND t.kind != 'channel' AND ts.channel_id IS NOT NULL \
         ) WHERE pick = 1 ORDER BY created_at DESC, channel_id ASC LIMIT ?",
    )
    .bind(since_ms)
    .bind(limit.clamp(1, 200))
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

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

    #[tokio::test]
    async fn a_visit_reports_the_previous_time_once_per_session() {
        let pool = pool().await;
        // Profile ids are unique to this test: the session memo is process-wide.
        assert_eq!(
            begin_visit(&pool, 9_001, 1_000).await.unwrap(),
            None,
            "first ever visit"
        );
        assert_eq!(
            begin_visit(&pool, 9_001, 5_000).await.unwrap(),
            None,
            "same session: same answer, stored time not moved"
        );
        assert_eq!(
            get_setting(&pool, "last_visit_9001")
                .await
                .unwrap()
                .as_deref(),
            Some("1000")
        );

        sqlx::query("INSERT INTO settings (key, value) VALUES ('last_visit_9002', '4200')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            begin_visit(&pool, 9_002, 8_000).await.unwrap(),
            Some(4_200),
            "a stored visit is returned"
        );
        assert_eq!(
            get_setting(&pool, "last_visit_9002")
                .await
                .unwrap()
                .as_deref(),
            Some("8000"),
            "and replaced by this one"
        );
        assert_eq!(begin_visit(&pool, 9_002, 9_000).await.unwrap(), Some(4_200));
    }

    async fn add_title(
        pool: &Pool<Sqlite>,
        key: &str,
        kind: &str,
        created_at: i64,
        channels: &[(i64, i64)],
    ) {
        sqlx::query("INSERT INTO titles (content_key, kind, title, created_at, updated_at) VALUES (?, ?, ?, ?, ?)")
            .bind(key).bind(kind).bind(key).bind(created_at).bind(created_at)
            .execute(pool).await.unwrap();
        for (channel_id, rank) in channels {
            sqlx::query("INSERT OR IGNORE INTO playlists (id, name, source, created_at, updated_at) VALUES (1, 'p', 'url', 0, 0)").execute(pool).await.unwrap();
            sqlx::query("INSERT INTO channels (id, playlist_id, name, url) VALUES (?, 1, ?, ?)")
                .bind(channel_id)
                .bind(key)
                .bind(format!("https://e.example/{channel_id}"))
                .execute(pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO title_sources (content_key, channel_id, stream_url, rank, created_at) VALUES (?, ?, ?, ?, ?)")
                .bind(key).bind(channel_id).bind(format!("https://e.example/{channel_id}")).bind(rank).bind(created_at)
                .execute(pool).await.unwrap();
        }
    }

    #[tokio::test]
    async fn new_titles_are_newer_than_the_visit_one_per_title_and_never_live_channels() {
        let pool = pool().await;
        add_title(&pool, "tmdb:movie:1", "movie", 1_000, &[(1, 0)]).await; // before the visit
        add_title(&pool, "tmdb:movie:2", "movie", 3_000, &[(2, 0), (3, 5)]).await; // two sources: the higher rank is chosen
        add_title(&pool, "tmdb:tv:3:S01E01", "series", 4_000, &[(4, 0)]).await;
        add_title(&pool, "tvg:news", "channel", 5_000, &[(5, 0)]).await; // live channel
        assert_eq!(
            new_title_channel_ids(&pool, 2_000, 10).await.unwrap(),
            vec![4, 3],
            "newest first, best source, no live channels"
        );
        assert_eq!(
            new_title_channel_ids(&pool, 2_000, 1).await.unwrap(),
            vec![4],
            "limit applies"
        );
        assert!(
            new_title_channel_ids(&pool, 9_000, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
