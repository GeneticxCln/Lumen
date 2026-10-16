//! Cross-device sync, end-to-end encrypted.
//!
//! Profiles, My List and watch progress are queued locally as they change, sealed with the
//! group's sync key (`sync_crypto`) and pushed to the sync Worker, which stores only opaque
//! blobs. Remote records are opened, then applied last-write-wins on `updated_at`. Applying a
//! remote record writes the local rows directly and never re-queues them, so two devices do
//! not echo each other's changes back and forth. Only http(s) streams sync: a local file path
//! means nothing on another device.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Pool, Row, Sqlite, SqliteConnection};
use tokio::sync::Mutex;

use crate::db::{clean_profile_name, get_setting, set_setting};
use crate::error::{PlayerError, Result};
use crate::sync_crypto::{SealedItem, SyncKeys};

const KIND_PROFILE: &str = "profile";
const KIND_FAVORITE: &str = "favorite";
const KIND_PROGRESS: &str = "progress";
const KIND_EPISODE: &str = "episode";

/// The server rejects a whole push if one sealed record is over its size cap, so oversized
/// records are not queued. Otherwise one long record would block every later change. 4,000 bytes
/// of payload seal to well under the server's 6,144-character blob limit.
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
const SKIPPED_SETTING: &str = "sync_skipped";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

static SYNC_LOCK: Mutex<()> = Mutex::const_new(());
static SCHEDULED: AtomicBool = AtomicBool::new(false);

/// How often playback progress, saved every few seconds while something plays, may start a
/// sync. Every save is still written to the local queue at once; only the network round trip is
/// spaced out. Stopping playback and every other kind of change sync immediately.
const PROGRESS_SYNC_INTERVAL_MS: i64 = 90_000;
static LAST_PROGRESS_SYNC_MS: AtomicI64 = AtomicI64::new(0);

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
    items: Vec<SealedItem>,
    has_more: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// Local changes not yet accepted by the server.
    pub pending: i64,
    /// Unix milliseconds of the last completed sync, if any.
    pub last_synced_at: Option<i64>,
    /// Records from the last pull that failed verification and were ignored. Non-zero means the
    /// server returned something this key did not write, or altered a record.
    pub skipped: i64,
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
pub async fn queue_profile_deleted(
    conn: &mut SqliteConnection,
    sync_key: &str,
    now: i64,
) -> Result<()> {
    let item = SyncItem {
        kind: KIND_PROFILE.into(),
        key: sync_key.into(),
        payload: json!({}),
        updated_at: now,
        deleted: true,
    };
    enqueue(conn, &item).await
}

/// Redacts user credentials and sensitive tokens from a stream URL before sync/storage.
pub fn sanitize_url(raw_url: &str) -> String {
    if let Ok(mut parsed) = url::Url::parse(raw_url) {
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
        // Strip access tokens or sensitive query keys while keeping clean paths
        let query_pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .filter(|(k, _)| {
                let lower = k.to_ascii_lowercase();
                !lower.contains("token")
                    && !lower.contains("auth")
                    && !lower.contains("secret")
                    && !lower.contains("key")
                    && !lower.contains("pass")
            })
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if query_pairs.is_empty() {
            parsed.set_query(None);
        } else {
            parsed.query_pairs_mut().clear().extend_pairs(query_pairs);
        }
        parsed.to_string()
    } else {
        raw_url.to_string()
    }
}

/// Queues a favorite or its tombstone. The key is content_key with stream fallback.
pub async fn queue_favorite(
    conn: &mut SqliteConnection,
    profile_id: i64,
    content_key: &str,
    url: &str,
    active: bool,
    now: i64,
) -> Result<()> {
    let Some(profile_key) = profile_sync_key(conn, profile_id).await? else {
        return Ok(());
    };
    let clean_url = sanitize_url(url);
    let item = SyncItem {
        kind: KIND_FAVORITE.into(),
        key: format!("{profile_key}:{}", hashed(content_key)),
        payload: json!({
            "profile": profile_key,
            "content_key": content_key,
            "url": clean_url,
        }),
        updated_at: now,
        deleted: !active,
    };
    enqueue(conn, &item).await
}

/// Queues the resume point for a title/stream.
pub async fn queue_progress(
    conn: &mut SqliteConnection,
    profile_id: i64,
    content_key: &str,
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
    let clean_url = sanitize_url(url);
    let item = SyncItem {
        kind: KIND_PROGRESS.into(),
        key: format!("{profile_key}:{}", hashed(content_key)),
        payload: json!({
            "profile": profile_key,
            "content_key": content_key,
            "url": clean_url,
            "position": position,
            "duration": duration,
            "played_at": played_at,
        }),
        updated_at: now_ms(),
        deleted: false,
    };
    enqueue(conn, &item).await
}

/// Queues a hand-set watched mark for one episode. `watched = false` is a real answer ("I have not
/// seen this"), so these are never tombstones.
pub async fn queue_episode(
    conn: &mut SqliteConnection,
    profile_id: i64,
    series_key: &str,
    season: i64,
    episode: i64,
    watched: bool,
    now: i64,
) -> Result<()> {
    let Some(profile_key) = profile_sync_key(conn, profile_id).await? else {
        return Ok(());
    };
    let item = SyncItem {
        kind: KIND_EPISODE.into(),
        key: format!("{profile_key}:{}:S{season}E{episode}", hashed(series_key)),
        payload: json!({
            "profile": profile_key,
            "series": series_key,
            "season": season,
            "episode": episode,
            "watched": watched,
        }),
        updated_at: now,
        deleted: false,
    };
    enqueue(conn, &item).await
}

async fn apply_episode(conn: &mut SqliteConnection, item: &SyncItem) -> Result<()> {
    let Some(profile_key) = item.payload.get("profile").and_then(Value::as_str) else {
        return Ok(());
    };
    let (Some(series), Some(season), Some(episode), Some(watched)) = (
        item.payload.get("series").and_then(Value::as_str),
        item.payload.get("season").and_then(Value::as_i64),
        item.payload.get("episode").and_then(Value::as_i64),
        item.payload.get("watched").and_then(Value::as_bool),
    ) else {
        return Ok(());
    };
    if series.trim().is_empty()
        || series.chars().count() > 200
        || !(0..=500).contains(&season)
        || !(0..=10_000).contains(&episode)
    {
        return Ok(());
    }
    let Some(profile_id) = profile_id_for_key(conn, profile_key).await? else {
        return Ok(());
    };
    // Applied directly, never re-queued: the other device already has it.
    sqlx::query(
        "INSERT INTO episode_state (profile_id, series_key, season, episode, watched, updated_at) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (profile_id, series_key, season, episode) DO UPDATE SET \
           watched = excluded.watched, updated_at = excluded.updated_at \
         WHERE excluded.updated_at > episode_state.updated_at",
    )
    .bind(profile_id)
    .bind(series)
    .bind(season)
    .bind(episode)
    .bind(i64::from(watched))
    .bind(item.updated_at)
    .execute(conn)
    .await
    .map_err(sql_err)?;
    Ok(())
}

async fn apply_remote(conn: &mut SqliteConnection, item: &SyncItem) -> Result<()> {
    match item.kind.as_str() {
        KIND_EPISODE => apply_episode(conn, item).await,
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

    let raw_name = item
        .payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
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
                sqlx::query(
                    "UPDATE profiles SET name = ?, color = ?, updated_at = ? WHERE sync_key = ?",
                )
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
    let content_key = item.payload.get("content_key").and_then(Value::as_str);
    let Some(profile_id) = profile_id_for_key(conn, profile_key).await? else {
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO favorites (profile_id, channel_url, content_key, active, updated_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (profile_id, channel_url) DO UPDATE SET
           content_key = COALESCE(excluded.content_key, favorites.content_key),
           active = excluded.active,
           updated_at = excluded.updated_at
         WHERE excluded.updated_at > favorites.updated_at",
    )
    .bind(profile_id)
    .bind(url)
    .bind(content_key)
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
    let content_key = item.payload.get("content_key").and_then(Value::as_str);
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
        "SELECT played_at, position FROM history WHERE profile_id = ? AND (path = ? OR (content_key IS NOT NULL AND content_key = ?))
         ORDER BY played_at DESC LIMIT 1",
    )
    .bind(profile_id)
    .bind(url)
    .bind(content_key)
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
    sqlx::query("DELETE FROM history WHERE profile_id = ? AND (path = ? OR (content_key IS NOT NULL AND content_key = ?))")
        .bind(profile_id)
        .bind(url)
        .bind(content_key)
        .execute(&mut *conn)
        .await
        .map_err(sql_err)?;
    sqlx::query(
        "INSERT INTO history (profile_id, channel_id, path, content_key, position, duration, played_at)
         VALUES (?, (SELECT id FROM channels WHERE url = ? LIMIT 1), ?, ?, ?, ?, ?)",
    )
    .bind(profile_id)
    .bind(url)
    .bind(url)
    .bind(content_key)
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
        queue_profile(&mut tx, id).await?;
    }

    let favorites = sqlx::query(
        "SELECT profile_id, channel_url, content_key, active, updated_at FROM favorites",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(sql_err)?;
    for row in favorites {
        let profile_id: i64 = row.get("profile_id");
        let url: String = row.get("channel_url");
        let content_key: Option<String> = row.get("content_key");
        let key = content_key.unwrap_or_else(|| url.clone());
        let active = row.get::<i64, _>("active") != 0;
        let updated_at: i64 = row.get("updated_at");
        queue_favorite(&mut tx, profile_id, &key, &url, active, updated_at).await?;
    }

    let history = sqlx::query(
        "SELECT profile_id, path, content_key, position, duration, played_at FROM history WHERE path LIKE 'http%'",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(sql_err)?;
    for row in history {
        let profile_id: i64 = row.get("profile_id");
        let path: String = row.get("path");
        let content_key: Option<String> = row.get("content_key");
        let key = content_key.unwrap_or_else(|| path.clone());
        queue_progress(
            &mut tx,
            profile_id,
            &key,
            &path,
            row.get("position"),
            row.get("duration"),
            row.get("played_at"),
        )
        .await?;
    }

    let marks = sqlx::query(
        "SELECT profile_id, series_key, season, episode, watched, updated_at FROM episode_state",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(sql_err)?;
    for row in marks {
        queue_episode(
            &mut tx,
            row.get("profile_id"),
            &row.get::<String, _>("series_key"),
            row.get("season"),
            row.get("episode"),
            row.get::<i64, _>("watched") != 0,
            row.get("updated_at"),
        )
        .await?;
    }

    tx.commit().await.map_err(sql_err)?;
    Ok(())
}

/// One sync group on one server: where to send requests and the keys that seal and open them.
pub(crate) struct Remote {
    client: reqwest::Client,
    origin: String,
    keys: SyncKeys,
}

impl Remote {
    pub(crate) fn new(origin: &str, keys: SyncKeys) -> Result<Self> {
        // Redirects are never followed: the bearer token must only ever go to the address the
        // user configured.
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            origin: origin.trim_end_matches('/').to_string(),
            keys,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.origin))
            .header("X-Sync-Id", self.keys.sync_id())
            .bearer_auth(self.keys.auth_token())
    }

    /// Registers this group on the server. Safe to repeat: the server answers "already exists"
    /// for the same key and refuses a different one. The proof of work takes about a second, so
    /// it runs off the async runtime.
    pub(crate) async fn register(&self, pow_bits: u32) -> Result<()> {
        let sync_id = self.keys.sync_id();
        let for_pow = sync_id.clone();
        let nonce =
            tokio::task::spawn_blocking(move || crate::sync_crypto::solve_pow(&for_pow, pow_bits))
                .await
                .map_err(|error| {
                    PlayerError::Io(format!("Could not prepare the sync group: {error}"))
                })?;
        let response = self
            .client
            .post(format!("{}/v1/groups", self.origin))
            .json(&json!({ "sync_id": sync_id, "verifier": self.keys.verifier(), "pow_nonce": nonce }))
            .send()
            .await?;
        check(response).await.map(|_| ())
    }

    /// Whether this key has a group on the server, without creating one.
    pub(crate) async fn exists(&self) -> Result<bool> {
        let response = self
            .request(reqwest::Method::GET, "/v1/items")
            .query(&[("since", 0)])
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(true);
        }
        let (status, code) = failure(response).await;
        if status == reqwest::StatusCode::NOT_FOUND && code.as_deref() == Some("group_not_found") {
            return Ok(false);
        }
        Err(describe(status, code.as_deref()))
    }

    /// Deletes the group and everything in it from the server.
    pub(crate) async fn delete_group(&self) -> Result<()> {
        let response = self
            .request(reqwest::Method::DELETE, "/v1/groups")
            .send()
            .await?;
        match check(response).await {
            Ok(_) => Ok(()),
            // Already gone is the state the caller wanted.
            Err(PlayerError::NotFound(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// The status and the server's `error` code of a failed response.
async fn failure(response: reqwest::Response) -> (reqwest::StatusCode, Option<String>) {
    let status = response.status();
    let code = response.json::<Value>().await.ok().and_then(|body| {
        body.get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    (status, code)
}

fn describe(status: reqwest::StatusCode, code: Option<&str>) -> PlayerError {
    use reqwest::StatusCode;
    match (status, code) {
        (StatusCode::UNAUTHORIZED, _) => PlayerError::Io(
            "The sync server no longer accepts this sync key. Turn sync off and join again with the right key".into(),
        ),
        (StatusCode::NOT_FOUND, Some("group_not_found")) => PlayerError::NotFound(
            "The synced data for this key was deleted or has expired. Turn sync off, then on again to start fresh".into(),
        ),
        (StatusCode::NOT_FOUND, _) => PlayerError::Io("That address is not a Lumen sync server".into()),
        (StatusCode::CONFLICT, _) => PlayerError::Io("That sync group already belongs to a different key".into()),
        (StatusCode::PAYLOAD_TOO_LARGE, _) => PlayerError::Io("The sync server is full for this group".into()),
        (StatusCode::TOO_MANY_REQUESTS, _) => {
            PlayerError::Io("The sync server is limiting new groups from this network. Try again tomorrow".into())
        }
        (StatusCode::BAD_REQUEST, Some("invalid_pow")) => {
            PlayerError::Io("The sync server wants a stronger proof of work than this version of Lumen provides. Update Lumen".into())
        }
        _ => PlayerError::Io(format!("Sync server returned {status}")),
    }
}

async fn check(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let (status, code) = failure(response).await;
    Err(describe(status, code.as_deref()))
}

async fn push_batch(remote: &Remote, items: &[SyncItem]) -> Result<()> {
    let sealed = items
        .iter()
        .map(|item| remote.keys.seal(item))
        .collect::<Result<Vec<_>>>()?;
    let response = remote
        .request(reqwest::Method::POST, "/v1/items")
        .json(&json!({ "items": sealed }))
        .send()
        .await?;
    check(response).await.map(|_| ())
}

async fn push_pending(pool: &Pool<Sqlite>, remote: &Remote) -> Result<()> {
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

        push_batch(remote, &items).await?;

        // Remove only what was sent. A newer edit to the same record has a later updated_at and
        // stays queued.
        for item in &items {
            sqlx::query(
                "DELETE FROM sync_queue WHERE kind = ? AND item_key = ? AND updated_at = ?",
            )
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

/// Where the next pull page starts. The server pages in `(updated_at, id)` order, so the last
/// row received names the boundary exactly. Using the timestamp alone would skip every row that
/// shares the boundary's `updated_at`, and a bulk migration gives many rows one timestamp.
#[derive(Debug, Clone, PartialEq)]
struct PullCursor {
    since: i64,
    after: Option<String>,
}

/// The cursor after `items`, or an error if a page claims more rows but carries none (the pull
/// would never advance). Taken from the sealed rows, so a row that fails verification still
/// moves the cursor past itself.
fn next_cursor(items: &[SealedItem]) -> Result<PullCursor> {
    let last = items
        .last()
        .ok_or_else(|| PlayerError::Io("Sync page reported more rows but returned none".into()))?;
    Ok(PullCursor {
        since: last.updated_at,
        after: Some(last.id.clone()),
    })
}

async fn pull_page(remote: &Remote, cursor: &PullCursor) -> Result<PullPage> {
    let mut request = remote
        .request(reqwest::Method::GET, "/v1/items")
        .query(&[("since", cursor.since)]);
    if let Some(id) = &cursor.after {
        request = request.query(&[("after_id", id.as_str())]);
    }
    let response = check(request.send().await?).await?;
    Ok(response.json::<PullPage>().await?)
}

/// Pulls and applies every record newer than the saved cursor. Returns how many records failed
/// verification: they are not applied, and the pull carries on past them.
async fn pull_remote(pool: &Pool<Sqlite>, remote: &Remote) -> Result<i64> {
    let saved: i64 = get_setting(pool, CURSOR_SETTING)
        .await?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut newest = saved;
    let mut skipped = 0;
    // Never below zero: the server rejects a negative `since`, which a first sync would otherwise send.
    let mut cursor = PullCursor {
        since: saved.saturating_sub(PULL_OVERLAP_MS).max(0),
        after: None,
    };

    loop {
        let page = pull_page(remote, &cursor).await?;

        let mut tx = pool.begin().await.map_err(sql_err)?;
        for sealed in &page.items {
            match remote.keys.open(sealed) {
                Ok(item) => apply_remote(&mut tx, &item).await?,
                Err(_) => skipped += 1,
            }
        }
        tx.commit().await.map_err(sql_err)?;

        if let Some(value) = page.items.iter().map(|item| item.updated_at).max() {
            newest = newest.max(value);
        }
        if !page.has_more {
            break;
        }
        cursor = next_cursor(&page.items)?;
    }

    set_setting(pool, CURSOR_SETTING, &newest.to_string()).await?;
    Ok(skipped)
}

pub async fn status(pool: &Pool<Sqlite>) -> Result<SyncStatus> {
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_queue")
        .fetch_one(pool)
        .await
        .map_err(sql_err)?;
    let last_synced_at = get_setting(pool, LAST_SYNC_SETTING)
        .await?
        .and_then(|value| value.parse().ok());
    let skipped = get_setting(pool, SKIPPED_SETTING)
        .await?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Ok(SyncStatus {
        pending,
        last_synced_at,
        skipped,
    })
}

/// Pushes queued changes, then pulls and applies remote ones. Does nothing while sync is off.
pub async fn sync_now(pool: &Pool<Sqlite>) -> Result<SyncStatus> {
    use crate::sync_keys::{KeyringStore, RootStore};
    // Without a usable keychain there cannot be a sync key, so there is nothing to sync and
    // nothing worth reporting every few minutes. (Turning sync on reports the problem.)
    if !matches!(KeyringStore.load(), Ok(Some(_))) {
        return status(pool).await;
    }
    let Some(remote) = crate::sync_keys::remote(pool, &KeyringStore).await? else {
        return status(pool).await;
    };
    sync_with(pool, &remote).await
}

pub(crate) async fn sync_with(pool: &Pool<Sqlite>, remote: &Remote) -> Result<SyncStatus> {
    let _guard = SYNC_LOCK.lock().await;

    if get_setting(pool, SEEDED_SETTING)
        .await?
        .filter(|value| !value.is_empty())
        .is_none()
    {
        queue_all(pool).await?;
        set_setting(pool, SEEDED_SETTING, "1").await?;
    }

    push_pending(pool, remote).await?;
    let skipped = pull_remote(pool, remote).await?;
    set_setting(pool, SKIPPED_SETTING, &skipped.to_string()).await?;
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

/// True when enough time has passed since `last` for a progress save to start a sync.
fn progress_sync_due(last: i64, now: i64) -> bool {
    // A clock that moved backwards (`now < last`) must not hold syncing off until it catches up.
    now < last || now - last >= PROGRESS_SYNC_INTERVAL_MS
}

/// Like `schedule`, for the steady stream of progress saves during playback: at most one sync
/// per `PROGRESS_SYNC_INTERVAL_MS`, instead of a push and a pull every few seconds.
pub fn schedule_progress(pool: &'static Pool<Sqlite>) {
    let now = now_ms();
    let last = LAST_PROGRESS_SYNC_MS.load(Ordering::SeqCst);
    if progress_sync_due(last, now)
        && LAST_PROGRESS_SYNC_MS
            .compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        schedule(pool);
    }
}

/// Syncs at startup and then every few minutes, so changes from another device arrive even
/// while this one is idle. Does nothing while sync is off.
pub fn start(pool: &'static Pool<Sqlite>) {
    tokio::spawn(async move {
        crate::sync_keys::clear_legacy_account_token();
        loop {
            if let Err(error) = sync_now(pool).await {
                eprintln!("lumen: sync failed: {}", String::from(error));
            }
            tokio::time::sleep(PERIODIC).await;
        }
    });
}

/// Drops this device's sync state when sync is turned off, so turning it on again (possibly with
/// another key) does not inherit queued changes or a pull position from the previous group.
pub async fn forget(pool: &Pool<Sqlite>) -> Result<()> {
    let _guard = SYNC_LOCK.lock().await;
    sqlx::query("DELETE FROM sync_queue")
        .execute(pool)
        .await
        .map_err(sql_err)?;
    set_setting(pool, CURSOR_SETTING, "0").await?;
    set_setting(pool, LAST_SYNC_SETTING, "").await?;
    set_setting(pool, SKIPPED_SETTING, "0").await?;
    set_setting(pool, SEEDED_SETTING, "").await
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    fn item(id: &str, updated_at: i64) -> SealedItem {
        SealedItem {
            id: id.into(),
            updated_at,
            deleted: false,
            blob: String::new(),
        }
    }

    #[test]
    fn the_cursor_names_the_last_row_not_just_its_timestamp() {
        // Every row shares one timestamp: a timestamp-only cursor could not tell them apart.
        let page = vec![item("a", 7), item("b", 7), item("c", 7)];
        let cursor = next_cursor(&page).unwrap();
        assert_eq!(cursor.since, 7);
        assert_eq!(cursor.after, Some("c".to_string()));
    }

    #[test]
    fn progress_saves_start_a_sync_at_most_once_per_interval() {
        assert!(
            progress_sync_due(0, PROGRESS_SYNC_INTERVAL_MS),
            "first save after startup"
        );
        assert!(
            !progress_sync_due(1_000_000, 1_000_000 + 5_000),
            "a save 5 s later waits"
        );
        assert!(!progress_sync_due(
            1_000_000,
            1_000_000 + PROGRESS_SYNC_INTERVAL_MS - 1
        ));
        assert!(progress_sync_due(
            1_000_000,
            1_000_000 + PROGRESS_SYNC_INTERVAL_MS
        ));
        assert!(
            progress_sync_due(2_000_000, 1_000_000),
            "a clock that moved backwards cannot wedge it"
        );
    }

    #[test]
    fn an_empty_page_that_claims_more_cannot_loop_forever() {
        assert!(next_cursor(&[]).is_err());
    }
}
