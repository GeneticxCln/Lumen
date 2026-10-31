//! Identity, resume and cross-device-removal regressions for the external audit's issues 6-15.
//! These are the parts that do not change the stored `content_key` derivation; the v2 key migration
//! (issues 6, 7 and 15) is specified in `IDENTITY-MIGRATION.md` and is not enabled yet.

use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};

use crate::db::{
    add_history, create_profile, extract_origin_for_channel, get_resume_position, import_playlist,
    list_channels, list_favorites, merge_profiles, migrate_tmdb_columns, run_migrations,
    toggle_favorite,
};
use crate::playlist::import::{Channel, Playlist};
use crate::sync::{SyncItem, apply_favorite, apply_progress};

const PROFILE: i64 = 1;

async fn test_pool() -> Pool<Sqlite> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    run_migrations(&pool).await.unwrap();
    migrate_tmdb_columns(&pool).await.unwrap();
    pool
}

fn playlist(name: &str, source: &str, origin: Option<&str>) -> Playlist {
    Playlist {
        id: None,
        name: name.to_string(),
        source: source.to_string(),
        origin: origin.map(str::to_string),
        created_at: 0,
        updated_at: 0,
    }
}

fn channel(name: &str, url: &str) -> Channel {
    Channel {
        id: None,
        playlist_id: 0,
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

/// Issue 10 and 11: a resume point saved while one source played must be found when another
/// source for the same title starts, and a title watched to the last few seconds must not resume
/// there and stop immediately.
#[tokio::test]
async fn resume_follows_the_title_across_sources_and_ignores_the_last_seconds() {
    let pool = test_pool().await;

    let mut first = channel("News HD", "https://one.example/live.m3u8");
    first.tvg_id = Some("news.hd".to_string());
    let mut second = channel("News HD backup", "https://two.example/live.m3u8");
    second.tvg_id = Some("news.hd".to_string());

    let playlist_id = import_playlist(
        &pool,
        &playlist("Two sources", "file", None),
        &[first, second],
    )
    .await
    .unwrap();
    let rows = list_channels(&pool, playlist_id, None, None).await.unwrap();
    let id_for = |url: &str| {
        rows.iter()
            .find(|row| row.channel.url == url)
            .and_then(|row| row.channel.id)
            .unwrap()
    };
    let (first_id, second_id) = (
        id_for("https://one.example/live.m3u8"),
        id_for("https://two.example/live.m3u8"),
    );

    add_history(
        &pool,
        PROFILE,
        Some(first_id),
        "https://one.example/live.m3u8",
        42.5,
        120.0,
    )
    .await
    .unwrap();

    assert_eq!(
        get_resume_position(
            &pool,
            PROFILE,
            Some(second_id),
            "https://two.example/live.m3u8"
        )
        .await
        .unwrap(),
        Some(42.5),
        "the other source must find the title's resume point"
    );

    // Within the last five seconds the title is finished, not resumable: seeking there would
    // stop playback immediately.
    add_history(
        &pool,
        PROFILE,
        Some(first_id),
        "https://one.example/live.m3u8",
        117.0,
        120.0,
    )
    .await
    .unwrap();
    assert_eq!(
        get_resume_position(
            &pool,
            PROFILE,
            Some(second_id),
            "https://two.example/live.m3u8"
        )
        .await
        .unwrap(),
        None,
        "a finished title must start from the beginning"
    );
}

/// Issue 8: removing a favorite through one source must remove the title, even though another
/// playlist's channel carries it under the same content key.
#[tokio::test]
async fn unfavoriting_one_source_clears_the_title_everywhere() {
    let pool = test_pool().await;

    let mut first = channel("News HD", "https://one.example/live.m3u8");
    first.tvg_id = Some("news.hd".to_string());
    let mut second = channel("News HD backup", "https://two.example/live.m3u8");
    second.tvg_id = Some("news.hd".to_string());

    let playlist_id = import_playlist(
        &pool,
        &playlist("Two sources", "file", None),
        &[first, second],
    )
    .await
    .unwrap();
    let rows = list_channels(&pool, playlist_id, None, None).await.unwrap();
    let id_for = |url: &str| {
        rows.iter()
            .find(|row| row.channel.url == url)
            .and_then(|row| row.channel.id)
            .unwrap()
    };
    let (first_id, second_id) = (
        id_for("https://one.example/live.m3u8"),
        id_for("https://two.example/live.m3u8"),
    );

    assert!(toggle_favorite(&pool, PROFILE, first_id).await.unwrap());
    // A favorite is a title: My List intentionally reports every source that carries it.
    assert_eq!(
        list_favorites(&pool, PROFILE).await.unwrap(),
        vec![first_id, second_id]
    );

    assert!(
        !toggle_favorite(&pool, PROFILE, second_id).await.unwrap(),
        "toggling the other source turns the shared favorite off"
    );
    assert!(
        list_favorites(&pool, PROFILE).await.unwrap().is_empty(),
        "the title must leave My List through every source"
    );
    let active: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM favorites WHERE profile_id = ? AND active = 1")
            .bind(PROFILE)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(active, 0);
}

/// Issue 9: a removal tombstone must match by content key, not only by the sender's URL, or a
/// device that stores a different URL for the same title keeps it favorited forever.
#[tokio::test]
async fn remote_favorite_tombstone_clears_a_different_url_for_the_same_title() {
    let pool = test_pool().await;
    sqlx::query("UPDATE profiles SET sync_key = 'profile-key' WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO favorites (profile_id, channel_url, content_key, active, updated_at) \
         VALUES (1, 'https://a.example/one.m3u8', 'tvg:same', 1, 1000)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let removal = SyncItem {
        kind: "favorite".into(),
        key: "unused".into(),
        payload: json!({
            "profile": "profile-key",
            "content_key": "tvg:same",
            "url": "https://b.example/two.m3u8",
        }),
        updated_at: 2000,
        deleted: true,
    };
    apply_favorite(&mut conn, &removal).await.unwrap();
    let active: i64 = sqlx::query_scalar("SELECT active FROM favorites WHERE channel_url = ?")
        .bind("https://a.example/one.m3u8")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(active, 0, "the other URL was cleared by the same title key");

    // An older tombstone arriving late must not undo a newer re-favorite.
    sqlx::query("UPDATE favorites SET active = 1, updated_at = 3000 WHERE channel_url = ?")
        .bind("https://a.example/one.m3u8")
        .execute(&mut *conn)
        .await
        .unwrap();
    apply_favorite(&mut conn, &removal).await.unwrap();
    let active: i64 = sqlx::query_scalar("SELECT active FROM favorites WHERE channel_url = ?")
        .bind("https://a.example/one.m3u8")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        active, 1,
        "a stale removal cannot win against a newer favorite"
    );
}

/// Issue 14: an old progress tombstone must not wipe history written after it.
#[tokio::test]
async fn an_old_progress_tombstone_does_not_delete_newer_history() {
    let pool = test_pool().await;
    sqlx::query("UPDATE profiles SET sync_key = 'profile-key' WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO history (profile_id, path, content_key, position, duration, played_at) \
         VALUES (1, 'https://cdn.example/live.m3u8', 'stream:key', 10.0, 100.0, 5000)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let mut removal = SyncItem {
        kind: "progress".into(),
        key: "unused".into(),
        payload: json!({ "profile": "profile-key", "content_key": "stream:key" }),
        // 1_000_000 ms is older than played_at 5000 s (5_000_000 ms).
        updated_at: 1_000_000,
        deleted: true,
    };
    apply_progress(&mut conn, &removal).await.unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM history")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        remaining, 1,
        "an older tombstone must not erase newer progress"
    );

    removal.updated_at = 6_000_000;
    apply_progress(&mut conn, &removal).await.unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM history")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(remaining, 0, "a newer tombstone still deletes the row");
}

/// Issue 13: merging a profile moves its data locally, so it must also be re-queued under the
/// surviving profile's sync key; otherwise the other devices lose it with the deleted profile.
#[tokio::test]
async fn merging_profiles_queues_the_moved_data_under_the_surviving_profile() {
    let pool = test_pool().await;
    let mut channel = channel("News HD", "https://one.example/live.m3u8");
    channel.tvg_id = Some("news.hd".to_string());
    let playlist_id = import_playlist(&pool, &playlist("One", "file", None), &[channel])
        .await
        .unwrap();
    let channel_id = list_channels(&pool, playlist_id, None, None).await.unwrap()[0]
        .channel
        .id
        .unwrap();

    let second = create_profile(&pool, "Second", None).await.unwrap();
    let second_key: String = sqlx::query_scalar("SELECT sync_key FROM profiles WHERE id = ?")
        .bind(second.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let into_key: String = sqlx::query_scalar("SELECT sync_key FROM profiles WHERE id = ?")
        .bind(PROFILE)
        .fetch_one(&pool)
        .await
        .unwrap();

    toggle_favorite(&pool, second.id, channel_id).await.unwrap();
    add_history(
        &pool,
        second.id,
        Some(channel_id),
        "https://one.example/live.m3u8",
        30.0,
        60.0,
    )
    .await
    .unwrap();

    merge_profiles(&pool, PROFILE, second.id).await.unwrap();

    let payloads: Vec<String> =
        sqlx::query_scalar("SELECT payload FROM sync_queue WHERE kind IN ('favorite', 'progress')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        payloads.iter().any(|p| p.contains(&into_key)),
        "the moved data is queued under the surviving profile: {payloads:?}"
    );
    assert!(
        payloads.iter().any(|p| p.contains("tvg:news.hd")),
        "the moved favorite/progress keeps its title key: {payloads:?}"
    );
    let deleted_profiles: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sync_queue WHERE kind = 'profile' AND deleted = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(deleted_profiles, 1);
    assert!(!payloads.iter().any(|p| p.contains(&second_key)));
}

/// Issue 12: harvested entries are already stream URLs; only entries created by the extractor
/// store a page and need yt-dlp at play time.
#[tokio::test]
async fn harvested_entries_play_their_found_stream_instead_of_resolving_the_page() {
    let pool = test_pool().await;
    let harvested = import_playlist(
        &pool,
        &playlist("Harvested", "harvest", Some("https://site.example/page")),
        &[channel("Found", "https://cdn.example/found.m3u8")],
    )
    .await
    .unwrap();
    let harvested_id = list_channels(&pool, harvested, None, None).await.unwrap()[0]
        .channel
        .id
        .unwrap();
    assert_eq!(
        extract_origin_for_channel(&pool, harvested_id)
            .await
            .unwrap(),
        None,
        "a harvested stream must not be re-extracted from the page"
    );

    let extracted = import_playlist(
        &pool,
        &playlist("Extracted", "extract", Some("https://site.example/watch")),
        &[channel("Page", "https://site.example/watch")],
    )
    .await
    .unwrap();
    let extracted_id = list_channels(&pool, extracted, None, None).await.unwrap()[0]
        .channel
        .id
        .unwrap();
    assert_eq!(
        extract_origin_for_channel(&pool, extracted_id)
            .await
            .unwrap()
            .as_deref(),
        Some("https://site.example/watch")
    );
}
