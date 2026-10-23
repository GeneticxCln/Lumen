use crate::auth;
use crate::content_key::{CategoryOverride, ContentKey, parse_season_episode};
use crate::error::{PlayerError, Result};
use crate::pin_guard;
use crate::playlist::import::{Channel, Playlist, import_m3u_file};
use crate::sync;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, QueryBuilder, Row, Sqlite, SqliteConnection, sqlite::SqlitePoolOptions};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

static DB_POOL: OnceLock<Pool<Sqlite>> = OnceLock::new();

/// A cold start has no pool yet, and the UI reaches several commands at once, each of which
/// calls `ensure_db`. Without this lock they all pass the check below and run the schema work
/// concurrently, which fails on the second connection ("duplicate column name") and surfaces
/// as a broken library. Held only while the pool is created; every later call returns early.
static DB_INIT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Applies the schema. The one copy of the migrations lives in this crate, so the app and every
/// test (including the desktop crate's) build the same database.
pub async fn run_migrations(pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| PlayerError::Sql(format!("Migration failed: {}", e)))
}

pub async fn init_db(app_dir: &Path) -> Result<Pool<Sqlite>> {
    if let Some(existing) = DB_POOL.get() {
        return Ok(existing.clone());
    }

    // Re-checked under the lock: whoever waited for the first initialization returns that pool
    // instead of repeating the migrations and the additive column pass.
    let _init_guard = DB_INIT_LOCK.lock().await;
    if let Some(existing) = DB_POOL.get() {
        return Ok(existing.clone());
    }

    let db_path = app_dir.join("lumen.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

    // Pragmas set via a one-off query only take effect on whichever single connection ran it, not
    // on the other connections the pool opens — `after_connect` runs them on every connection
    // instead. `busy_timeout` makes a write wait for a few seconds when another connection is
    // mid-transaction rather than failing at once with "database is locked", which otherwise a
    // user can hit from two ordinary actions (e.g. favoriting a channel while sync is writing).
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("PRAGMA foreign_keys = ON;")
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("PRAGMA busy_timeout = 5000;")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&db_url)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    run_migrations(&pool).await?;

    // sqlx records a migration as applied the first time it runs, so a new column added
    // to the migration file would never reach an existing database. Apply the additive
    // TMDB columns idempotently on every startup instead.
    migrate_tmdb_columns(&pool).await?;

    // Repair rows orphaned by connections that deleted channels without `foreign_keys` enabled.
    sqlx::query(
        "UPDATE history SET channel_id = NULL WHERE channel_id IS NOT NULL AND channel_id NOT IN (SELECT id FROM channels)",
    )
    .execute(&pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    // Earlier builds appended one history row per progress save; compact anything a
    // previous version left behind.
    match prune_history(&pool, DEFAULT_HISTORY_RETENTION).await {
        Ok(deleted) if deleted > 0 => {
            eprintln!("lumen: pruned {} stale history row(s)", deleted);
        }
        Ok(_) => {}
        Err(e) => eprintln!("lumen: history compaction failed: {}", e),
    }

    let _ = DB_POOL.set(pool.clone());

    // Recovering language labels re-reads playlist files, so it runs after the pool is
    // published rather than holding the init lock while those files are parsed.
    let backfill_pool = pool.clone();
    tokio::spawn(async move {
        if let Err(e) = backfill_channel_languages(&backfill_pool).await {
            eprintln!("lumen: channel language backfill failed: {}", e);
        }
        if let Err(e) = backfill_titles_and_sources(&backfill_pool).await {
            eprintln!("lumen: titles and title_sources backfill failed: {}", e);
        }
    });

    Ok(pool)
}

pub fn get_pool() -> Option<&'static Pool<Sqlite>> {
    DB_POOL.get()
}

/// Settings key recording that the language labels of every local playlist were recovered.
const CHANNEL_LANGUAGE_BACKFILL_SETTING: &str = "channel_language_backfill_v1";

/// Channels imported before `language` was stored have no label, and the label exists only in
/// the playlist file. Re-read each local playlist that still has unlabeled channels and copy its
/// `tvg-language` onto them, matched by stream URL. Remote playlists are not fetched. The setting
/// is written only when every such file was read, so a missing file is retried on the next start.
pub async fn backfill_channel_languages(pool: &Pool<Sqlite>) -> Result<()> {
    if get_setting(pool, CHANNEL_LANGUAGE_BACKFILL_SETTING)
        .await?
        .as_deref()
        == Some("done")
    {
        return Ok(());
    }

    let playlists = sqlx::query(
        "SELECT DISTINCT p.id, p.origin FROM playlists p \
         JOIN channels c ON c.playlist_id = p.id \
         WHERE p.source = 'file' AND c.language IS NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let mut complete = true;
    for row in playlists {
        let playlist_id: i64 = row.get("id");
        let Some(path) = row.get::<Option<String>, _>("origin") else {
            continue;
        };

        let parsed = match import_m3u_file(&path, None).await {
            Ok((_, channels)) => channels,
            Err(e) => {
                complete = false;
                eprintln!(
                    "lumen: could not re-read {} for language labels: {}",
                    path, e
                );
                continue;
            }
        };

        let mut labels: HashMap<String, String> = HashMap::new();
        for channel in parsed {
            if let Some(language) = channel.language {
                labels.entry(channel.url).or_insert(language);
            }
        }
        if labels.is_empty() {
            continue;
        }

        // Match in memory by URL: one pass over the playlist's rows instead of a scan per channel.
        let unlabeled =
            sqlx::query("SELECT id, url FROM channels WHERE playlist_id = ? AND language IS NULL")
                .bind(playlist_id)
                .fetch_all(pool)
                .await
                .map_err(|e| PlayerError::Sql(e.to_string()))?;

        let mut tx = pool
            .begin()
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
        for row in unlabeled {
            let url: String = row.get("url");
            if let Some(language) = labels.get(&url) {
                sqlx::query("UPDATE channels SET language = ? WHERE id = ?")
                    .bind(language)
                    .bind(row.get::<i64, _>("id"))
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| PlayerError::Sql(e.to_string()))?;
            }
        }
        tx.commit()
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
    }

    if complete {
        set_setting(pool, CHANNEL_LANGUAGE_BACKFILL_SETTING, "done").await?;
    }
    Ok(())
}

/// Settings key recording that titles and title_sources were backfilled.
const TITLES_BACKFILL_SETTING: &str = "titles_backfill_v1";

/// Backfills `titles` and `title_sources` from existing `channels`, and populates `content_key`
/// on pre-existing `favorites` and `history` rows.
pub async fn backfill_titles_and_sources(pool: &Pool<Sqlite>) -> Result<()> {
    if get_setting(pool, TITLES_BACKFILL_SETTING).await?.as_deref() == Some("done") {
        return Ok(());
    }

    let channels = sqlx::query(
        "SELECT id, name, url, group_title, tvg_id, tvg_logo, extra_headers, is_live, \
         tmdb_id, tmdb_type, poster_path, backdrop_path, overview, release_year, genre_names \
         FROM channels",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let now = Utc::now().timestamp_millis();
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    for row in &channels {
        let channel_id: i64 = row.get("id");
        let name: String = row.get("name");
        let url: String = row.get("url");
        let tvg_id = row.get::<Option<String>, _>("tvg_id");
        let tmdb_id = row.get::<Option<i64>, _>("tmdb_id");
        let tmdb_type = row.get::<Option<String>, _>("tmdb_type");
        let poster_path = row.get::<Option<String>, _>("poster_path");
        let backdrop_path = row.get::<Option<String>, _>("backdrop_path");
        let overview = row.get::<Option<String>, _>("overview");
        let release_year = row.get::<Option<i64>, _>("release_year");
        let genre_names = row.get::<Option<String>, _>("genre_names");
        let extra_headers = row.get::<Option<String>, _>("extra_headers");
        let is_live = row.get::<Option<i64>, _>("is_live");

        let content_key = ContentKey::derive(
            tmdb_id,
            tmdb_type.as_deref(),
            &name,
            tvg_id.as_deref(),
            &url,
        )
        .into_string();

        let (kind, season, ep) = if let Some(ref t) = tmdb_type {
            if t == "tv" || t == "series" {
                let (s, e) = parse_season_episode(&name).unwrap_or((1, 1));
                ("series", Some(s), Some(e))
            } else {
                ("movie", None, None)
            }
        } else if is_live == Some(1) {
            ("channel", None, None)
        } else {
            ("movie", None, None)
        };

        sqlx::query(
            "INSERT INTO titles (content_key, kind, title, tmdb_id, tmdb_type, season, episode, release_year, poster_path, backdrop_path, overview, genres, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (content_key) DO UPDATE SET \
               title = excluded.title, \
               poster_path = COALESCE(titles.poster_path, excluded.poster_path), \
               backdrop_path = COALESCE(titles.backdrop_path, excluded.backdrop_path), \
               overview = COALESCE(titles.overview, excluded.overview), \
               updated_at = excluded.updated_at",
        )
        .bind(&content_key)
        .bind(kind)
        .bind(&name)
        .bind(tmdb_id)
        .bind(&tmdb_type)
        .bind(season)
        .bind(ep)
        .bind(release_year)
        .bind(&poster_path)
        .bind(&backdrop_path)
        .bind(&overview)
        .bind(&genre_names)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

        sqlx::query(
            "INSERT INTO title_sources (content_key, channel_id, stream_url, rank, headers, is_live, created_at) \
             VALUES (?, ?, ?, 0, ?, ?, ?) \
             ON CONFLICT (content_key, stream_url) DO UPDATE SET \
               channel_id = excluded.channel_id, \
               headers = excluded.headers, \
               is_live = excluded.is_live",
        )
        .bind(&content_key)
        .bind(channel_id)
        .bind(&url)
        .bind(extra_headers.unwrap_or_else(|| "{}".into()))
        .bind(is_live)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

        // Backfill content_key into favorites matching this url
        sqlx::query(
            "UPDATE favorites SET content_key = ? WHERE channel_url = ? AND content_key IS NULL",
        )
        .bind(&content_key)
        .bind(&url)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

        // Backfill content_key into history matching this url or channel_id
        sqlx::query("UPDATE history SET content_key = ? WHERE (path = ? OR channel_id = ?) AND content_key IS NULL")
            .bind(&content_key)
            .bind(&url)
            .bind(channel_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
    }

    // For any remaining history or favorites rows, generate a stream/file fallback content_key
    // SQLite doesn't have sha256 built-in, so derive using URL prefix or rowid fallback
    sqlx::query(
        "UPDATE favorites SET content_key = 'stream:' || hex(channel_url) \
         WHERE content_key IS NULL",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    sqlx::query(
        "UPDATE history SET content_key = CASE \
           WHEN path LIKE 'http%' THEN 'stream:' || hex(path) \
           ELSE 'file:' || hex(path) \
         END \
         WHERE content_key IS NULL",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    set_setting(pool, TITLES_BACKFILL_SETTING, "done").await?;
    Ok(())
}

/// Columns added after the first release, in the order they were introduced. Each is
/// applied only when absent, so this is safe to call on every startup.
const TMDB_COLUMNS: [(&str, &str); 9] = [
    ("tmdb_id", "INTEGER"),
    ("tmdb_type", "TEXT"),
    ("poster_path", "TEXT"),
    ("backdrop_path", "TEXT"),
    ("overview", "TEXT"),
    ("release_year", "INTEGER"),
    ("rating", "REAL"),
    ("genre_names", "TEXT"),
    ("enriched_at", "INTEGER"),
];

pub async fn migrate_tmdb_columns(pool: &Pool<Sqlite>) -> Result<()> {
    let existing: Vec<String> = sqlx::query("PRAGMA table_info(channels)")
        .fetch_all(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect();

    for (name, kind) in TMDB_COLUMNS {
        if !existing.iter().any(|column| column == name) {
            // `name`/`kind` come from the fixed TMDB_COLUMNS constant, never from user input.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE channels ADD COLUMN {} {}",
                name, kind
            )))
            .execute(pool)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
        }
    }

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_channels_tmdb_id ON channels(tmdb_id)")
        .execute(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(())
}

/// Create a playlist together with its channels in one transaction, so a failed import
/// cannot leave an empty playlist or a half-inserted channel list behind.
/// Returns the new playlist id.
pub async fn import_playlist(
    pool: &Pool<Sqlite>,
    playlist: &Playlist,
    channels: &[Channel],
) -> Result<i64> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let row = sqlx::query(
        r#"
        INSERT INTO playlists (name, source, origin, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?)
        RETURNING id
        "#,
    )
    .bind(&playlist.name)
    .bind(&playlist.source)
    .bind(&playlist.origin)
    .bind(playlist.created_at)
    .bind(playlist.updated_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let playlist_id: i64 = row.get("id");
    insert_channel_rows(&mut tx, playlist_id, channels).await?;

    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(playlist_id)
}

/// Save a discovered source only when it produced playable entries. Ordinary user imports
/// still use `import_playlist` directly, where an empty playlist can be a valid result.
pub async fn import_playlist_if_nonempty(
    pool: &Pool<Sqlite>,
    playlist: &Playlist,
    channels: &[Channel],
) -> Result<Option<i64>> {
    if channels.is_empty() {
        return Ok(None);
    }
    import_playlist(pool, playlist, channels).await.map(Some)
}

pub async fn delete_playlist(pool: &Pool<Sqlite>, playlist_id: i64) -> Result<()> {
    let now = Utc::now().timestamp_millis();
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    // Favorites are keyed by URL and outlive channel rows. A favorite whose stream only this
    // playlist carried can no longer be reached, so it is tombstoned and the removal syncs.
    let orphaned: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT f.profile_id, f.channel_url, f.content_key FROM favorites f
         WHERE f.active = 1
           AND f.channel_url IN (SELECT url FROM channels WHERE playlist_id = ?)
           AND NOT EXISTS (
             SELECT 1 FROM channels c WHERE c.url = f.channel_url AND c.playlist_id <> ?
           )",
    )
    .bind(playlist_id)
    .bind(playlist_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(playlist_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    for (profile_id, url, stored_content_key) in orphaned {
        // Use the content_key already stored on the favorite, the same one `sync::queue_favorite`
        // used when the favorite was created: the channel row (and its tvg_id/TMDB match) is
        // already gone at this point, so re-deriving it would silently fall back to a different
        // key and the tombstone would miss the original synced record entirely.
        let content_key = match stored_content_key {
            Some(key) => key,
            None => resolve_content_key(pool, None, &url).await,
        };
        sqlx::query(
            "UPDATE favorites SET active = 0, updated_at = ? WHERE profile_id = ? AND channel_url = ?",
        )
        .bind(now)
        .bind(profile_id)
        .bind(&url)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
        sync::queue_favorite(&mut tx, profile_id, &content_key, &url, false, now).await?;
    }

    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(())
}

async fn insert_channel_rows(
    conn: &mut SqliteConnection,
    playlist_id: i64,
    channels: &[Channel],
) -> Result<()> {
    if channels.is_empty() {
        return Ok(());
    }

    // Batch insert up to 100 rows per statement to stay well below SQLite's 999/32766 bind parameter limits
    for chunk in channels.chunks(100) {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "INSERT INTO channels (playlist_id, name, url, group_title, tvg_id, tvg_logo, language, extra_headers, last_ok, last_error, is_live) ",
        );
        qb.push_values(chunk, |mut b, channel| {
            let headers_json = channel.extra_headers.as_deref().unwrap_or("{}");
            b.push_bind(playlist_id)
                .push_bind(&channel.name)
                .push_bind(&channel.url)
                .push_bind(&channel.group_title)
                .push_bind(&channel.tvg_id)
                .push_bind(&channel.tvg_logo)
                .push_bind(&channel.language)
                .push_bind(headers_json)
                .push_bind(channel.last_ok)
                .push_bind(&channel.last_error)
                .push_bind(channel.is_live.map(i64::from));
        });
        qb.build()
            .execute(&mut *conn)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
    }

    Ok(())
}

pub async fn list_playlists(pool: &Pool<Sqlite>) -> Result<Vec<Playlist>> {
    let rows = sqlx::query(
        "SELECT id, name, source, origin, created_at, updated_at FROM playlists ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let playlists = rows
        .into_iter()
        .map(|row| Playlist {
            id: Some(row.get("id")),
            name: row.get("name"),
            source: row.get("source"),
            origin: row.get("origin"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Ok(playlists)
}

/// A channel row plus its TMDB enrichment, flattened so the frontend receives one object
/// per title exactly as it does today, with the metadata fields simply added.
#[derive(Debug, Serialize)]
pub struct EnrichedChannel {
    #[serde(flatten)]
    pub channel: Channel,
    pub tmdb_id: Option<i64>,
    pub tmdb_type: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub overview: Option<String>,
    pub release_year: Option<i64>,
    pub rating: Option<f64>,
    pub genre_names: Option<String>,
    pub enriched_at: Option<i64>,
    /// A user's manual Kind/Genre correction, merged in only when `tmdb_id` is `None` — TMDB
    /// stays authoritative whenever it has actually matched this title.
    pub category_override_kind: Option<String>,
    pub category_override_genres: Option<String>,
}

/// The single channel-listing query, used by both the command layer and the tests.
/// Returning the TMDB columns here keeps one implementation of the query/group
/// filtering instead of a production copy and a test-only copy that could drift.
/// A `LIKE` pattern matching `needle` anywhere, with `%`, `_` and `\` taken literally. Pair it
/// with `ESCAPE '\'` in the SQL. Without this a search for "100%" or "a_b" matches far more.
pub fn contains_pattern(needle: &str) -> String {
    let mut pattern = String::with_capacity(needle.len() + 2);
    pattern.push('%');
    for ch in needle.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

pub async fn list_channels(
    pool: &Pool<Sqlite>,
    playlist_id: i64,
    query: Option<&str>,
    group: Option<&str>,
) -> Result<Vec<EnrichedChannel>> {
    let mut sql = String::from(
        "SELECT id, playlist_id, name, url, group_title, tvg_id, tvg_logo, language, extra_headers, last_ok, last_error, is_live, \
         tmdb_id, tmdb_type, poster_path, backdrop_path, overview, release_year, rating, genre_names, enriched_at \
         FROM channels WHERE playlist_id = ?",
    );

    if query.is_some() {
        sql.push_str(" AND name LIKE ? ESCAPE '\\'");
    }

    if group.is_some() {
        sql.push_str(" AND group_title = ?");
    }

    sql.push_str(" ORDER BY group_title, name");

    // `sql` is built only from literal fragments; user values are bound below.
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(playlist_id);

    if let Some(q_str) = query {
        q = q.bind(contains_pattern(q_str));
    }

    if let Some(g) = group {
        q = q.bind(g);
    }

    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    // A content_key per row that has no TMDB match, so its override (if any) can be looked up;
    // `None` for a TMDB-matched row, since TMDB stays authoritative and any leftover override
    // row must be ignored.
    let content_keys: Vec<Option<String>> = rows
        .iter()
        .map(|row| {
            let tmdb_id: Option<i64> = row.get("tmdb_id");
            if tmdb_id.is_some() {
                return None;
            }
            let name: String = row.get("name");
            let url: String = row.get("url");
            let tvg_id: Option<String> = row.get("tvg_id");
            Some(ContentKey::derive(None, None, &name, tvg_id.as_deref(), &url).into_string())
        })
        .collect();

    let overrides =
        fetch_category_overrides(pool, content_keys.iter().filter_map(|key| key.as_deref()))
            .await?;

    let channels = rows
        .into_iter()
        .zip(content_keys)
        .map(|(row, content_key)| {
            let override_row = content_key.as_deref().and_then(|key| overrides.get(key));
            EnrichedChannel {
                channel: Channel {
                    id: Some(row.get("id")),
                    playlist_id: row.get("playlist_id"),
                    name: row.get("name"),
                    url: row.get("url"),
                    group_title: row.get("group_title"),
                    tvg_id: row.get("tvg_id"),
                    tvg_logo: row.get("tvg_logo"),
                    language: row.get("language"),
                    extra_headers: row.get("extra_headers"),
                    last_ok: row.get("last_ok"),
                    last_error: row.get("last_error"),
                    is_live: row.get::<Option<i64>, _>("is_live").map(|value| value != 0),
                },
                tmdb_id: row.get("tmdb_id"),
                tmdb_type: row.get("tmdb_type"),
                poster_path: row.get("poster_path"),
                backdrop_path: row.get("backdrop_path"),
                overview: row.get("overview"),
                release_year: row.get("release_year"),
                rating: row.get("rating"),
                genre_names: row.get("genre_names"),
                enriched_at: row.get("enriched_at"),
                category_override_kind: override_row.and_then(|o| o.kind.clone()),
                category_override_genres: override_row.and_then(|o| o.genres.clone()),
            }
        })
        .collect();

    Ok(channels)
}

/// Batch-fetches overrides for exactly the given content keys, so listing channels never issues
/// one query per row.
async fn fetch_category_overrides<'a>(
    pool: &Pool<Sqlite>,
    keys: impl Iterator<Item = &'a str>,
) -> Result<HashMap<String, CategoryOverride>> {
    // Dedupe first: channels sharing a tvg_id (and so one content_key) would otherwise each
    // count separately toward the per-chunk bind limit below.
    let keys: Vec<&str> = keys
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let mut overrides = HashMap::new();
    // Chunked at 100, the same limit `insert_channel_rows` uses, to stay well below SQLite's
    // 999/32766 bind parameter limits: an aggregated IPTV playlist can carry tens of thousands
    // of unmatched channels, and one query for all of them can exceed the limit outright.
    for chunk in keys.chunks(100) {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT content_key, kind, genres FROM category_overrides WHERE content_key IN (",
        );
        {
            let mut separated = qb.separated(", ");
            for key in chunk {
                separated.push_bind(*key);
            }
        }
        qb.push(")");
        let rows = qb
            .build()
            .fetch_all(pool)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
        overrides.extend(rows.into_iter().map(|row| {
            let content_key: String = row.get("content_key");
            (
                content_key.clone(),
                CategoryOverride {
                    content_key,
                    kind: row.get("kind"),
                    genres: row.get("genres"),
                },
            )
        }));
    }
    Ok(overrides)
}

pub async fn get_channel(pool: &Pool<Sqlite>, channel_id: i64) -> Result<Channel> {
    let row = sqlx::query(
        "SELECT id, playlist_id, name, url, group_title, tvg_id, tvg_logo, language, extra_headers, last_ok, last_error, is_live FROM channels WHERE id = ?",
    )
    .bind(channel_id)
    .fetch_one(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(Channel {
        id: Some(row.get("id")),
        playlist_id: row.get("playlist_id"),
        name: row.get("name"),
        url: row.get("url"),
        group_title: row.get("group_title"),
        tvg_id: row.get("tvg_id"),
        tvg_logo: row.get("tvg_logo"),
        language: row.get("language"),
        extra_headers: row.get("extra_headers"),
        last_ok: row.get("last_ok"),
        last_error: row.get("last_error"),
        is_live: row.get::<Option<i64>, _>("is_live").map(|value| value != 0),
    })
}

pub async fn update_channel_last_played(
    pool: &Pool<Sqlite>,
    channel_id: i64,
    ok: bool,
    error: Option<String>,
) -> Result<()> {
    let now = Utc::now().timestamp();
    if ok {
        sqlx::query("UPDATE channels SET last_ok = ?, last_error = NULL WHERE id = ?")
            .bind(now)
            .bind(channel_id)
            .execute(pool)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
    } else {
        sqlx::query("UPDATE channels SET last_ok = NULL, last_error = ? WHERE id = ?")
            .bind(error.unwrap_or_else(|| "Probe failed".to_string()))
            .bind(channel_id)
            .execute(pool)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
    }
    Ok(())
}

pub async fn channel_exists(pool: &Pool<Sqlite>, channel_id: i64) -> Result<bool> {
    let row = sqlx::query("SELECT 1 AS present FROM channels WHERE id = ?")
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(row.is_some())
}

/// Resolves the canonical content key for a channel, falling back to a derived stream URL key.
pub async fn resolve_content_key(
    pool: &Pool<Sqlite>,
    channel_id: Option<i64>,
    path: &str,
) -> String {
    if let Some(cid) = channel_id {
        let row = sqlx::query(
            "SELECT name, url, group_title, tvg_id, tmdb_id, tmdb_type FROM channels WHERE id = ?",
        )
        .bind(cid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        if let Some(r) = row {
            let name: String = r.get("name");
            let url: String = r.get("url");
            let tvg_id = r.get::<Option<String>, _>("tvg_id");
            let tmdb_id = r.get::<Option<i64>, _>("tmdb_id");
            let tmdb_type = r.get::<Option<String>, _>("tmdb_type");
            return ContentKey::derive(
                tmdb_id,
                tmdb_type.as_deref(),
                &name,
                tvg_id.as_deref(),
                &url,
            )
            .into_string();
        }
    }
    ContentKey::derive(None, None, "", None, path).into_string()
}

/// Toggles a favorite and returns whether the stream is now a favorite. Favorites are keyed
/// by content_key (with stream URL fallback), surviving re-imports and cross-stream swaps.
pub async fn toggle_favorite(
    pool: &Pool<Sqlite>,
    profile_id: i64,
    channel_id: i64,
) -> Result<bool> {
    let row = sqlx::query(
        "SELECT name, url, group_title, tvg_id, tmdb_id, tmdb_type FROM channels WHERE id = ?",
    )
    .bind(channel_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;
    let Some(row) = row else {
        return Err(PlayerError::NotFound("Channel no longer exists".into()));
    };
    let url: String = row.get("url");
    let name: String = row.get("name");
    let tvg_id = row.get::<Option<String>, _>("tvg_id");
    let tmdb_id = row.get::<Option<i64>, _>("tmdb_id");
    let tmdb_type = row.get::<Option<String>, _>("tmdb_type");

    let content_key = ContentKey::derive(
        tmdb_id,
        tmdb_type.as_deref(),
        &name,
        tvg_id.as_deref(),
        &url,
    )
    .into_string();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let active: Option<i64> = sqlx::query_scalar(
        "SELECT active FROM favorites WHERE profile_id = ? AND (content_key = ? OR channel_url = ?)",
    )
    .bind(profile_id)
    .bind(&content_key)
    .bind(&url)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let favorite = active.unwrap_or(0) == 0;
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO favorites (profile_id, channel_url, content_key, active, updated_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (profile_id, channel_url) DO UPDATE SET
           content_key = excluded.content_key,
           active = excluded.active,
           updated_at = excluded.updated_at",
    )
    .bind(profile_id)
    .bind(&url)
    .bind(&content_key)
    .bind(i64::from(favorite))
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    sync::queue_favorite(&mut tx, profile_id, &content_key, &url, favorite, now).await?;

    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(favorite)
}

/// Channel ids of the profile's active favorites. A stream carried by more than one playlist
/// lists each of its channel rows.
pub async fn list_favorites(pool: &Pool<Sqlite>, profile_id: i64) -> Result<Vec<i64>> {
    let rows = sqlx::query(
        "SELECT DISTINCT c.id FROM favorites f \
         LEFT JOIN title_sources ts ON ts.content_key = f.content_key \
         JOIN channels c ON (c.id = ts.channel_id OR c.url = f.channel_url) \
         WHERE f.profile_id = ? AND f.active = 1 ORDER BY c.id",
    )
    .bind(profile_id)
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(rows.into_iter().map(|r| r.get("id")).collect())
}

/// Saves the manual Kind/Genre correction for a title TMDB has not matched, replacing whatever
/// was saved before (`None` for a field means "no override for that piece", not "leave it
/// alone" — the caller always sends the whole desired state). Passing `None` for both clears the
/// row entirely. Keyed by content_key so the correction survives re-imports and applies to every
/// playlist row that resolves to the same title. Refuses a channel that already carries a real
/// TMDB match: TMDB stays authoritative there.
pub async fn set_category_override(
    pool: &Pool<Sqlite>,
    channel_id: i64,
    kind: Option<&str>,
    genres: Option<&[String]>,
) -> Result<CategoryOverride> {
    let row = sqlx::query("SELECT name, url, tvg_id, tmdb_id FROM channels WHERE id = ?")
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    let Some(row) = row else {
        return Err(PlayerError::NotFound("Channel no longer exists".into()));
    };
    let tmdb_id: Option<i64> = row.get("tmdb_id");
    if tmdb_id.is_some() {
        return Err(PlayerError::InvalidArg(
            "This title already has real metadata from TMDB; it cannot be overridden".into(),
        ));
    }
    let name: String = row.get("name");
    let url: String = row.get("url");
    let tvg_id: Option<String> = row.get("tvg_id");
    let content_key = ContentKey::derive(None, None, &name, tvg_id.as_deref(), &url).into_string();
    let genres_joined = genres.map(|g| g.join(";"));

    if kind.is_none() && genres_joined.is_none() {
        sqlx::query("DELETE FROM category_overrides WHERE content_key = ?")
            .bind(&content_key)
            .execute(pool)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
        return Ok(CategoryOverride {
            content_key,
            kind: None,
            genres: None,
        });
    }

    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO category_overrides (content_key, kind, genres, created_at, updated_at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(content_key) DO UPDATE SET kind = excluded.kind, genres = excluded.genres, updated_at = excluded.updated_at",
    )
    .bind(&content_key)
    .bind(kind)
    .bind(&genres_joined)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(CategoryOverride {
        content_key,
        kind: kind.map(String::from),
        genres: genres_joined,
    })
}

/// Clears any override for a channel's content_key. Equivalent to
/// `set_category_override(pool, channel_id, None, None)`.
pub async fn clear_category_override(pool: &Pool<Sqlite>, channel_id: i64) -> Result<()> {
    set_category_override(pool, channel_id, None, None).await?;
    Ok(())
}

/// A local identity. `pin_hash` is never sent to the frontend; `has_pin` says whether one is
/// set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: i64,
    pub name: String,
    pub color: Option<String>,
    pub has_pin: bool,
    pub created_at: i64,
    pub last_used_at: i64,
}

/// The id the first migration gave the profile that owns pre-existing data.
pub const DEFAULT_PROFILE_ID: i64 = 1;

const ACTIVE_PROFILE_KEY: &str = "active_profile_id";
const MAX_PROFILE_NAME_CHARS: usize = 40;

pub(crate) fn clean_profile_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(PlayerError::InvalidArg("A profile needs a name".into()));
    }
    if trimmed.chars().count() > MAX_PROFILE_NAME_CHARS {
        return Err(PlayerError::InvalidArg(format!(
            "Profile names are limited to {} characters",
            MAX_PROFILE_NAME_CHARS
        )));
    }
    Ok(trimmed.to_string())
}

fn profile_from_row(row: &sqlx::sqlite::SqliteRow) -> Profile {
    Profile {
        id: row.get("id"),
        name: row.get("name"),
        color: row.get("color"),
        has_pin: row.get::<i64, _>("has_pin") != 0,
        created_at: row.get("created_at"),
        last_used_at: row.get("last_used_at"),
    }
}

const PROFILE_COLUMNS: &str =
    "id, name, color, pin_hash IS NOT NULL AS has_pin, created_at, last_used_at";

pub async fn list_profiles(pool: &Pool<Sqlite>) -> Result<Vec<Profile>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM profiles ORDER BY created_at, id",
        PROFILE_COLUMNS
    )))
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(rows.iter().map(profile_from_row).collect())
}

pub async fn get_profile(pool: &Pool<Sqlite>, profile_id: i64) -> Result<Profile> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM profiles WHERE id = ?",
        PROFILE_COLUMNS
    )))
    .bind(profile_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?
    .ok_or_else(|| PlayerError::NotFound("Profile no longer exists".into()))?;

    Ok(profile_from_row(&row))
}

pub async fn profile_exists(pool: &Pool<Sqlite>, profile_id: i64) -> Result<bool> {
    let row = sqlx::query("SELECT 1 AS present FROM profiles WHERE id = ?")
        .bind(profile_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(row.is_some())
}

pub async fn create_profile(
    pool: &Pool<Sqlite>,
    name: &str,
    color: Option<&str>,
) -> Result<Profile> {
    let name = clean_profile_name(name)?;
    let now = Utc::now().timestamp();
    let now_ms = Utc::now().timestamp_millis();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    // The sync key is random, so each device that creates a profile gets its own identity.
    let row = sqlx::query(
        "INSERT INTO profiles (name, color, pin_hash, created_at, last_used_at, sync_key, updated_at)
         VALUES (?, ?, NULL, ?, ?, lower(hex(randomblob(16))), ?) RETURNING id",
    )
    .bind(&name)
    .bind(color)
    .bind(now)
    .bind(now)
    .bind(now_ms)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;
    let id: i64 = row.get("id");

    sync::queue_profile(&mut tx, id).await?;

    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    get_profile(pool, id).await
}

pub async fn rename_profile(pool: &Pool<Sqlite>, profile_id: i64, name: &str) -> Result<Profile> {
    let name = clean_profile_name(name)?;
    let now_ms = Utc::now().timestamp_millis();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let result = sqlx::query("UPDATE profiles SET name = ?, updated_at = ? WHERE id = ?")
        .bind(&name)
        .bind(now_ms)
        .bind(profile_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(PlayerError::NotFound("Profile no longer exists".into()));
    }

    sync::queue_profile(&mut tx, profile_id).await?;

    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    get_profile(pool, profile_id).await
}

/// Delete a profile and everything it scoped (history and My List cascade). The last
/// remaining profile cannot be deleted, so the app always has somewhere to land.
pub async fn delete_profile(pool: &Pool<Sqlite>, profile_id: i64) -> Result<()> {
    let count: i64 = sqlx::query("SELECT COUNT(*) AS total FROM profiles")
        .fetch_one(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?
        .get("total");

    if count <= 1 {
        return Err(PlayerError::InvalidArg(
            "The only profile cannot be deleted".into(),
        ));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let sync_key: Option<String> = sqlx::query_scalar("SELECT sync_key FROM profiles WHERE id = ?")
        .bind(profile_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?
        .flatten();

    let result = sqlx::query("DELETE FROM profiles WHERE id = ?")
        .bind(profile_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(PlayerError::NotFound("Profile no longer exists".into()));
    }

    if let Some(sync_key) = sync_key {
        sync::queue_profile_deleted(&mut tx, &sync_key, Utc::now().timestamp_millis()).await?;
    }

    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(())
}

/// Set (`Some`) or clear (`None`) a profile's PIN.
pub async fn set_profile_pin(
    pool: &Pool<Sqlite>,
    profile_id: i64,
    pin: Option<&str>,
) -> Result<Profile> {
    let hash = match pin {
        Some(pin) => Some(auth::hash_pin(pin)?),
        None => None,
    };

    let result = sqlx::query("UPDATE profiles SET pin_hash = ? WHERE id = ?")
        .bind(&hash)
        .bind(profile_id)
        .execute(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(PlayerError::NotFound("Profile no longer exists".into()));
    }

    get_profile(pool, profile_id).await
}

/// Verify a PIN for a profile. A profile without a PIN verifies for any input, so callers can
/// treat "no PIN" and "correct PIN" the same way.
pub async fn verify_profile_pin(pool: &Pool<Sqlite>, profile_id: i64, pin: &str) -> Result<bool> {
    let row = sqlx::query("SELECT pin_hash FROM profiles WHERE id = ?")
        .bind(profile_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?
        .ok_or_else(|| PlayerError::NotFound("Profile no longer exists".into()))?;

    match row.get::<Option<String>, _>("pin_hash") {
        Some(hash) => Ok(auth::verify_pin(pin, &hash)),
        None => Ok(true),
    }
}

/// Proves the caller knows a profile's PIN before a change to that profile. A profile without a
/// PIN is open. Wrong guesses are throttled per profile (see `pin_guard`).
pub async fn authorize_profile(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    profile_id: i64,
    pin: Option<&str>,
) -> Result<()> {
    if let Some(wait) = pin_guard::locked_out_for(profile_id) {
        return Err(PlayerError::InvalidArg(format!(
            "Too many wrong PINs. Try again in {} seconds.",
            wait.as_secs().max(1)
        )));
    }
    if verify_profile_pin(pool, profile_id, pin.unwrap_or_default()).await? {
        pin_guard::record_success(profile_id);
        Ok(())
    } else {
        pin_guard::record_failure(profile_id);
        Err(PlayerError::InvalidArg(
            "That PIN is not correct".to_string(),
        ))
    }
}

pub async fn touch_profile(pool: &Pool<Sqlite>, profile_id: i64) -> Result<()> {
    sqlx::query("UPDATE profiles SET last_used_at = ? WHERE id = ?")
        .bind(Utc::now().timestamp())
        .bind(profile_id)
        .execute(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(())
}

/// The profile the app is acting as. Stored in `settings` so a restart continues where the
/// user left off; falls back to the oldest profile if that one is gone.
pub async fn set_active_profile(pool: &Pool<Sqlite>, profile_id: i64) -> Result<()> {
    if !profile_exists(pool, profile_id).await? {
        return Err(PlayerError::NotFound("Profile no longer exists".into()));
    }
    set_setting(pool, ACTIVE_PROFILE_KEY, &profile_id.to_string()).await
}

pub async fn active_profile_id(pool: &Pool<Sqlite>) -> Result<i64> {
    if let Some(stored) = get_setting(pool, ACTIVE_PROFILE_KEY).await?
        && let Ok(id) = stored.parse::<i64>()
        && profile_exists(pool, id).await?
    {
        return Ok(id);
    }

    let row = sqlx::query("SELECT id FROM profiles ORDER BY created_at, id LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(row.map(|row| row.get("id")).unwrap_or(DEFAULT_PROFILE_ID))
}

/// History is a "recently watched" list, not an append-only log: every write replaces the
/// previous entry for that path, and the list is capped by count and age.
#[derive(Debug, Clone, Copy)]
pub struct HistoryRetention {
    pub keep: i64,
    pub max_age_secs: i64,
}

pub const DEFAULT_HISTORY_RETENTION: HistoryRetention = HistoryRetention {
    keep: 500,
    max_age_secs: 90 * 24 * 60 * 60,
};

/// Delete history rows outside the retention window: any row older than `max_age_secs`, plus
/// every path beyond the newest `keep` paths of its own profile. Returns rows deleted.
async fn prune_history_with(
    conn: &mut SqliteConnection,
    retention: HistoryRetention,
) -> Result<u64> {
    // Age first: it is not profile-specific and needs no per-path bookkeeping.
    let cutoff = Utc::now().timestamp() - retention.max_age_secs.max(0);
    let mut deleted = sqlx::query("DELETE FROM history WHERE played_at < ?")
        .bind(cutoff)
        .execute(&mut *conn)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?
        .rows_affected();

    let profile_ids: Vec<i64> = sqlx::query("SELECT id FROM profiles ORDER BY id")
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?
        .into_iter()
        .map(|row| row.get("id"))
        .collect();

    // Then the per-profile count cap. The retained paths are resolved first: a DELETE whose
    // WHERE clause reads the table it is deleting from is not something to rely on.
    for profile_id in profile_ids {
        let keep_paths: Vec<String> = sqlx::query(
            "SELECT path FROM history WHERE profile_id = ? GROUP BY path ORDER BY MAX(played_at) DESC, MAX(id) DESC LIMIT ?",
        )
        .bind(profile_id)
        .bind(retention.keep.max(1))
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?
        .into_iter()
        .map(|row| row.get("path"))
        .collect();

        if keep_paths.is_empty() {
            continue;
        }

        let mut qb: QueryBuilder<Sqlite> =
            QueryBuilder::new("DELETE FROM history WHERE profile_id = ");
        qb.push_bind(profile_id);
        qb.push(" AND path NOT IN (");
        {
            let mut separated = qb.separated(", ");
            for path in &keep_paths {
                separated.push_bind(path);
            }
        }
        qb.push(")");

        deleted += qb
            .build()
            .execute(&mut *conn)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?
            .rows_affected();
    }

    Ok(deleted)
}

pub async fn prune_history(pool: &Pool<Sqlite>, retention: HistoryRetention) -> Result<u64> {
    // `BEGIN IMMEDIATE` takes the write lock up front: a deferred transaction that reads
    // before writing fails with SQLITE_BUSY (rather than waiting) if another connection is
    // writing, because upgrading from a read lock can deadlock.
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    let deleted = prune_history_with(&mut tx, retention).await?;
    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(deleted)
}

pub async fn add_history(
    pool: &Pool<Sqlite>,
    profile_id: i64,
    channel_id: Option<i64>,
    path: &str,
    position: f64,
    duration: f64,
) -> Result<()> {
    // `history.channel_id` references channels(id); a stale id (source deleted since the
    // player started) would abort the insert with a foreign-key error, so drop it instead.
    let channel_id = match channel_id {
        Some(id) if channel_exists(pool, id).await? => Some(id),
        _ => None,
    };

    // Resolved before the transaction opens: it reads through the pool, and a transaction already
    // holds a connection, so asking for a second one while holding the first can exhaust the pool.
    let content_key = resolve_content_key(pool, channel_id, path).await;

    let now = Utc::now().timestamp();
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let previous = sqlx::query(
        "SELECT channel_id, position, duration FROM history WHERE profile_id = ? AND (content_key = ? OR path = ?) ORDER BY played_at DESC, id DESC LIMIT 1",
    )
    .bind(profile_id)
    .bind(&content_key)
    .bind(path)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?
    .map(|row| {
        (
            row.get::<Option<i64>, _>("channel_id"),
            row.get::<f64, _>("position"),
            row.get::<f64, _>("duration"),
        )
    });

    // A playback-start ping carries no progress: keep the resume point (and the channel
    // link) it would otherwise erase.
    let (channel_id, position, duration) = match previous {
        Some((prev_channel, prev_position, prev_duration)) => (
            channel_id.or(prev_channel),
            if duration <= 0.0 && prev_duration > 0.0 && prev_position > 0.0 {
                prev_position
            } else {
                position
            },
            if duration <= 0.0 && prev_duration > 0.0 {
                prev_duration
            } else {
                duration
            },
        ),
        None => (channel_id, position, duration),
    };

    sqlx::query("DELETE FROM history WHERE profile_id = ? AND (content_key = ? OR path = ?)")
        .bind(profile_id)
        .bind(&content_key)
        .bind(path)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    sqlx::query(
        "INSERT INTO history (profile_id, channel_id, path, content_key, position, duration, played_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(profile_id)
    .bind(channel_id)
    .bind(path)
    .bind(&content_key)
    .bind(position)
    .bind(duration)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    sync::queue_progress(
        &mut tx,
        profile_id,
        &content_key,
        path,
        position,
        duration,
        now,
    )
    .await?;

    prune_history_with(&mut tx, DEFAULT_HISTORY_RETENTION).await?;

    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(())
}

pub async fn get_history(
    pool: &Pool<Sqlite>,
    profile_id: i64,
    limit: i64,
) -> Result<Vec<HistoryEntry>> {
    // One row per path. Databases written before history was compacted can still hold a
    // run of rows for a path, so prefer the newest row that carries a resume point.
    let rows = sqlx::query(
        r#"
        SELECT h.id, h.channel_id, h.path, h.position, h.duration, h.played_at
        FROM history h
        WHERE h.profile_id = ?
          AND h.id = COALESCE(
            (SELECT MAX(p.id) FROM history p
              WHERE p.profile_id = h.profile_id AND p.path = h.path
                AND p.duration > 0 AND p.position > 0),
            (SELECT MAX(p.id) FROM history p
              WHERE p.profile_id = h.profile_id AND p.path = h.path)
        )
        ORDER BY h.played_at DESC, h.id DESC
        LIMIT ?
        "#,
    )
    .bind(profile_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let history = rows
        .into_iter()
        .map(|row| HistoryEntry {
            id: row.get("id"),
            channel_id: row.get("channel_id"),
            path: row.get("path"),
            position: row.get("position"),
            duration: row.get("duration"),
            played_at: row.get("played_at"),
        })
        .collect();

    Ok(history)
}

pub async fn get_resume_position(
    pool: &Pool<Sqlite>,
    profile_id: i64,
    path: &str,
) -> Result<Option<f64>> {
    let content_key = resolve_content_key(pool, None, path).await;
    let row = sqlx::query(
        "SELECT position FROM history WHERE profile_id = ? AND (content_key = ? OR path = ?) AND duration > 0 AND position > 0 AND position < duration ORDER BY played_at DESC LIMIT 1",
    )
    .bind(profile_id)
    .bind(&content_key)
    .bind(path)
    .fetch_optional(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(row.map(|r| r.get("position")))
}

/// The page an extracted entry was made from, when `channel_id` belongs to an extracted source.
/// Such an entry's stream link is resolved when it is played: links expire and often carry tokens.
/// Entries saved by older versions hold a stream link as their URL, and still resolve from the
/// source's origin.
pub async fn extract_origin_for_channel(
    pool: &Pool<Sqlite>,
    channel_id: i64,
) -> Result<Option<String>> {
    let origin: Option<Option<String>> = sqlx::query_scalar(
        "SELECT p.origin FROM channels c JOIN playlists p ON p.id = c.playlist_id \
         WHERE c.id = ? AND p.source = 'extract'",
    )
    .bind(channel_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(origin.flatten().filter(|origin| !origin.trim().is_empty()))
}

/// Lists all available sources for a title in order of rank (for fallback).
pub async fn list_ranked_sources(
    pool: &Pool<Sqlite>,
    content_key: &str,
) -> Result<Vec<(String, Option<String>, Option<i64>)>> {
    let rows = sqlx::query(
        "SELECT stream_url, headers, channel_id FROM title_sources \
         WHERE content_key = ? ORDER BY rank DESC, id ASC",
    )
    .bind(content_key)
    .fetch_all(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(rows
        .into_iter()
        .map(|r| (r.get("stream_url"), r.get("headers"), r.get("channel_id")))
        .collect())
}

/// The source ranked directly below `failed_url` for the same title. Used when the current stream
/// fails to play. Returns None when the URL is not a ranked source or is already the last one.
pub async fn next_ranked_source_after(
    pool: &Pool<Sqlite>,
    failed_url: &str,
) -> Result<Option<(String, Option<String>, Option<i64>)>> {
    let key_row = sqlx::query(
        "SELECT content_key FROM title_sources WHERE stream_url = ? \
         ORDER BY rank DESC, id ASC LIMIT 1",
    )
    .bind(failed_url)
    .fetch_optional(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let Some(key_row) = key_row else {
        return Ok(None);
    };
    let content_key: String = key_row.get("content_key");
    let ranked = list_ranked_sources(pool, &content_key).await?;
    let position = ranked.iter().position(|(url, _, _)| url == failed_url);
    Ok(position.and_then(|index| ranked.into_iter().nth(index + 1)))
}

#[derive(Debug, Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub channel_id: Option<i64>,
    pub path: String,
    pub position: f64,
    pub duration: f64,
    pub played_at: i64,
}

pub async fn get_setting(pool: &Pool<Sqlite>, key: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    Ok(row.map(|r| r.get("value")))
}

pub async fn delete_setting(pool: &Pool<Sqlite>, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(())
}

pub async fn set_setting(pool: &Pool<Sqlite>, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = ?",
    )
    .bind(key)
    .bind(value)
    .bind(value)
    .execute(pool)
    .await
    .map_err(|e| PlayerError::Sql(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connected database with no schema applied yet.
    async fn empty_pool() -> Pool<Sqlite> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::query("PRAGMA foreign_keys = ON;")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn test_pool() -> Pool<Sqlite> {
        let pool = empty_pool().await;
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        // `list_channels` reads the enrichment columns, which `init_db` adds after migrating.
        migrate_tmdb_columns(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn extracted_entries_resolve_from_their_page_not_a_stored_link() {
        let pool = test_pool().await;
        let now = 1_700_000_000;
        let make = |name: &str, source: &str, origin: Option<&str>, url: &str| {
            (
                Playlist {
                    id: None,
                    name: name.into(),
                    source: source.into(),
                    origin: origin.map(String::from),
                    created_at: now,
                    updated_at: now,
                },
                Channel {
                    id: None,
                    playlist_id: 0,
                    name: name.into(),
                    url: url.into(),
                    group_title: None,
                    tvg_id: None,
                    tvg_logo: None,
                    language: None,
                    extra_headers: None,
                    last_ok: None,
                    last_error: None,
                    is_live: None,
                },
            )
        };
        let (extract_pl, extract_ch) = make(
            "Clip",
            "extract",
            Some("https://video.example/watch?v=1"),
            "https://video.example/watch?v=1",
        );
        let (legacy_pl, legacy_ch) = make(
            "Old clip",
            "extract",
            Some("https://video.example/watch?v=2"),
            "https://cdn.example/expired.mp4?sig=x",
        );
        let (m3u_pl, m3u_ch) = make(
            "Playlist",
            "url",
            Some("https://lists.example/a.m3u"),
            "https://cdn.example/live.m3u8",
        );
        let mut ids = Vec::new();
        for (playlist, channel) in [
            (extract_pl, extract_ch),
            (legacy_pl, legacy_ch),
            (m3u_pl, m3u_ch),
        ] {
            let playlist_id = import_playlist(&pool, &playlist, &[channel]).await.unwrap();
            let id: i64 = sqlx::query_scalar("SELECT id FROM channels WHERE playlist_id = ?")
                .bind(playlist_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            ids.push(id);
        }
        assert_eq!(
            extract_origin_for_channel(&pool, ids[0])
                .await
                .unwrap()
                .as_deref(),
            Some("https://video.example/watch?v=1")
        );
        assert_eq!(
            extract_origin_for_channel(&pool, ids[1])
                .await
                .unwrap()
                .as_deref(),
            Some("https://video.example/watch?v=2"),
            "an entry saved by an older version holds a stream link, but still resolves from its page"
        );
        assert_eq!(
            extract_origin_for_channel(&pool, ids[2]).await.unwrap(),
            None,
            "playlist entries play their own URL"
        );
        assert_eq!(
            extract_origin_for_channel(&pool, 9_999).await.unwrap(),
            None
        );
    }

    fn test_channel(name: &str, url: &str, tvg_id: Option<&str>) -> Channel {
        Channel {
            id: None,
            playlist_id: 0,
            name: name.into(),
            url: url.into(),
            group_title: None,
            tvg_id: tvg_id.map(String::from),
            tvg_logo: None,
            language: None,
            extra_headers: None,
            last_ok: None,
            last_error: None,
            is_live: None,
        }
    }

    async fn insert_test_playlist(pool: &Pool<Sqlite>, channels: &[Channel]) -> i64 {
        let playlist = Playlist {
            id: None,
            name: "Test".into(),
            source: "url".into(),
            origin: None,
            created_at: 1000,
            updated_at: 1000,
        };
        import_playlist(pool, &playlist, channels).await.unwrap()
    }

    #[tokio::test]
    async fn setting_an_override_is_merged_into_list_channels() {
        let pool = test_pool().await;
        let playlist_id = insert_test_playlist(
            &pool,
            &[test_channel(
                "Local News",
                "https://cdn.example/news.m3u8",
                Some("news.local"),
            )],
        )
        .await;
        let channels = list_channels(&pool, playlist_id, None, None).await.unwrap();
        let channel_id = channels[0].channel.id.unwrap();
        assert_eq!(channels[0].category_override_kind, None);

        set_category_override(
            &pool,
            channel_id,
            Some("series"),
            Some(&["Comedy".to_string(), "Family".to_string()]),
        )
        .await
        .unwrap();

        let channels = list_channels(&pool, playlist_id, None, None).await.unwrap();
        assert_eq!(
            channels[0].category_override_kind.as_deref(),
            Some("series")
        );
        assert_eq!(
            channels[0].category_override_genres.as_deref(),
            Some("Comedy;Family")
        );
    }

    /// `fetch_category_overrides` chunks its `IN (...)` query at 100 keys, the same limit
    /// `insert_channel_rows` uses for batch inserts. A playlist with more unmatched channels
    /// than that must still return every override merged in, not fail or silently drop some.
    #[tokio::test]
    async fn overrides_past_the_single_query_chunk_size_are_all_merged_in() {
        let pool = test_pool().await;
        let channels: Vec<Channel> = (0..150)
            .map(|index| {
                test_channel(
                    &format!("Channel {index}"),
                    &format!("https://cdn.example/ch{index}.m3u8"),
                    Some(&format!("ch{index}")),
                )
            })
            .collect();
        let playlist_id = insert_test_playlist(&pool, &channels).await;
        let listed = list_channels(&pool, playlist_id, None, None).await.unwrap();
        assert_eq!(listed.len(), 150);

        for channel in &listed {
            set_category_override(
                &pool,
                channel.channel.id.unwrap(),
                Some("live"),
                Some(&["News".to_string()]),
            )
            .await
            .unwrap();
        }

        let listed = list_channels(&pool, playlist_id, None, None).await.unwrap();
        assert_eq!(listed.len(), 150);
        assert!(
            listed
                .iter()
                .all(|channel| channel.category_override_kind.as_deref() == Some("live")),
            "every one of the 150 overrides must be merged in, not just the first chunk"
        );
    }

    #[tokio::test]
    async fn an_override_is_refused_once_tmdb_has_matched_the_channel() {
        let pool = test_pool().await;
        let playlist_id = insert_test_playlist(
            &pool,
            &[test_channel(
                "Fight Club",
                "https://cdn.example/fc.m3u8",
                None,
            )],
        )
        .await;
        let channels = list_channels(&pool, playlist_id, None, None).await.unwrap();
        let channel_id = channels[0].channel.id.unwrap();
        sqlx::query("UPDATE channels SET tmdb_id = 550, tmdb_type = 'movie' WHERE id = ?")
            .bind(channel_id)
            .execute(&pool)
            .await
            .unwrap();

        let error = set_category_override(&pool, channel_id, Some("movie"), None)
            .await
            .unwrap_err();
        assert!(matches!(error, PlayerError::InvalidArg(_)));

        // list_channels must never surface an override for a TMDB-matched row either, even if
        // one somehow still exists in the table.
        let channels = list_channels(&pool, playlist_id, None, None).await.unwrap();
        assert_eq!(channels[0].category_override_kind, None);
    }

    #[tokio::test]
    async fn two_channels_sharing_a_tvg_id_share_one_override() {
        let pool = test_pool().await;
        let playlist_id = insert_test_playlist(
            &pool,
            &[
                test_channel("News HD", "https://a.example/news.m3u8", Some("news.id")),
                test_channel(
                    "News SD (mirror)",
                    "https://b.example/news-mirror.m3u8",
                    Some("news.id"),
                ),
            ],
        )
        .await;
        let channels = list_channels(&pool, playlist_id, None, None).await.unwrap();
        let first_id = channels[0].channel.id.unwrap();

        set_category_override(&pool, first_id, Some("live"), Some(&["News".to_string()]))
            .await
            .unwrap();

        let channels = list_channels(&pool, playlist_id, None, None).await.unwrap();
        assert!(
            channels
                .iter()
                .all(|c| c.category_override_kind.as_deref() == Some("live")),
            "both rows share a content_key, so both must see the same override"
        );
    }

    #[tokio::test]
    async fn clearing_an_override_reverts_to_inferred_values() {
        let pool = test_pool().await;
        let playlist_id = insert_test_playlist(
            &pool,
            &[test_channel(
                "Local News",
                "https://cdn.example/news.m3u8",
                Some("news.local"),
            )],
        )
        .await;
        let channels = list_channels(&pool, playlist_id, None, None).await.unwrap();
        let channel_id = channels[0].channel.id.unwrap();

        set_category_override(&pool, channel_id, Some("series"), None)
            .await
            .unwrap();
        clear_category_override(&pool, channel_id).await.unwrap();

        let channels = list_channels(&pool, playlist_id, None, None).await.unwrap();
        assert_eq!(channels[0].category_override_kind, None);
        assert_eq!(channels[0].category_override_genres, None);
    }

    #[tokio::test]
    async fn test_next_ranked_source_after_follows_rank_order() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO titles (content_key, kind, title, created_at, updated_at) \
             VALUES ('tmdb:movie:550', 'movie', 'Fight Club', 1000, 1000)",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (url, rank) in [
            ("https://a.example/hd.m3u8", 2),
            ("https://b.example/sd.m3u8", 1),
            ("https://c.example/lo.m3u8", 0),
        ] {
            sqlx::query(
                "INSERT INTO title_sources (content_key, stream_url, rank, created_at) \
                 VALUES ('tmdb:movie:550', ?, ?, 1000)",
            )
            .bind(url)
            .bind(rank)
            .execute(&pool)
            .await
            .unwrap();
        }

        let next = next_ranked_source_after(&pool, "https://a.example/hd.m3u8")
            .await
            .unwrap();
        assert_eq!(
            next.map(|(url, _, _)| url),
            Some("https://b.example/sd.m3u8".to_string())
        );

        let next = next_ranked_source_after(&pool, "https://b.example/sd.m3u8")
            .await
            .unwrap();
        assert_eq!(
            next.map(|(url, _, _)| url),
            Some("https://c.example/lo.m3u8".to_string())
        );

        // The lowest-ranked source has nothing below it, and unknown URLs have no title.
        assert!(
            next_ranked_source_after(&pool, "https://c.example/lo.m3u8")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            next_ranked_source_after(&pool, "https://unknown.example/x.m3u8")
                .await
                .unwrap()
                .is_none()
        );
    }

    fn playlist(name: &str) -> Playlist {
        Playlist {
            id: None,
            name: name.to_string(),
            source: "file".to_string(),
            origin: None,
            created_at: 1000,
            updated_at: 1000,
        }
    }

    fn channel(playlist_id: i64, name: &str, url: &str) -> Channel {
        Channel {
            id: None,
            playlist_id,
            name: name.to_string(),
            url: url.to_string(),
            group_title: None,
            tvg_id: None,
            tvg_logo: None,
            language: None,
            extra_headers: None,
            last_ok: None,
            last_error: None,
            is_live: None,
        }
    }

    #[tokio::test]
    async fn test_backfill_recovers_language_labels_from_local_playlist_files() {
        let pool = test_pool().await;
        let dir =
            std::env::temp_dir().join(format!("lumen-language-backfill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("list.m3u");
        std::fs::write(
            &path,
            "#EXTM3U\n#EXTINF:-1 tvg-language=\"English\",One\nhttps://example.com/one.m3u8\n#EXTINF:-1 tvg-language=\"French\",Two\nhttps://example.com/two.m3u8\n#EXTINF:-1,Three\nhttps://example.com/three.m3u8\n",
        )
        .unwrap();

        // Imported before `language` was stored, so every channel starts unlabeled.
        let mut local = playlist("list");
        local.origin = Some(path.to_string_lossy().to_string());
        let id = import_playlist(
            &pool,
            &local,
            &[
                channel(0, "One", "https://example.com/one.m3u8"),
                channel(0, "Two", "https://example.com/two.m3u8"),
                channel(0, "Three", "https://example.com/three.m3u8"),
            ],
        )
        .await
        .unwrap();

        backfill_channel_languages(&pool).await.unwrap();

        let labels: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT url, language FROM channels WHERE playlist_id = ? ORDER BY url")
                .bind(id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            labels,
            vec![
                (
                    "https://example.com/one.m3u8".to_string(),
                    Some("English".to_string())
                ),
                ("https://example.com/three.m3u8".to_string(), None),
                (
                    "https://example.com/two.m3u8".to_string(),
                    Some("French".to_string())
                ),
            ]
        );
        assert_eq!(
            get_setting(&pool, CHANNEL_LANGUAGE_BACKFILL_SETTING)
                .await
                .unwrap()
                .as_deref(),
            Some("done")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_backfill_retries_on_the_next_start_when_a_playlist_file_is_missing() {
        let pool = test_pool().await;
        let mut gone = playlist("gone");
        gone.origin = Some("/nonexistent/lumen-missing-playlist.m3u".to_string());
        import_playlist(
            &pool,
            &gone,
            &[channel(0, "One", "https://example.com/gone.m3u8")],
        )
        .await
        .unwrap();

        backfill_channel_languages(&pool).await.unwrap();

        // Not recorded as done, so a later start tries the file again.
        assert_eq!(
            get_setting(&pool, CHANNEL_LANGUAGE_BACKFILL_SETTING)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn test_empty_discovery_does_not_persist_a_source() {
        let pool = test_pool().await;
        let discovered = playlist("Harvested: example.org");

        assert_eq!(
            import_playlist_if_nonempty(&pool, &discovered, &[])
                .await
                .unwrap(),
            None
        );
        assert!(list_playlists(&pool).await.unwrap().is_empty());

        let channel = channel(0, "Found stream", "https://example.org/live.m3u8");
        let id = import_playlist_if_nonempty(&pool, &discovered, &[channel])
            .await
            .unwrap()
            .expect("a nonempty discovery should create its source");
        assert_eq!(list_playlists(&pool).await.unwrap()[0].id, Some(id));
    }

    #[tokio::test]
    async fn test_sqlite_batch_insert_favorites_and_settings() {
        let pool = test_pool().await;

        let mut chans = Vec::new();
        for idx in 0..250 {
            let mut chan = channel(
                0,
                &format!("Channel {:03}", idx),
                &format!("https://example.com/stream/{}.m3u8", idx),
            );
            chan.group_title = Some(if idx % 2 == 0 {
                "News".into()
            } else {
                "Sports".into()
            });
            chans.push(chan);
        }
        let pid = import_playlist(&pool, &playlist("Test 250 Channels"), &chans)
            .await
            .unwrap();

        let all = list_channels(&pool, pid, None, None).await.unwrap();
        assert_eq!(all.len(), 250);

        let news = list_channels(&pool, pid, None, Some("News")).await.unwrap();
        assert_eq!(news.len(), 125);

        let first_id = all[0].channel.id.unwrap();
        assert!(
            toggle_favorite(&pool, DEFAULT_PROFILE_ID, first_id)
                .await
                .unwrap()
        );
        assert_eq!(
            list_favorites(&pool, DEFAULT_PROFILE_ID).await.unwrap(),
            vec![first_id]
        );
        assert!(
            !toggle_favorite(&pool, DEFAULT_PROFILE_ID, first_id)
                .await
                .unwrap()
        );

        set_setting(&pool, "hwdec", "vaapi").await.unwrap();
        assert_eq!(
            get_setting(&pool, "hwdec").await.unwrap().as_deref(),
            Some("vaapi")
        );

        add_history(
            &pool,
            DEFAULT_PROFILE_ID,
            Some(first_id),
            "/tmp/video.mkv",
            42.5,
            120.0,
        )
        .await
        .unwrap();
        assert_eq!(
            get_resume_position(&pool, DEFAULT_PROFILE_ID, "/tmp/video.mkv")
                .await
                .unwrap(),
            Some(42.5)
        );
    }

    #[tokio::test]
    async fn test_import_is_transactional_and_delete_cascades() {
        let pool = test_pool().await;

        // A channel insert that violates the playlist foreign key fails, and nothing from
        // the aborted transaction may survive it.
        let mut tx = pool.begin().await.unwrap();
        let orphan = insert_channel_rows(
            &mut tx,
            9999,
            &[channel(9999, "Orphan", "https://example.com/orphan.m3u8")],
        )
        .await;
        assert!(
            orphan.is_err(),
            "foreign key violation should fail the insert"
        );
        tx.rollback().await.unwrap();
        assert!(list_playlists(&pool).await.unwrap().is_empty());

        let pid = import_playlist(
            &pool,
            &playlist("Atomic"),
            &[channel(0, "Kept", "https://example.com/kept.m3u8")],
        )
        .await
        .unwrap();
        let channels = list_channels(&pool, pid, None, None).await.unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(
            channels[0].channel.playlist_id, pid,
            "channels belong to the new playlist"
        );

        let channel_id = channels[0].channel.id.unwrap();
        add_history(
            &pool,
            DEFAULT_PROFILE_ID,
            Some(channel_id),
            "https://example.com/kept.m3u8",
            12.0,
            60.0,
        )
        .await
        .unwrap();

        // Deleting the source removes its channels; history keeps the entry, unlinked.
        delete_playlist(&pool, pid).await.unwrap();
        assert!(
            list_channels(&pool, pid, None, None)
                .await
                .unwrap()
                .is_empty()
        );
        let history = get_history(&pool, DEFAULT_PROFILE_ID, 10).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].channel_id, None);
    }

    #[tokio::test]
    async fn test_history_compaction_keeps_one_row_per_path() {
        let pool = test_pool().await;
        let pid = import_playlist(
            &pool,
            &playlist("History"),
            &[channel(0, "One", "https://example.com/one.m3u8")],
        )
        .await
        .unwrap();
        let channel_id = list_channels(&pool, pid, None, None).await.unwrap()[0]
            .channel
            .id
            .unwrap();

        add_history(
            &pool,
            DEFAULT_PROFILE_ID,
            Some(channel_id),
            "/media/movie.mkv",
            0.0,
            0.0,
        )
        .await
        .unwrap();
        add_history(
            &pool,
            DEFAULT_PROFILE_ID,
            Some(channel_id),
            "/media/movie.mkv",
            42.5,
            120.0,
        )
        .await
        .unwrap();
        // A later playback-start ping must not erase the resume point or the channel link.
        add_history(
            &pool,
            DEFAULT_PROFILE_ID,
            Some(channel_id),
            "/media/movie.mkv",
            0.0,
            0.0,
        )
        .await
        .unwrap();

        let rows = get_history(&pool, DEFAULT_PROFILE_ID, 50).await.unwrap();
        assert_eq!(rows.len(), 1, "history holds one row per path");
        assert_eq!(rows[0].position, 42.5);
        assert_eq!(rows[0].duration, 120.0);
        assert_eq!(rows[0].channel_id, Some(channel_id));
        assert_eq!(
            get_resume_position(&pool, DEFAULT_PROFILE_ID, "/media/movie.mkv")
                .await
                .unwrap(),
            Some(42.5)
        );

        // Playback that restarts from the beginning clears the resume point.
        add_history(
            &pool,
            DEFAULT_PROFILE_ID,
            Some(channel_id),
            "/media/movie.mkv",
            0.0,
            120.0,
        )
        .await
        .unwrap();
        assert_eq!(
            get_history(&pool, DEFAULT_PROFILE_ID, 50)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            get_resume_position(&pool, DEFAULT_PROFILE_ID, "/media/movie.mkv")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn test_prune_history_drops_paths_beyond_count_and_age() {
        let pool = test_pool().await;
        for path in ["/media/one.mkv", "/media/two.mkv", "/media/three.mkv"] {
            add_history(&pool, DEFAULT_PROFILE_ID, None, path, 10.0, 100.0)
                .await
                .unwrap();
        }
        assert_eq!(
            get_history(&pool, DEFAULT_PROFILE_ID, 50)
                .await
                .unwrap()
                .len(),
            3
        );

        let deleted = prune_history(
            &pool,
            HistoryRetention {
                keep: 2,
                max_age_secs: 90 * 24 * 60 * 60,
            },
        )
        .await
        .unwrap();
        assert_eq!(deleted, 1);
        let remaining: Vec<String> = get_history(&pool, DEFAULT_PROFILE_ID, 50)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(remaining.len(), 2);
        assert!(!remaining.contains(&"/media/one.mkv".to_string()));

        // Age-based retention drops what a previous version left behind.
        sqlx::query(
            "INSERT INTO history (profile_id, channel_id, path, position, duration, played_at) VALUES (?, NULL, '/media/old.mkv', 5.0, 10.0, ?)",
        )
        .bind(DEFAULT_PROFILE_ID)
        .bind(Utc::now().timestamp() - 400 * 24 * 60 * 60)
        .execute(&pool)
        .await
        .unwrap();

        let deleted = prune_history(&pool, DEFAULT_HISTORY_RETENTION)
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        assert!(
            !get_history(&pool, DEFAULT_PROFILE_ID, 50)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.path == "/media/old.mkv")
        );
    }

    #[tokio::test]
    async fn test_get_history_prefers_progress_for_legacy_duplicate_rows() {
        let pool = test_pool().await;
        let now = Utc::now().timestamp();

        // Rows an older build would have appended for one path: the newest carries no
        // progress yet.
        for (position, duration, played_at) in [(77.0, 200.0, now - 100), (0.0, 0.0, now - 50)] {
            sqlx::query(
                "INSERT INTO history (profile_id, channel_id, path, position, duration, played_at) VALUES (?, NULL, '/media/legacy.mkv', ?, ?, ?)",
            )
            .bind(DEFAULT_PROFILE_ID)
            .bind(position)
            .bind(duration)
            .bind(played_at)
            .execute(&pool)
            .await
            .unwrap();
        }

        let rows = get_history(&pool, DEFAULT_PROFILE_ID, 50).await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "legacy duplicate rows are collapsed when read"
        );
        assert_eq!(rows[0].position, 77.0);
        assert_eq!(
            get_resume_position(&pool, DEFAULT_PROFILE_ID, "/media/legacy.mkv")
                .await
                .unwrap(),
            Some(77.0)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_history_writes_keep_one_row_per_path() {
        let dir = std::env::temp_dir().join(format!("lumen-db-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.join("lumen.db").display()
            ))
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        // Overlapping progress saves must serialize instead of failing with SQLITE_BUSY.
        let mut writes = Vec::new();
        for idx in 0..24 {
            let pool = pool.clone();
            writes.push(tokio::spawn(async move {
                add_history(
                    &pool,
                    DEFAULT_PROFILE_ID,
                    None,
                    &format!("/media/concurrent-{}.mkv", idx % 8),
                    1.0 + idx as f64,
                    100.0,
                )
                .await
            }));
        }
        for write in writes {
            write
                .await
                .expect("history write task should not panic")
                .expect("concurrent history write should succeed");
        }

        assert_eq!(
            get_history(&pool, DEFAULT_PROFILE_ID, 50)
                .await
                .unwrap()
                .len(),
            8,
            "every path keeps exactly one row"
        );

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_profile_migration_adopts_existing_watch_state() {
        // Rebuild the pre-profile schema from the migration files themselves, fill it with
        // the watch state a user already has, then apply the profile migration the way an
        // upgrade would. This is the one migration that rewrites existing history rows.
        let pool = empty_pool().await;
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../migrations/002_history_indexes.sql"))
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO playlists (id, name, source, origin, created_at, updated_at) VALUES (1, 'Old list', 'file', NULL, 1000, 1000)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO channels (id, playlist_id, name, url) VALUES (7, 1, 'Old channel', 'https://example.com/old.m3u8')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO favorites (channel_id) VALUES (7)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO history (channel_id, path, position, duration, played_at) VALUES (7, 'https://example.com/old.m3u8', 12.5, 60.0, 2000)",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(include_str!("../migrations/003_profiles.sql"))
            .execute(&pool)
            .await
            .unwrap();
        // Favorites move from channel ids to channel URLs; the favorite must survive that.
        sqlx::raw_sql(include_str!("../migrations/006_sync.sql"))
            .execute(&pool)
            .await
            .unwrap();
        // The remaining migrations an upgraded database runs before the app reads these tables.
        sqlx::raw_sql(include_str!("../migrations/007_channel_live.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../migrations/008_titles.sql"))
            .execute(&pool)
            .await
            .unwrap();

        // Pre-existing data is adopted by the first profile instead of being lost.
        let profiles = list_profiles(&pool).await.unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, DEFAULT_PROFILE_ID);
        assert!(!profiles[0].has_pin);
        assert_eq!(
            list_favorites(&pool, DEFAULT_PROFILE_ID).await.unwrap(),
            vec![7]
        );
        assert_eq!(
            get_resume_position(&pool, DEFAULT_PROFILE_ID, "https://example.com/old.m3u8")
                .await
                .unwrap(),
            Some(12.5)
        );
        let rows = get_history(&pool, DEFAULT_PROFILE_ID, 50).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].channel_id, Some(7));

        // A new profile starts empty while the library stays shared.
        let second = create_profile(&pool, "Sam", None).await.unwrap();
        assert!(list_favorites(&pool, second.id).await.unwrap().is_empty());
        assert!(get_history(&pool, second.id, 50).await.unwrap().is_empty());

        // The rebuilt tables keep their cascade behaviour.
        delete_playlist(&pool, 1).await.unwrap();
        assert_eq!(
            get_history(&pool, DEFAULT_PROFILE_ID, 50).await.unwrap()[0].channel_id,
            None
        );
        assert!(
            list_favorites(&pool, DEFAULT_PROFILE_ID)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_profiles_scope_history_and_my_list() {
        let pool = test_pool().await;
        let pid = import_playlist(
            &pool,
            &playlist("Shared library"),
            &[channel(0, "One", "https://example.com/one.m3u8")],
        )
        .await
        .unwrap();
        let channel_id = list_channels(&pool, pid, None, None).await.unwrap()[0]
            .channel
            .id
            .unwrap();

        let second = create_profile(&pool, "  Sam  ", Some("teal"))
            .await
            .unwrap();
        assert_eq!(second.name, "Sam", "names are trimmed");
        assert!(!second.has_pin);

        // The library is shared, the watch state is not.
        add_history(
            &pool,
            DEFAULT_PROFILE_ID,
            Some(channel_id),
            "https://example.com/one.m3u8",
            30.0,
            60.0,
        )
        .await
        .unwrap();
        toggle_favorite(&pool, second.id, channel_id).await.unwrap();

        assert_eq!(
            get_resume_position(&pool, DEFAULT_PROFILE_ID, "https://example.com/one.m3u8")
                .await
                .unwrap(),
            Some(30.0)
        );
        assert_eq!(
            get_resume_position(&pool, second.id, "https://example.com/one.m3u8")
                .await
                .unwrap(),
            None,
            "another profile does not inherit a resume point"
        );
        assert_eq!(
            list_favorites(&pool, second.id).await.unwrap(),
            vec![channel_id]
        );
        assert!(
            list_favorites(&pool, DEFAULT_PROFILE_ID)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(get_history(&pool, second.id, 50).await.unwrap().is_empty());

        // Deleting a profile takes its history and My List with it, and leaves the rest alone.
        delete_profile(&pool, second.id).await.unwrap();
        assert!(list_favorites(&pool, second.id).await.unwrap().is_empty());
        assert_eq!(
            get_history(&pool, DEFAULT_PROFILE_ID, 50)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            get_resume_position(&pool, DEFAULT_PROFILE_ID, "https://example.com/one.m3u8")
                .await
                .unwrap(),
            Some(30.0)
        );
    }

    #[tokio::test]
    async fn test_profile_pin_locks_without_storing_the_pin() {
        let pool = test_pool().await;
        let profile = get_profile(&pool, DEFAULT_PROFILE_ID).await.unwrap();
        assert!(!profile.has_pin);
        assert!(
            verify_profile_pin(&pool, profile.id, "anything")
                .await
                .unwrap(),
            "a profile without a PIN is open"
        );

        let locked = set_profile_pin(&pool, profile.id, Some("4821"))
            .await
            .unwrap();
        assert!(locked.has_pin);
        assert!(verify_profile_pin(&pool, profile.id, "4821").await.unwrap());
        assert!(!verify_profile_pin(&pool, profile.id, "1111").await.unwrap());
        assert!(!verify_profile_pin(&pool, profile.id, "").await.unwrap());

        // Only a hash is persisted; the PIN itself never reaches the database.
        let stored: Option<String> = sqlx::query("SELECT pin_hash FROM profiles WHERE id = ?")
            .bind(profile.id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("pin_hash");
        let stored = stored.expect("a PIN hash should be stored");
        assert!(stored.starts_with("$argon2id$"));
        assert!(!stored.contains("4821"));

        // A too-short PIN is rejected before anything is written.
        assert!(
            set_profile_pin(&pool, profile.id, Some("12"))
                .await
                .is_err()
        );

        let cleared = set_profile_pin(&pool, profile.id, None).await.unwrap();
        assert!(!cleared.has_pin);
        assert!(
            verify_profile_pin(&pool, profile.id, "anything")
                .await
                .unwrap()
        );
    }

    /// Only this test calls `authorize_profile`: the attempt limiter is process-wide and keyed by
    /// profile id, so a second test reusing an id could lock it out mid-run.
    #[tokio::test]
    async fn test_profile_changes_require_the_current_pin() {
        let pool = test_pool().await;
        let profile = create_profile(&pool, "Parent", None).await.unwrap();

        authorize_profile(&pool, profile.id, None)
            .await
            .expect("a profile without a PIN is open");

        set_profile_pin(&pool, profile.id, Some("4821"))
            .await
            .unwrap();
        assert!(
            authorize_profile(&pool, profile.id, None).await.is_err(),
            "no PIN supplied"
        );
        assert!(
            authorize_profile(&pool, profile.id, Some("0000"))
                .await
                .is_err(),
            "wrong PIN"
        );
        authorize_profile(&pool, profile.id, Some("4821"))
            .await
            .expect("the right PIN is accepted");

        // Repeated wrong guesses lock the profile, and then even the right PIN waits.
        for _ in 0..5 {
            let _ = authorize_profile(&pool, profile.id, Some("0000")).await;
        }
        let locked = authorize_profile(&pool, profile.id, Some("4821"))
            .await
            .unwrap_err();
        assert!(String::from(locked).contains("Too many wrong PINs"));

        pin_guard::record_success(profile.id);
        authorize_profile(&pool, profile.id, Some("4821"))
            .await
            .unwrap();
        pin_guard::record_success(profile.id);
    }

    #[tokio::test]
    async fn test_live_flag_survives_import_and_reload() {
        let pool = test_pool().await;
        let mut live = channel(0, "Live", "http://x.example/live.m3u8");
        live.is_live = Some(true);
        let mut film = channel(0, "Film", "http://x.example/film.mp4");
        film.is_live = Some(false);
        let unknown = channel(0, "Unknown", "http://x.example/unknown.m3u8");
        let pid = import_playlist(&pool, &playlist("Flags"), &[live, film, unknown])
            .await
            .unwrap();

        let rows = list_channels(&pool, pid, None, None).await.unwrap();
        let flag = |name: &str| {
            rows.iter()
                .find(|row| row.channel.name == name)
                .unwrap()
                .channel
                .is_live
        };
        assert_eq!(flag("Live"), Some(true));
        assert_eq!(flag("Film"), Some(false));
        assert_eq!(flag("Unknown"), None);
    }

    #[tokio::test]
    async fn test_channel_search_treats_percent_and_underscore_literally() {
        let pool = test_pool().await;
        let pid = import_playlist(
            &pool,
            &playlist("Wildcards"),
            &[
                channel(0, "100% Real", "http://x.example/1.m3u8"),
                channel(0, "100 Really", "http://x.example/2.m3u8"),
                channel(0, "a_b", "http://x.example/3.m3u8"),
                channel(0, "axb", "http://x.example/4.m3u8"),
            ],
        )
        .await
        .unwrap();

        let names = |rows: Vec<EnrichedChannel>| -> Vec<String> {
            let mut names: Vec<String> = rows.into_iter().map(|row| row.channel.name).collect();
            names.sort();
            names
        };
        assert_eq!(
            names(list_channels(&pool, pid, Some("100%"), None).await.unwrap()),
            vec!["100% Real"]
        );
        assert_eq!(
            names(list_channels(&pool, pid, Some("a_b"), None).await.unwrap()),
            vec!["a_b"]
        );
        assert_eq!(
            names(list_channels(&pool, pid, Some("real"), None).await.unwrap()),
            vec!["100 Really", "100% Real"]
        );
        assert_eq!(contains_pattern(r"50%_\"), r"%50\%\_\\%");
    }

    #[tokio::test]
    async fn test_profile_name_guards_and_last_profile_protection() {
        let pool = test_pool().await;

        assert!(
            create_profile(&pool, "   ", None).await.is_err(),
            "empty name"
        );
        assert!(
            create_profile(&pool, &"x".repeat(41), None).await.is_err(),
            "name too long"
        );
        assert!(
            rename_profile(&pool, 9876, "Ghost").await.is_err(),
            "missing profile"
        );
        assert!(set_profile_pin(&pool, 9876, Some("4821")).await.is_err());
        assert!(verify_profile_pin(&pool, 9876, "4821").await.is_err());

        assert_eq!(list_profiles(&pool).await.unwrap().len(), 1);
        assert!(
            delete_profile(&pool, DEFAULT_PROFILE_ID).await.is_err(),
            "the only profile must survive"
        );

        let renamed = rename_profile(&pool, DEFAULT_PROFILE_ID, "Quinton")
            .await
            .unwrap();
        assert_eq!(renamed.name, "Quinton");
    }

    #[tokio::test]
    async fn test_active_profile_persists_and_falls_back() {
        let pool = test_pool().await;
        // With nothing stored, the oldest profile is active.
        assert_eq!(active_profile_id(&pool).await.unwrap(), DEFAULT_PROFILE_ID);

        let second = create_profile(&pool, "Sam", None).await.unwrap();
        set_active_profile(&pool, second.id).await.unwrap();
        touch_profile(&pool, second.id).await.unwrap();
        assert_eq!(active_profile_id(&pool).await.unwrap(), second.id);
        assert!(get_profile(&pool, second.id).await.unwrap().last_used_at >= second.last_used_at);

        assert!(
            set_active_profile(&pool, 9876).await.is_err(),
            "unknown profile"
        );

        // Deleting the active profile must not leave the app pointing at nothing.
        delete_profile(&pool, second.id).await.unwrap();
        assert_eq!(active_profile_id(&pool).await.unwrap(), DEFAULT_PROFILE_ID);
    }

    /// A cold start has no pool yet, and the UI fires several commands at once (`list_playlists`,
    /// `list_favorites`, `get_history`, `list_profiles`, `active_profile`), each of which reaches
    /// `ensure_db`. They must all come back with a working database instead of several of them
    /// trying to create the schema at the same time.
    #[tokio::test]
    async fn test_concurrent_startup_initializes_once() {
        let dir =
            std::env::temp_dir().join(format!("lumen-init-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(6));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let dir = dir.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                // Everything starts at the same moment, like five commands arriving together.
                barrier.wait().await;
                init_db(&dir).await
            }));
        }

        for handle in handles {
            handle.await.unwrap().expect(
                "a request racing the first one must get a working database, not a failed migration",
            );
        }

        // The process ends up with exactly one pool, and it is fully migrated.
        let pool = get_pool().expect("the first successful init_db owns the process pool");
        assert_eq!(list_profiles(pool).await.unwrap().len(), 1);
        assert!(list_playlists(pool).await.unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting a playlist tombstones an orphaned favorite under the exact same sync item_key
    /// it was created with, not a freshly re-derived one — otherwise the tombstone pushes a new,
    /// unrelated sync record and the original "active favorite" record never gets cleared on
    /// other devices.
    #[tokio::test]
    async fn deleting_a_playlist_tombstones_a_favorite_under_its_original_sync_key() {
        let pool = test_pool().await;
        let playlist_id = insert_test_playlist(
            &pool,
            &[test_channel(
                "BBC One",
                "https://cdn.example/bbc1.m3u8",
                Some("bbc1.uk"),
            )],
        )
        .await;
        let channel_id = list_channels(&pool, playlist_id, None, None).await.unwrap()[0]
            .channel
            .id
            .unwrap();

        // A profile with a sync key behaves as if sync is enabled, without needing the real
        // keyring/server plumbing: `queue_favorite` only checks `profiles.sync_key`.
        sqlx::query("UPDATE profiles SET sync_key = 'test-sync-key' WHERE id = ?")
            .bind(DEFAULT_PROFILE_ID)
            .execute(&pool)
            .await
            .unwrap();

        assert!(
            toggle_favorite(&pool, DEFAULT_PROFILE_ID, channel_id)
                .await
                .unwrap()
        );
        let queued_before: Vec<(String, i64)> =
            sqlx::query_as("SELECT item_key, deleted FROM sync_queue WHERE kind = 'favorite'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(queued_before.len(), 1, "one favorite queued for sync");
        assert_eq!(queued_before[0].1, 0, "queued as active");
        let item_key = queued_before[0].0.clone();

        delete_playlist(&pool, playlist_id).await.unwrap();

        let queued_after: Vec<(String, i64)> =
            sqlx::query_as("SELECT item_key, deleted FROM sync_queue WHERE kind = 'favorite'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            queued_after.len(),
            1,
            "the tombstone must update the existing sync record, not add a second one"
        );
        assert_eq!(
            queued_after[0].0, item_key,
            "the tombstone must reuse the favorite's original sync item_key"
        );
        assert_eq!(queued_after[0].1, 1, "queued as removed");
    }
}
