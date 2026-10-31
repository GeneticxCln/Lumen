//! Migration-level regressions for the v2 content-key change (audit issues 6, 7 and 15).
//!
//! These run on a real file-backed SQLite database, not `:memory:`, so the `VACUUM INTO` backup
//! and `PRAGMA quick_check` verification are exercised too.

use chrono::Utc;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};

use crate::content_key::ContentKey;
use crate::content_migration::{
    CONTENT_KEY_VERSION_SETTING, migrate_content_keys_v2, migrate_inner,
};
use crate::db::{import_playlist, migrate_tmdb_columns, run_migrations};
use crate::playlist::import::{Channel, Playlist};

async fn test_pool(tag: &str) -> (Pool<Sqlite>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "lumen-key-v2-{}-{tag}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lumen.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    run_migrations(&pool).await.unwrap();
    migrate_tmdb_columns(&pool).await.unwrap();
    (pool, dir)
}

fn playlist() -> Playlist {
    Playlist {
        id: None,
        name: "Migration test".into(),
        source: "file".into(),
        origin: None,
        created_at: 0,
        updated_at: 0,
    }
}

fn channel(name: &str, url: &str, tvg_id: Option<&str>) -> Channel {
    Channel {
        id: None,
        playlist_id: 0,
        name: name.into(),
        url: url.into(),
        group_title: None,
        tvg_id: tvg_id.map(str::to_string),
        tvg_logo: None,
        language: None,
        extra_headers: None,
        last_ok: None,
        last_error: None,
        is_live: Some(true),
    }
}

async fn favorite_key(pool: &Pool<Sqlite>, url: &str) -> String {
    sqlx::query_scalar("SELECT content_key FROM favorites WHERE channel_url = ?")
        .bind(url)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn setting(pool: &Pool<Sqlite>, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn v2_migration_rekeys_query_distinct_streams_and_placeholder_tvg_ids() {
    let (pool, dir) = test_pool("rekey").await;
    let url_one = "https://cdn.example/play.php?id=1";
    let url_two = "https://cdn.example/play.php?id=2";
    let url_placeholder = "https://cdn.example/live.m3u8";
    let url_real_tvg = "https://cdn.example/bbc.m3u8";

    import_playlist(
        &pool,
        &playlist(),
        &[
            channel("News one", url_one, None),
            channel("News two", url_two, None),
            channel("Placeholder", url_placeholder, Some("None")),
            channel("BBC One", url_real_tvg, Some("news.hd")),
        ],
    )
    .await
    .unwrap();

    let ids: Vec<(i64, String)> = sqlx::query_as("SELECT id, url FROM channels ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let id_for = |url: &str| ids.iter().find(|(_, u)| u == url).unwrap().0;
    let (one, two, placeholder, _real_tvg) = (
        id_for(url_one),
        id_for(url_two),
        id_for(url_placeholder),
        id_for(url_real_tvg),
    );

    let legacy_one = ContentKey::derive_legacy(None, None, "News one", None, url_one).into_string();
    let legacy_two = ContentKey::derive_legacy(None, None, "News two", None, url_two).into_string();
    assert_eq!(
        legacy_one, legacy_two,
        "the old derivation must reproduce the collision this migration fixes"
    );
    let legacy_placeholder =
        ContentKey::derive_legacy(None, None, "Placeholder", Some("None"), url_placeholder)
            .into_string();
    assert_eq!(legacy_placeholder, "tvg:none");

    // Replace the import's v2 titles/sources with the pre-migration legacy rows.
    sqlx::query("DELETE FROM title_sources")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM titles")
        .execute(&pool)
        .await
        .unwrap();

    for (url, key, active) in [
        (url_one, legacy_one.as_str(), 1),
        (url_two, legacy_two.as_str(), 1),
        (url_placeholder, legacy_placeholder.as_str(), 1),
        (url_real_tvg, "tvg:news.hd", 1),
    ] {
        sqlx::query(
            "INSERT INTO favorites (profile_id, channel_url, active, updated_at, content_key) \
             VALUES (1, ?, ?, 1000, ?)",
        )
        .bind(url)
        .bind(active)
        .bind(key)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (channel_id, url, key) in [
        (one, url_one, legacy_one.as_str()),
        (two, url_two, legacy_two.as_str()),
    ] {
        sqlx::query(
            "INSERT INTO history \
             (profile_id, channel_id, path, position, duration, played_at, content_key) \
             VALUES (1, ?, ?, 42.0, 3600.0, 5000, ?)",
        )
        .bind(channel_id)
        .bind(url)
        .bind(key)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO titles (content_key, kind, title, created_at, updated_at) \
         VALUES (?, 'channel', 'News', 0, 0)",
    )
    .bind(&legacy_one)
    .execute(&pool)
    .await
    .unwrap();
    for (channel_id, url) in [(one, url_one), (two, url_two)] {
        sqlx::query(
            "INSERT INTO title_sources (content_key, channel_id, stream_url, rank, created_at) \
             VALUES (?, ?, ?, 0, 0)",
        )
        .bind(&legacy_one)
        .bind(channel_id)
        .bind(url)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO titles (content_key, kind, title, created_at, updated_at) \
         VALUES (?, 'channel', 'Placeholder', 0, 0)",
    )
    .bind(&legacy_placeholder)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO title_sources (content_key, channel_id, stream_url, rank, created_at) \
         VALUES (?, ?, ?, 0, 0)",
    )
    .bind(&legacy_placeholder)
    .bind(placeholder)
    .bind(url_placeholder)
    .execute(&pool)
    .await
    .unwrap();

    // Pause after one batch, as a crash would, then resume.
    let completed = migrate_inner(&pool, 1, Some(1)).await.unwrap();
    assert!(!completed, "one batch must leave the migration resumable");
    let cursor = setting(&pool, "content_key_migration_cursor").await;
    assert!(cursor.is_some_and(|value| value.parse::<i64>().unwrap() > 0));
    assert_eq!(setting(&pool, CONTENT_KEY_VERSION_SETTING).await, None);

    assert!(migrate_content_keys_v2(&pool).await.unwrap());
    assert_eq!(
        setting(&pool, CONTENT_KEY_VERSION_SETTING).await.as_deref(),
        Some("2")
    );
    assert_eq!(setting(&pool, "content_key_migration_cursor").await, None);

    let one_key = favorite_key(&pool, url_one).await;
    let two_key = favorite_key(&pool, url_two).await;
    assert_eq!(
        one_key,
        ContentKey::derive(None, None, "", None, url_one).into_string()
    );
    assert_eq!(
        two_key,
        ContentKey::derive(None, None, "", None, url_two).into_string()
    );
    assert_ne!(
        one_key, two_key,
        "the two query-distinct streams must split"
    );
    assert!(one_key.starts_with("stream2:"));
    assert_ne!(favorite_key(&pool, url_placeholder).await, "tvg:none");
    assert_eq!(favorite_key(&pool, url_real_tvg).await, "tvg:news.hd");

    let history_keys: Vec<String> =
        sqlx::query_scalar("SELECT content_key FROM history ORDER BY path")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(history_keys.len(), 2);
    assert!(history_keys.iter().all(|key| key.starts_with("stream2:")));
    assert_ne!(history_keys[0], history_keys[1]);

    let legacy_sources: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM title_sources \
         WHERE content_key LIKE 'stream:%' OR lower(content_key) = 'tvg:none'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(legacy_sources, 0);
    let legacy_titles: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM titles \
         WHERE content_key LIKE 'stream:%' OR lower(content_key) = 'tvg:none'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(legacy_titles, 0);

    let channel_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(&pool)
        .await
        .unwrap();
    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_sources")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        source_count, channel_count,
        "every channel must have exactly its own v2 source after the rebuild"
    );

    let check: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(check, "ok");

    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".pre-key-v2-"))
        .collect();
    assert_eq!(
        backups.len(),
        1,
        "the migration must leave a rollback backup"
    );

    // Idempotent: a second run must not move anything or add another backup.
    assert!(migrate_content_keys_v2(&pool).await.unwrap());
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".pre-key-v2-"))
        .collect();
    assert_eq!(backups.len(), 1);

    pool.close().await;
    let _ = std::fs::remove_dir_all(dir);
}

/// Opt-in real-database run. Point `LUMEN_KEY_MIGRATION_DB_COPY` at a *copy* (never the live
/// file) and run:
/// `LUMEN_KEY_MIGRATION_DB_COPY=/tmp/lumen.db cargo test -p lumen-core --release \
///  migrates_a_real_database_copy -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set LUMEN_KEY_MIGRATION_DB_COPY to a database copy"]
async fn migrates_a_real_database_copy() {
    let path = std::env::var("LUMEN_KEY_MIGRATION_DB_COPY")
        .expect("set LUMEN_KEY_MIGRATION_DB_COPY to a database copy");
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect(&format!("sqlite://{path}?mode=rwc"))
        .await
        .unwrap();

    let channels: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(&pool)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    assert!(migrate_content_keys_v2(&pool).await.unwrap());
    let elapsed = started.elapsed();

    let version = setting(&pool, CONTENT_KEY_VERSION_SETTING).await;
    let cursor = setting(&pool, "content_key_migration_cursor").await;
    let mut legacy_remaining = 0i64;
    for table in ["favorites", "history", "title_sources"] {
        let remaining: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {table} WHERE content_key IS NULL OR content_key = '' \
             OR content_key LIKE 'stream:%' OR lower(content_key) IN \
             ('tvg:none','tvg:null','tvg:nil','tvg:n/a','tvg:na','tvg:-','tvg:unknown','tvg:undefined')"
        )))
        .fetch_one(&pool)
        .await
        .unwrap();
        legacy_remaining += remaining;
    }
    let check: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&pool)
        .await
        .unwrap();
    println!(
        "real copy: {channels} channels, {:?}, version={version:?}, cursor={cursor:?}, legacy={legacy_remaining}, quick_check={check}",
        elapsed
    );

    assert_eq!(version.as_deref(), Some("2"));
    assert_eq!(cursor, None);
    assert_eq!(legacy_remaining, 0);
    assert_eq!(check, "ok");
    pool.close().await;
}

/// Opt-in library-scale measurement. Point `LUMEN_PAGE_TEST_DB_COPY` at a copy of a large
/// database and run:
/// `LUMEN_PAGE_TEST_DB_COPY=/tmp/lumen.db cargo test -p lumen-core --release \
///  pages_a_real_database_copy -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set LUMEN_PAGE_TEST_DB_COPY to a database copy"]
async fn pages_a_real_database_copy() {
    let path = std::env::var("LUMEN_PAGE_TEST_DB_COPY")
        .expect("set LUMEN_PAGE_TEST_DB_COPY to a database copy");
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect(&format!("sqlite://{path}?mode=rwc"))
        .await
        .unwrap();
    // Applies migration 014 (the FTS table/triggers) to the old snapshot; it does not start the
    // long index backfill, so the search below exercises the LIKE fallback.
    run_migrations(&pool).await.unwrap();
    migrate_tmdb_columns(&pool).await.unwrap();

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(&pool)
        .await
        .unwrap();
    let first_started = std::time::Instant::now();
    let first = crate::db::list_channels_page(&pool, None, None, None, None, None, 500)
        .await
        .unwrap();
    let first_elapsed = first_started.elapsed();
    assert_eq!(first.channels.len(), 500);
    assert!(first.has_more);
    let name_cursor = first
        .next_after_name
        .clone()
        .expect("a full page has a name cursor");
    let id_cursor = first.next_after_id.expect("a full page has an id cursor");

    let second_started = std::time::Instant::now();
    let second = crate::db::list_channels_page(
        &pool,
        None,
        None,
        None,
        Some(&name_cursor),
        Some(id_cursor),
        500,
    )
    .await
    .unwrap();
    let second_elapsed = second_started.elapsed();
    assert_eq!(second.channels.len(), 500);
    assert!(
        second
            .channels
            .iter()
            .all(|channel| channel.channel.name.as_str() >= name_cursor.as_str()),
        "keyset pages must move forward by name"
    );

    let groups_started = std::time::Instant::now();
    let groups = crate::db::list_channel_groups(&pool, None).await.unwrap();
    let groups_elapsed = groups_started.elapsed();
    assert!(!groups.is_empty());

    let search_started = std::time::Instant::now();
    let search = crate::db::list_channels_page(&pool, None, Some("News"), None, None, None, 500)
        .await
        .unwrap();
    let search_elapsed = search_started.elapsed();

    println!(
        "real pages: {total} channels, first={first_elapsed:?}, second={second_elapsed:?}, \
         groups={groups_elapsed:?} ({} groups), LIKE search={search_elapsed:?} ({} rows)",
        groups.len(),
        search.channels.len()
    );
    pool.close().await;
}

#[tokio::test]
async fn v2_migration_queues_tombstones_when_sync_has_been_used() {
    let (pool, dir) = test_pool("sync").await;
    let url = "https://cdn.example/play.php?id=9";
    import_playlist(&pool, &playlist(), &[channel("Nine", url, None)])
        .await
        .unwrap();
    let channel_id: i64 = sqlx::query_scalar("SELECT id FROM channels")
        .fetch_one(&pool)
        .await
        .unwrap();
    let legacy = ContentKey::derive_legacy(None, None, "Nine", None, url).into_string();
    sqlx::query(
        "INSERT INTO favorites (profile_id, channel_url, active, updated_at, content_key) \
         VALUES (1, ?, 1, 1000, ?)",
    )
    .bind(url)
    .bind(&legacy)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO history \
         (profile_id, channel_id, path, position, duration, played_at, content_key) \
         VALUES (1, ?, ?, 120.0, 3600.0, 7000, ?)",
    )
    .bind(channel_id)
    .bind(url)
    .bind(&legacy)
    .execute(&pool)
    .await
    .unwrap();
    // A device that has synced before, so the old identity exists remotely.
    sqlx::query("INSERT INTO settings (key, value) VALUES ('sync_last_synced_at', '1')")
        .execute(&pool)
        .await
        .unwrap();

    assert!(migrate_content_keys_v2(&pool).await.unwrap());

    let v2 = ContentKey::derive(None, None, "", None, url).into_string();
    let tombstoned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sync_queue WHERE kind = 'favorite' AND deleted = 1 \
         AND payload LIKE ?",
    )
    .bind(format!("%\"content_key\":\"{legacy}\"%"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(tombstoned, 1, "the old favorite identity must be retired");
    let pushed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sync_queue WHERE kind = 'favorite' AND deleted = 0 \
         AND payload LIKE ?",
    )
    .bind(format!("%\"content_key\":\"{v2}\"%"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pushed, 1, "the new favorite identity must be pushed");

    pool.close().await;
    let _ = std::fs::remove_dir_all(dir);
}
