//! Versioned migration from the pre-v2 `content_key` derivation to v2 (audit issues 6, 7, 15).
//!
//! The key derivation changed in two ways: stream fallbacks keep distinguishing query parameters
//! (stripping only credentials/volatile ones) and use the new `stream2:` prefix, and placeholder
//! `tvg-id`s such as `None` no longer become a shared `tvg:none` identity. Those keys are stored
//! in favorites, history, titles, title_sources and the sync queue, so switching the derivation
//! without rekeying would orphan existing My List/resume data.
//!
//! Properties this runner must keep:
//! - **Resumable:** the cursor is committed in the same transaction as the rows of that batch.
//! - **Idempotent:** every update matches the legacy key too, so re-running a batch is a no-op.
//! - **Bounded memory:** keyset pagination everywhere; never `fetch_all` a library table.
//! - **Backed up first:** `VACUUM INTO` a sibling file before the first write, and skip the
//!   migration if the backup fails. The backup is the rollback.
//! - **Verified before completion:** `content_key_version = 2` is written only after every
//!   stored key is non-legacy and `PRAGMA quick_check` says `ok`.

use crate::content_key::ContentKey;
use crate::db::{
    TITLES_BACKFILL_SETTING, TitleBackfillRow, batch_upsert_titles_and_sources,
    title_backfill_row_from,
};
use crate::error::{PlayerError, Result};
use crate::sync;
use chrono::Utc;
use sqlx::{Pool, Row, Sqlite};
use std::time::Duration;

pub const CONTENT_KEY_VERSION_SETTING: &str = "content_key_version";
const CURSOR_SETTING: &str = "content_key_migration_cursor";
const STARTED_SETTING: &str = "content_key_migration_started_at";
const BATCH_SIZE: i64 = 500;

/// Serializes the job inside one process. `init_db` only starts it once, but tests and a future
/// explicit retry command must not run two overlapping passes.
static MIGRATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Keys that only the pre-v2 derivation produced. The v2 stream prefix is `stream2:`, so a
/// `stream:` row is unambiguously legacy.
const LEGACY_KEY_FILTER: &str = "(content_key LIKE 'stream:%' OR lower(content_key) IN \
    ('tvg:none','tvg:null','tvg:nil','tvg:n/a','tvg:na','tvg:-','tvg:unknown','tvg:undefined'))";

/// Runs the migration to completion, or returns `Ok(false)` if it is paused between batches
/// (tests use that to prove the cursor resumes). A second call after completion is a no-op.
pub async fn migrate_content_keys_v2(pool: &Pool<Sqlite>) -> Result<bool> {
    migrate_inner(pool, BATCH_SIZE, None).await
}

pub(crate) async fn migrate_inner(
    pool: &Pool<Sqlite>,
    batch_size: i64,
    max_batches: Option<usize>,
) -> Result<bool> {
    let _guard = MIGRATION_LOCK.lock().await;
    if setting(pool, CONTENT_KEY_VERSION_SETTING).await?.as_deref() == Some("2") {
        return Ok(true);
    }

    let channels: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(pool)
        .await
        .map_err(sql_err)?;
    if channels == 0 {
        // Nothing to rekey. Do not create a backup of an empty database.
        finish(pool).await?;
        return Ok(true);
    }

    ensure_backup(pool).await?;

    // Only queue sync work when this device has actually synced. `profile_sync_key` exists on
    // every profile, so queueing unconditionally would write one queue row per favorite/history
    // row on a huge, sync-disabled library.
    let sync_used = setting(pool, "sync_last_synced_at").await?.is_some()
        || setting(pool, "sync_pull_cursor_seq").await?.is_some();
    // Empty tables are common (a fresh library, or titles not yet backfilled). Knowing this up
    // front turns the per-channel pass into a pure scan instead of three pointless lookups for
    // each of 14.76M channels.
    let has_favorites = count(pool, "favorites").await? > 0;
    let has_history = count(pool, "history").await? > 0;
    let has_title_sources = count(pool, "title_sources").await? > 0;

    let mut batches = 0usize;
    loop {
        if max_batches.is_some_and(|max| batches >= max) {
            return Ok(false);
        }
        let cursor = setting(pool, CURSOR_SETTING)
            .await?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let rows = sqlx::query(
            "SELECT id, name, url, tvg_id, tmdb_id, tmdb_type FROM channels \
             WHERE id > ? ORDER BY id LIMIT ?",
        )
        .bind(cursor)
        .bind(batch_size)
        .fetch_all(pool)
        .await
        .map_err(sql_err)?;
        if rows.is_empty() {
            break;
        }

        let now = Utc::now().timestamp_millis();
        let mut tx = pool.begin().await.map_err(sql_err)?;
        let mut last_id = cursor;
        for row in &rows {
            let id: i64 = row.get("id");
            last_id = id;
            let name: String = row.get("name");
            let url: String = row.get("url");
            let tvg_id: Option<String> = row.get("tvg_id");
            let tmdb_id: Option<i64> = row.get("tmdb_id");
            let tmdb_type: Option<String> = row.get("tmdb_type");
            let legacy = ContentKey::derive_legacy(
                tmdb_id,
                tmdb_type.as_deref(),
                &name,
                tvg_id.as_deref(),
                &url,
            )
            .into_string();
            let v2 = ContentKey::derive(
                tmdb_id,
                tmdb_type.as_deref(),
                &name,
                tvg_id.as_deref(),
                &url,
            )
            .into_string();
            if legacy != v2 {
                rekey_channel(
                    &mut tx,
                    id,
                    &Rekey {
                        name: &name,
                        url: &url,
                        legacy: &legacy,
                        v2: &v2,
                        now,
                        sync_used,
                        has_favorites,
                        has_history,
                        has_title_sources,
                    },
                )
                .await?;
            }
        }
        write_setting_tx(&mut tx, CURSOR_SETTING, &last_id.to_string()).await?;
        tx.commit().await.map_err(sql_err)?;
        batches += 1;
        if batches.is_multiple_of(100) {
            tracing::info!(batches, last_channel_id = last_id, "content-key v2 batch");
        }
        // Let playback and browsing keep their writes between batches; a 14.76M-row library is
        // ~29,500 transactions and this must not monopolize the pool.
        tokio::time::sleep(Duration::from_millis(2)).await;
        if rows.len() < batch_size as usize {
            // The final page; the next query would be empty. Fall through to cleanup now.
            break;
        }
    }

    // Rows whose channel is gone (deleted playlist, old backfill's hex keys) cannot be rekeyed by
    // channel id. Rekey them from their own URL/path.
    rekey_orphan_favorites(pool, batch_size, sync_used).await?;
    rekey_orphan_history(pool, batch_size, sync_used).await?;
    rekey_orphan_title_sources(pool, batch_size).await?;
    rebuild_titles(pool, batch_size).await?;
    verify(pool).await?;
    finish(pool).await?;
    tracing::info!("content-key v2 migration complete");
    Ok(true)
}

struct Rekey<'a> {
    name: &'a str,
    url: &'a str,
    legacy: &'a str,
    v2: &'a str,
    now: i64,
    sync_used: bool,
    has_favorites: bool,
    has_history: bool,
    has_title_sources: bool,
}

/// One channel's rows. Updates are scoped by the legacy key as well as the URL/path, so two
/// channels that shared a legacy key split onto their own v2 keys one row at a time.
async fn rekey_channel(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    channel_id: i64,
    row: &Rekey<'_>,
) -> Result<()> {
    let favorites: Vec<(i64, String, i64)> = if row.has_favorites {
        sqlx::query_as(
            "SELECT profile_id, channel_url, active FROM favorites \
             WHERE channel_url = ? AND content_key = ?",
        )
        .bind(row.url)
        .bind(row.legacy)
        .fetch_all(&mut **tx)
        .await
        .map_err(sql_err)?
    } else {
        Vec::new()
    };
    for (profile_id, channel_url, active) in favorites {
        sqlx::query(
            "UPDATE favorites SET content_key = ? \
             WHERE profile_id = ? AND channel_url = ? AND content_key = ?",
        )
        .bind(row.v2)
        .bind(profile_id)
        .bind(&channel_url)
        .bind(row.legacy)
        .execute(&mut **tx)
        .await
        .map_err(sql_err)?;
        if row.sync_used {
            sync::queue_favorite(tx, profile_id, row.legacy, &channel_url, false, row.now).await?;
            sync::queue_favorite(tx, profile_id, row.v2, &channel_url, active != 0, row.now)
                .await?;
        }
    }

    let history: Vec<(i64, i64, String, f64, f64, i64)> = if row.has_history {
        sqlx::query_as(
            "SELECT id, profile_id, path, position, duration, played_at FROM history \
             WHERE path = ? AND content_key = ?",
        )
        .bind(row.url)
        .bind(row.legacy)
        .fetch_all(&mut **tx)
        .await
        .map_err(sql_err)?
    } else {
        Vec::new()
    };
    for (id, profile_id, path, position, duration, played_at) in history {
        sqlx::query("UPDATE history SET content_key = ? WHERE id = ?")
            .bind(row.v2)
            .bind(id)
            .execute(&mut **tx)
            .await
            .map_err(sql_err)?;
        if row.sync_used {
            sync::queue_progress_deleted(tx, profile_id, row.legacy, row.now).await?;
            sync::queue_progress(tx, profile_id, row.v2, &path, position, duration, played_at)
                .await?;
        }
    }

    if row.has_title_sources {
        // `title_sources.content_key` is a foreign key into `titles`, so the v2 title must exist
        // before the source can point at it. The rebuild later fills in its metadata.
        ensure_title(tx, row.v2, row.name, row.now).await?;
        sqlx::query(
            "UPDATE title_sources SET content_key = ? WHERE channel_id = ? AND content_key = ?",
        )
        .bind(row.v2)
        .bind(channel_id)
        .bind(row.legacy)
        .execute(&mut **tx)
        .await
        .map_err(sql_err)?;
    }
    Ok(())
}

async fn ensure_title(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    content_key: &str,
    title: &str,
    now: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO titles (content_key, kind, title, created_at, updated_at) \
         VALUES (?, 'channel', ?, ?, ?) ON CONFLICT(content_key) DO NOTHING",
    )
    .bind(content_key)
    .bind(title)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(sql_err)?;
    Ok(())
}

async fn rekey_orphan_favorites(pool: &Pool<Sqlite>, batch: i64, queue_sync: bool) -> Result<()> {
    let mut cursor_profile = 0i64;
    let mut cursor_url = String::new();
    loop {
        let rows: Vec<(i64, String, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT profile_id, channel_url, active, content_key FROM favorites \
             WHERE {LEGACY_KEY_FILTER} \
               AND (profile_id > ? OR (profile_id = ? AND channel_url > ?)) \
             ORDER BY profile_id, channel_url LIMIT ?"
        )))
        .bind(cursor_profile)
        .bind(cursor_profile)
        .bind(&cursor_url)
        .bind(batch)
        .fetch_all(pool)
        .await
        .map_err(sql_err)?;
        if rows.is_empty() {
            break;
        }
        let now = Utc::now().timestamp_millis();
        let mut tx = pool.begin().await.map_err(sql_err)?;
        for (profile_id, url, active, legacy) in rows {
            cursor_profile = profile_id;
            cursor_url.clone_from(&url);
            let v2 = ContentKey::derive(None, None, "", None, &url).into_string();
            if v2 == legacy {
                continue;
            }
            sqlx::query(
                "UPDATE favorites SET content_key = ? \
                 WHERE profile_id = ? AND channel_url = ? AND content_key = ?",
            )
            .bind(&v2)
            .bind(profile_id)
            .bind(&url)
            .bind(&legacy)
            .execute(&mut *tx)
            .await
            .map_err(sql_err)?;
            if queue_sync {
                sync::queue_favorite(&mut tx, profile_id, &legacy, &url, false, now).await?;
                sync::queue_favorite(&mut tx, profile_id, &v2, &url, active != 0, now).await?;
            }
        }
        tx.commit().await.map_err(sql_err)?;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    Ok(())
}

async fn rekey_orphan_history(pool: &Pool<Sqlite>, batch: i64, queue_sync: bool) -> Result<()> {
    let mut cursor = 0i64;
    loop {
        let rows: Vec<(i64, i64, String, f64, f64, i64, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT id, profile_id, path, position, duration, played_at, content_key \
             FROM history WHERE id > ? AND {LEGACY_KEY_FILTER} ORDER BY id LIMIT ?"
            )))
            .bind(cursor)
            .bind(batch)
            .fetch_all(pool)
            .await
            .map_err(sql_err)?;
        if rows.is_empty() {
            break;
        }
        let now = Utc::now().timestamp_millis();
        let mut tx = pool.begin().await.map_err(sql_err)?;
        for (id, profile_id, path, position, duration, played_at, legacy) in rows {
            cursor = id;
            let v2 = ContentKey::derive(None, None, "", None, &path).into_string();
            if v2 == legacy {
                continue;
            }
            sqlx::query("UPDATE history SET content_key = ? WHERE id = ?")
                .bind(&v2)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(sql_err)?;
            if queue_sync {
                sync::queue_progress_deleted(&mut tx, profile_id, &legacy, now).await?;
                sync::queue_progress(
                    &mut tx, profile_id, &v2, &path, position, duration, played_at,
                )
                .await?;
            }
        }
        tx.commit().await.map_err(sql_err)?;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    Ok(())
}

async fn rekey_orphan_title_sources(pool: &Pool<Sqlite>, batch: i64) -> Result<()> {
    let mut cursor = 0i64;
    loop {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT ts.id, ts.content_key, ts.stream_url, ts.channel_id, \
                    c.name AS channel_name, c.tvg_id, c.tmdb_id, c.tmdb_type \
             FROM title_sources ts LEFT JOIN channels c ON c.id = ts.channel_id \
             WHERE ts.id > ? AND {LEGACY_KEY_FILTER} ORDER BY ts.id LIMIT ?"
        )))
        .bind(cursor)
        .bind(batch)
        .fetch_all(pool)
        .await
        .map_err(sql_err)?;
        if rows.is_empty() {
            break;
        }
        let mut tx = pool.begin().await.map_err(sql_err)?;
        for row in rows {
            let id: i64 = row.get("id");
            cursor = id;
            let legacy: String = row.get("content_key");
            let stream_url: String = row.get("stream_url");
            let channel_name: Option<String> = row.get("channel_name");
            let tvg_id: Option<String> = row.get("tvg_id");
            let tmdb_id: Option<i64> = row.get("tmdb_id");
            let tmdb_type: Option<String> = row.get("tmdb_type");
            let v2 = ContentKey::derive(
                tmdb_id,
                tmdb_type.as_deref(),
                channel_name.as_deref().unwrap_or(""),
                tvg_id.as_deref(),
                &stream_url,
            )
            .into_string();
            if v2 == legacy {
                continue;
            }
            ensure_title(
                &mut tx,
                &v2,
                channel_name.as_deref().unwrap_or(&stream_url),
                Utc::now().timestamp_millis(),
            )
            .await?;
            sqlx::query("UPDATE title_sources SET content_key = ? WHERE id = ?")
                .bind(&v2)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(sql_err)?;
        }
        tx.commit().await.map_err(sql_err)?;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    Ok(())
}

/// Rebuilds `titles` from the now-correct `title_sources` one page at a time. Unlike the plan's
/// first sketch this does not `DELETE FROM titles` first: `title_sources` cascades from it, and
/// that would throw away ranks/quality/headers. Upserts are safe and stale legacy rows are removed
/// afterwards only when nothing references them.
async fn rebuild_titles(pool: &Pool<Sqlite>, batch: i64) -> Result<()> {
    // With no sources there is nothing whose key changed, and rebuilding 14.76M title rows here
    // would be an hours-long, multi-gigabyte backfill unrelated to this migration. The paged
    // `backfill_titles_and_sources` startup pass now uses a new settings key, so it rebuilds
    // those rows after this migration instead of skipping an old `v1 = done` marker.
    if count(pool, "title_sources").await? == 0 {
        // Do NOT write the completion marker: the paged startup backfill must still run and
        // populate `titles` from `channels`. Writing it here was what kept old databases empty.
        return Ok(());
    }
    let mut cursor = 0i64;
    loop {
        let channels = sqlx::query(
            "SELECT id, name, url, group_title, tvg_id, tvg_logo, extra_headers, is_live, \
             tmdb_id, tmdb_type, poster_path, backdrop_path, overview, release_year, genre_names \
             FROM channels WHERE id > ? ORDER BY id LIMIT ?",
        )
        .bind(cursor)
        .bind(batch)
        .fetch_all(pool)
        .await
        .map_err(sql_err)?;
        if channels.is_empty() {
            break;
        }
        cursor = channels
            .last()
            .map(|row| row.get::<i64, _>("id"))
            .unwrap_or(cursor);
        let prepared: Vec<TitleBackfillRow> =
            channels.iter().map(title_backfill_row_from).collect();
        let now = Utc::now().timestamp_millis();
        let mut tx = pool.begin().await.map_err(sql_err)?;
        batch_upsert_titles_and_sources(&mut tx, &prepared, now).await?;
        tx.commit().await.map_err(sql_err)?;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM titles WHERE {LEGACY_KEY_FILTER} \
         AND NOT EXISTS (SELECT 1 FROM title_sources ts WHERE ts.content_key = titles.content_key)"
    )))
    .execute(pool)
    .await
    .map_err(sql_err)?;
    crate::db::set_setting(pool, TITLES_BACKFILL_SETTING, "done").await?;
    Ok(())
}

async fn verify(pool: &Pool<Sqlite>) -> Result<()> {
    for table in ["favorites", "history", "title_sources"] {
        let bad: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {table} WHERE content_key IS NULL OR content_key = '' \
             OR {LEGACY_KEY_FILTER}"
        )))
        .fetch_one(pool)
        .await
        .map_err(sql_err)?;
        if bad != 0 {
            return Err(PlayerError::Sql(format!(
                "content-key v2 verification failed: {bad} legacy key(s) remain in {table}"
            )));
        }
    }
    let check: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(pool)
        .await
        .map_err(sql_err)?;
    if check != "ok" {
        return Err(PlayerError::Sql(format!(
            "content-key v2 verification failed: quick_check returned {check}"
        )));
    }
    Ok(())
}

async fn finish(pool: &Pool<Sqlite>) -> Result<()> {
    let mut tx = pool.begin().await.map_err(sql_err)?;
    write_setting_tx(&mut tx, CONTENT_KEY_VERSION_SETTING, "2").await?;
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(CURSOR_SETTING)
        .execute(&mut *tx)
        .await
        .map_err(sql_err)?;
    tx.commit().await.map_err(sql_err)?;
    Ok(())
}

async fn ensure_backup(pool: &Pool<Sqlite>) -> Result<()> {
    if setting(pool, STARTED_SETTING).await?.is_some() {
        return Ok(());
    }
    let rows = sqlx::query("PRAGMA database_list")
        .fetch_all(pool)
        .await
        .map_err(sql_err)?;
    let file = rows.iter().find_map(|row| {
        let name: String = row.get("name");
        (name == "main")
            .then(|| row.get::<Option<String>, _>("file"))
            .flatten()
            .filter(|path| !path.is_empty())
    });
    let Some(file) = file else {
        // In-memory database: nothing to back up and nowhere to put it.
        return Ok(());
    };
    let backup = format!(
        "{file}.pre-key-v2-{}.db",
        Utc::now().format("%Y%m%d-%H%M%S")
    );
    let escaped = backup.replace('\'', "''");
    sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{escaped}'")))
        .execute(pool)
        .await
        .map_err(|e| {
            PlayerError::Sql(format!(
                "content-key v2 migration stopped: backup to {backup} failed ({e})"
            ))
        })?;
    tracing::info!(backup = %backup, "content-key v2 backup written");
    crate::db::set_setting(
        pool,
        STARTED_SETTING,
        &Utc::now().timestamp_millis().to_string(),
    )
    .await?;
    Ok(())
}

async fn count(pool: &Pool<Sqlite>, table: &str) -> Result<i64> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(pool)
        .await
        .map_err(sql_err)
}

async fn setting(pool: &Pool<Sqlite>, key: &str) -> Result<Option<String>> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(sql_err)
}

async fn write_setting_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    key: &str,
    value: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(&mut **tx)
    .await
    .map_err(sql_err)?;
    Ok(())
}

fn sql_err(error: sqlx::Error) -> PlayerError {
    PlayerError::Sql(error.to_string())
}
