//! Cross-device sync for signed-in users.
//!
//! Profiles, My List and watch progress are queued locally as they change and pushed to the
//! account Worker. Remote records are applied last-write-wins on `updated_at`. Applying a
//! remote record writes the local rows directly and never re-queues them, so two devices do
//! not echo each other's changes back and forth. Only http(s) streams sync: a local file path
//! means nothing on another device.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Pool, Row, Sqlite, SqliteConnection};
use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::account;
use crate::commands::ensure_db;
use crate::db::{clean_profile_name, get_setting, set_setting};
use crate::error::{PlayerError, Result};

const KIND_PROFILE: &str = "profile";
const KIND_FAVORITE: &str = "favorite";
const KIND_PROGRESS: &str = "progress";

/// The server rejects a whole push if one payload is over 4 KB, so oversized records are not
/// queued. Otherwise one long record would block every later change.
const MAX_PAYLOAD_BYTES: usize = 4000;
const PUSH_BATCH: i64 = 500;
/// Pulls start this far before the saved cursor. A device with a slow clock can push a record
/// whose `updated_at` is older than another device's cursor; the overlap catches it. Applying
/// is idempotent, so re-reading a few minutes is harmless.
const PULL_OVERLAP_MS: i64 = 5 * 60 * 1000;
const DEBOUNCE: Duration = Duration::from_secs(3);
const PERIODIC: Duration = Duration::from_secs(5 * 60);

const CURSOR_SETTING: &str = "sync_pull_cursor";
const LAST_SYNC_SETTING: &str = "sync_last_synced_at";
const SEEDED_SETTING: &str = "sync_seeded";

static SYNC_LOCK: Mutex<()> = Mutex::const_new(());
static SCHEDULED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncItem {
    pub kind: String,
    pub key: String,
    pub payload: Value,
    pub updated_at: i64,
    pub deleted: bool,
}

#[derive(Deserialize)]
struct PullPage {
    items: Vec<SyncItem>,
    has_more: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// Local changes not yet accepted by the server.
    pub pending: i64,
    /// Unix milliseconds of the last completed sync, if any.
    pub last_synced_at: Option<i64>,
}

fn sql_err(error: sqlx::Error) -> PlayerError {
    PlayerError::Sql(error.to_string())
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn hashed(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn is_stream_url(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://")
}

async fn profile_sync_key(conn: &mut SqliteConnection, profile_id: i64) -> Result<Option<String>> {
    let row = sqlx::query("SELECT sync_key FROM profiles WHERE id = ?")
        .bind(profile_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql_err)?;
    Ok(row.and_then(|row| row.get::<Option<String>, _>("sync_key")))
}

async fn profile_id_for_key(conn: &mut SqliteConnection, sync_key: &str) -> Result<Option<i64>> {
    let row = sqlx::query("SELECT id FROM profiles WHERE sync_key = ?")
        .bind(sync_key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql_err)?;
    Ok(row.map(|row| row.get("id")))
}

/// Keeps only the latest state of each record: a newer local edit replaces an unsent older one.
async fn enqueue(conn: &mut SqliteConnection, item: &SyncItem) -> Result<()> {
    let payload = item.payload.to_string();
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO sync_queue (kind, item_key, payload, updated_at, deleted) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (kind, item_key) DO UPDATE SET
           payload = excluded.payload,
           updated_at = excluded.updated_at,
           deleted = excluded.deleted
         WHERE excluded.updated_at >= sync_queue.updated_at",
    )
    .bind(&item.kind)
    .bind(&item.key)
    .bind(payload)
    .bind(item.updated_at)
    .bind(i64::from(item.deleted))
    .execute(conn)
    .await
    .map_err(sql_err)?;
    Ok(())
}

/// Queues a profile's name and colour. Its sync key is stable across devices.
pub async fn queue_profile(conn: &mut SqliteConnection, profile_id: i64) -> Result<()> {
    let row = sqlx::query("SELECT sync_key, name, color, updated_at FROM profiles WHERE id = ?")
        .bind(profile_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql_err)?;
    let Some(row) = row else { return Ok(()) };
    let Some(sync_key) = row.get::<Option<String>, _>("sync_key") else {
        return Ok(());
    };
    let item = SyncItem {
        kind: KIND_PROFILE.into(),
        key: sync_key,
        payload: json!({
            "name": row.get::<String, _>("name"),
            "color": row.get::<Option<String>, _>("color"),
        }),
        updated_at: row.get("updated_at"),
        deleted: false,
    };
    enqueue(conn, &item).await
}

/// Queues a profile deletion so other devices remove it too.
pub async fn queue_profile_deleted(conn: &mut SqliteConnection, sync_key: &str, now: i64) -> Result<()> {
    let item = SyncItem {
        kind: KIND_PROFILE.into(),
        key: sync_key.into(),
        payload: json!({}),
        updated_at: now,
        deleted: true,
    };
    enqueue(conn, &item).await
}

/// Queues a favorite or its tombstone. The key is the stream URL, not the local channel id.
pub async fn queue_favorite(
    conn: &mut SqliteConnection,
    profile_id: i64,
    url: &str,
    active: bool,
    now: i64,
) -> Result<()> {
    let Some(profile_key) = profile_sync_key(conn, profile_id).await? else {
        return Ok(());
    };
    let item = SyncItem {
        kind: KIND_FAVORITE.into(),
        key: format!("{profile_key}:{}", hashed(url)),
        payload: json!({ "profile": profile_key, "url": url }),
        updated_at: now,
        deleted: !active,
    };
    enqueue(conn, &item).await
}

/// Queues the resume point for a stream. `played_at` is in seconds, matching history rows.
/// A zero position is not a resume point: opening a stream records one before anything plays,
/// and syncing it would overwrite the resume point saved on another device.
pub async fn queue_progress(
    conn: &mut SqliteConnection,
    profile_id: i64,
    url: &str,
    position: f64,
    duration: f64,
    played_at: i64,
) -> Result<()> {
    if !is_stream_url(url) || position <= 0.0 {
        return Ok(());
    }
    let Some(profile_key) = profile_sync_key(conn, profile_id).await? else {
        return Ok(());
    };
    let item = SyncItem {
        kind: KIND_PROGRESS.into(),
        key: format!("{profile_key}:{}", hashed(url)),
        payload: json!({
            "profile": profile_key,
            "url": url,
            "position": position,
            "duration": duration,
            "played_at": played_at,
        }),
        updated_at: now_ms(),
        deleted: false,
    };
    enqueue(conn, &item).await
}

async fn apply_remote(conn: &mut SqliteConnection, item: &SyncItem) -> Result<()> {
    match item.kind.as_str() {
        KIND_PROFILE => apply_profile(conn, item).await,
        KIND_FAVORITE => apply_favorite(conn, item).await,
        KIND_PROGRESS => apply_progress(conn, item).await,
        _ => Ok(()),
    }
}

async fn apply_profile(conn: &mut SqliteConnection, item: &SyncItem) -> Result<()> {
    if item.deleted {
        // Never remove the last profile on this device: the app always needs one to land on.
        sqlx::query(
            "DELETE FROM profiles WHERE sync_key = ? AND updated_at < ?
               AND (SELECT COUNT(*) FROM profiles) > 1",
        )
        .bind(&item.key)
        .bind(item.updated_at)
        .execute(&mut *conn)
        .await
        .map_err(sql_err)?;
        return Ok(());
    }

    let raw_name = item.payload.get("name").and_then(Value::as_str).unwrap_or_default();
    let Ok(name) = clean_profile_name(raw_name) else {
        return Ok(());
    };
    let color = item.payload.get("color").and_then(Value::as_str);

    let local = sqlx::query("SELECT updated_at FROM profiles WHERE sync_key = ?")
        .bind(&item.key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql_err)?;

    match local {
        None => {
            let now = Utc::now().timestamp();
            sqlx::query(
                "INSERT INTO profiles (name, color, pin_hash, created_at, last_used_at, sync_key, updated_at)
                 VALUES (?, ?, NULL, ?, ?, ?, ?)",
            )
            .bind(&name)
            .bind(color)
            .bind(now)
            .bind(now)
            .bind(&item.key)
            .bind(item.updated_at)
            .execute(&mut *conn)
            .await
            .map_err(sql_err)?;
        }
        Some(row) => {
            if item.updated_at > row.get::<i64, _>("updated_at") {
                sqlx::query("UPDATE profiles SET name = ?, color = ?, updated_at = ? WHERE sync_key = ?")
                    .bind(&name)
                    .bind(color)
                    .bind(item.updated_at)
                    .bind(&item.key)
                    .execute(&mut *conn)
                    .await
                    .map_err(sql_err)?;
            }
        }
    }
    Ok(())
}

async fn apply_favorite(conn: &mut SqliteConnection, item: &SyncItem) -> Result<()> {
    let Some(profile_key) = item.payload.get("profile").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(url) = item.payload.get("url").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(profile_id) = profile_id_for_key(conn, profile_key).await? else {
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO favorites (profile_id, channel_url, active, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (profile_id, channel_url) DO UPDATE SET
           active = excluded.active,
           updated_at = excluded.updated_at
         WHERE excluded.updated_at > favorites.updated_at",
    )
    .bind(profile_id)
    .bind(url)
    .bind(i64::from(!item.deleted))
    .bind(item.updated_at)
    .execute(conn)
    .await
    .map_err(sql_err)?;
    Ok(())
}

async fn apply_progress(conn: &mut SqliteConnection, item: &SyncItem) -> Result<()> {
    let Some(profile_key) = item.payload.get("profile").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(url) = item.payload.get("url").and_then(Value::as_str) else {
        return Ok(());
    };
    let position = item.payload.get("position").and_then(Value::as_f64);
    let duration = item.payload.get("duration").and_then(Value::as_f64);
    let played_at = item.payload.get("played_at").and_then(Value::as_i64);
    let (Some(position), Some(duration), Some(played_at)) = (position, duration, played_at) else {
        return Ok(());
    };
    if !is_stream_url(url) {
        return Ok(());
    }
    let Some(profile_id) = profile_id_for_key(conn, profile_key).await? else {
        return Ok(());
    };

    if position <= 0.0 {
        return Ok(());
    }
    // A local row with no position is a placeholder from opening the stream, so it yields to a
    // remote resume point. Otherwise the newer play time wins.
    let local = sqlx::query(
        "SELECT played_at, position FROM history WHERE profile_id = ? AND path = ?
         ORDER BY played_at DESC LIMIT 1",
    )
    .bind(profile_id)
    .bind(url)
    .fetch_optional(&mut *conn)
    .await
    .map_err(sql_err)?;
    if let Some(local) = local {
        let local_played_at: i64 = local.get("played_at");
        let local_position: f64 = local.get("position");
        if local_position > 0.0 && played_at <= local_played_at {
            return Ok(());
        }
    }

    // One history row per stream, as add_history keeps it. A channel link is taken from any
    // local channel with the same URL, if one exists.
    sqlx::query("DELETE FROM history WHERE profile_id = ? AND path = ?")
        .bind(profile_id)
        .bind(url)
        .execute(&mut *conn)
        .await
        .map_err(sql_err)?;
    sqlx::query(
        "INSERT INTO history (profile_id, channel_id, path, position, duration, played_at)
         VALUES (?, (SELECT id FROM channels WHERE url = ? LIMIT 1), ?, ?, ?, ?)",
    )
    .bind(profile_id)
    .bind(url)
    .bind(url)
    .bind(position)
    .bind(duration)
    .bind(played_at)
    .execute(&mut *conn)
    .await
    .map_err(sql_err)?;
    Ok(())
}

/// Queues everything already on this device the first time it signs in, so existing profiles,
/// favorites and history reach the account.
async fn queue_all(pool: &Pool<Sqlite>) -> Result<()> {
    let mut tx = pool.begin().await.map_err(sql_err)?;

    let profile_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM profiles")
        .fetch_all(&mut *tx)
        .await
        .map_err(sql_err)?;
    for id in profile_ids {
        queue_profile(&mut *tx, id).await?;
    }

    let favorites = sqlx::query("SELECT profile_id, channel_url, active, updated_at FROM favorites")
        .fetch_all(&mut *tx)
        .await
        .map_err(sql_err)?;
    for row in favorites {
        let profile_id: i64 = row.get("profile_id");
        let url: String = row.get("channel_url");
        let active = row.get::<i64, _>("active") != 0;
        let updated_at: i64 = row.get("updated_at");
        queue_favorite(&mut *tx, profile_id, &url, active, updated_at).await?;
    }

    let history = sqlx::query(
        "SELECT profile_id, path, position, duration, played_at FROM history WHERE path LIKE 'http%'",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(sql_err)?;
    for row in history {
        let profile_id: i64 = row.get("profile_id");
        let path: String = row.get("path");
        queue_progress(
            &mut *tx,
            profile_id,
            &path,
            row.get("position"),
            row.get("duration"),
            row.get("played_at"),
        )
        .await?;
    }

    tx.commit().await.map_err(sql_err)?;
    Ok(())
}

fn check_status(status: reqwest::StatusCode) -> Result<()> {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(PlayerError::Io("Sign in again to keep syncing".into()));
    }
    if !status.is_success() {
        return Err(PlayerError::Io(format!("Sync server returned {status}")));
    }
    Ok(())
}

async fn push_batch(client: &reqwest::Client, origin: &str, token: &str, items: &[SyncItem]) -> Result<()> {
    let response = client
        .post(format!("{origin}/api/desktop/sync"))
        .bearer_auth(token)
        .json(&json!({ "items": items }))
        .send()
        .await?;
    check_status(response.status())
}

async fn push_pending(pool: &Pool<Sqlite>, client: &reqwest::Client, origin: &str, token: &str) -> Result<()> {
    loop {
        let rows = sqlx::query(
            "SELECT kind, item_key, payload, updated_at, deleted FROM sync_queue ORDER BY updated_at LIMIT ?",
        )
        .bind(PUSH_BATCH)
        .fetch_all(pool)
        .await
        .map_err(sql_err)?;
        if rows.is_empty() {
            return Ok(());
        }

        let mut items = Vec::with_capacity(rows.len());
        for row in &rows {
            let payload: String = row.get("payload");
            items.push(SyncItem {
                kind: row.get("kind"),
                key: row.get("item_key"),
                payload: serde_json::from_str(&payload)?,
                updated_at: row.get("updated_at"),
                deleted: row.get::<i64, _>("deleted") != 0,
            });
        }

        push_batch(client, origin, token, &items).await?;

        // Remove only what was sent. A newer edit to the same record has a later updated_at and
        // stays queued.
        for item in &items {
            sqlx::query("DELETE FROM sync_queue WHERE kind = ? AND item_key = ? AND updated_at = ?")
                .bind(&item.kind)
                .bind(&item.key)
                .bind(item.updated_at)
                .execute(pool)
                .await
                .map_err(sql_err)?;
        }

        if (rows.len() as i64) < PUSH_BATCH {
            return Ok(());
        }
    }
}

async fn pull_page(client: &reqwest::Client, origin: &str, token: &str, since: i64) -> Result<PullPage> {
    let response = client
        .get(format!("{origin}/api/desktop/sync"))
        .query(&[("since", since)])
        .bearer_auth(token)
        .send()
        .await?;
    check_status(response.status())?;
    Ok(response.json::<PullPage>().await?)
}

async fn pull_remote(pool: &Pool<Sqlite>, client: &reqwest::Client, origin: &str, token: &str) -> Result<()> {
    let saved: i64 = get_setting(pool, CURSOR_SETTING)
        .await?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut newest = saved;
    let mut since = saved.saturating_sub(PULL_OVERLAP_MS);

    loop {
        let page = pull_page(client, origin, token, since).await?;

        let mut tx = pool.begin().await.map_err(sql_err)?;
        for item in &page.items {
            apply_remote(&mut *tx, item).await?;
        }
        tx.commit().await.map_err(sql_err)?;

        let page_newest = page.items.iter().map(|item| item.updated_at).max();
        if let Some(value) = page_newest {
            newest = newest.max(value);
        }
        if !page.has_more {
            break;
        }
        // The server pages in updated_at order. A page that cannot advance would loop forever.
        let next = page_newest.unwrap_or(since);
        if next <= since {
            return Err(PlayerError::Io("Sync page could not advance".into()));
        }
        since = next;
    }

    set_setting(pool, CURSOR_SETTING, &newest.to_string()).await
}

async fn status(pool: &Pool<Sqlite>) -> Result<SyncStatus> {
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_queue")
        .fetch_one(pool)
        .await
        .map_err(sql_err)?;
    let last_synced_at = get_setting(pool, LAST_SYNC_SETTING)
        .await?
        .and_then(|value| value.parse().ok());
    Ok(SyncStatus { pending, last_synced_at })
}

/// Pushes queued changes, then pulls and applies remote ones. Does nothing when signed out.
pub async fn sync_now(pool: &Pool<Sqlite>) -> Result<SyncStatus> {
    let _guard = SYNC_LOCK.lock().await;
    let Some(token) = account::load_app_token()? else {
        return status(pool).await;
    };
    let origin = account::account_origin();
    let client = account::http_client()?;

    if get_setting(pool, SEEDED_SETTING).await?.filter(|value| !value.is_empty()).is_none() {
        queue_all(pool).await?;
        set_setting(pool, SEEDED_SETTING, "1").await?;
    }

    push_pending(pool, &client, &origin, &token).await?;
    pull_remote(pool, &client, &origin, &token).await?;
    set_setting(pool, LAST_SYNC_SETTING, &now_ms().to_string()).await?;
    status(pool).await
}

/// Runs a sync shortly after a local change. A burst of saves becomes one sync, and a sync
/// already waiting absorbs later changes.
pub fn schedule(pool: &'static Pool<Sqlite>) {
    if SCHEDULED.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(DEBOUNCE).await;
        SCHEDULED.store(false, Ordering::SeqCst);
        if let Err(error) = sync_now(pool).await {
            eprintln!("lumen: sync failed: {}", String::from(error));
        }
    });
}

/// Syncs at startup and then every few minutes, so changes from another device arrive even
/// while this one is idle. Does nothing while signed out.
pub fn start(pool: &'static Pool<Sqlite>) {
    tokio::spawn(async move {
        loop {
            if let Err(error) = sync_now(pool).await {
                eprintln!("lumen: sync failed: {}", String::from(error));
            }
            tokio::time::sleep(PERIODIC).await;
        }
    });
}

/// Drops this device's sync state when it signs out, so the next account to sign in does not
/// inherit queued changes from the previous one.
pub async fn forget(pool: &Pool<Sqlite>) -> Result<()> {
    let _guard = SYNC_LOCK.lock().await;
    sqlx::query("DELETE FROM sync_queue")
        .execute(pool)
        .await
        .map_err(sql_err)?;
    set_setting(pool, CURSOR_SETTING, "0").await?;
    set_setting(pool, LAST_SYNC_SETTING, "").await?;
    set_setting(pool, SEEDED_SETTING, "").await
}

#[tauri::command]
pub async fn sync_now_cmd(app: AppHandle) -> Result<SyncStatus> {
    let pool = ensure_db(&app).await?;
    sync_now(pool).await
}

#[tauri::command]
pub async fn sync_status_cmd(app: AppHandle) -> Result<SyncStatus> {
    let pool = ensure_db(&app).await?;
    status(pool).await
}
