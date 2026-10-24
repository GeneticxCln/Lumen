//! Re-fetches a URL-imported playlist and merges the result into its existing `channels` rows
//! in place, instead of the duplicate-everything result a naive re-import would produce. A
//! channel present in both the old and the fresh list keeps its database id — so a favorite or
//! history row that reaches it through `title_sources`/`content_key` survives a routine refresh
//! untouched — a channel no longer listed is removed (tombstoning any favorite that depended
//! only on it), and a channel new to the fresh list is inserted.
use crate::db;
use crate::error::{PlayerError, Result};
use crate::playlist::import::{Channel, fetch_playlist_text, parse_into_channels};
use chrono::Utc;
use serde::Serialize;
use sqlx::{Pool, QueryBuilder, Row, Sqlite};
use std::collections::{HashMap, HashSet};

/// How many channels a refresh added, updated in place, or removed.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct RefreshStats {
    pub added: i64,
    pub updated: i64,
    pub removed: i64,
}

/// Re-fetches `playlist_id`'s source URL and merges the result into its existing channels,
/// matched by URL. Refuses a playlist that was not imported from a URL in the first place — a
/// file or an extracted source has nothing to re-fetch.
pub async fn refresh_playlist(
    pool: &Pool<Sqlite>,
    playlist_id: i64,
    allow_lan: bool,
) -> Result<RefreshStats> {
    let playlist = db::get_playlist(pool, playlist_id)
        .await?
        .ok_or_else(|| PlayerError::NotFound(format!("Playlist {playlist_id} no longer exists")))?;
    if playlist.source != "url" {
        return Err(PlayerError::InvalidArg(
            "Only a playlist imported from a URL can be refreshed".into(),
        ));
    }
    let origin = playlist.origin.clone().ok_or_else(|| {
        PlayerError::InvalidArg("This playlist has no source URL to refresh from".into())
    })?;

    let content = fetch_playlist_text(&origin, allow_lan).await?;
    let fresh = parse_into_channels(&content, Some(&origin), &origin, &playlist.name)?;

    let existing_rows = sqlx::query("SELECT id, url FROM channels WHERE playlist_id = ?")
        .bind(playlist_id)
        .fetch_all(pool)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    // First match wins on a duplicate URL within the existing set — rare, and no worse than
    // what importing that same playlist fresh would have produced.
    let mut existing_by_url: HashMap<String, i64> = HashMap::new();
    for row in &existing_rows {
        existing_by_url
            .entry(row.get("url"))
            .or_insert_with(|| row.get("id"));
    }

    let mut matched_ids: HashSet<i64> = HashSet::new();
    let mut updates: Vec<(i64, Channel)> = Vec::new();
    let mut inserts: Vec<Channel> = Vec::new();
    for channel in fresh {
        match existing_by_url.get(&channel.url) {
            Some(&id) => {
                matched_ids.insert(id);
                updates.push((id, channel));
            }
            None => inserts.push(channel),
        }
    }

    let removed: Vec<(i64, String)> = existing_rows
        .iter()
        .filter_map(|row| {
            let id: i64 = row.get("id");
            (!matched_ids.contains(&id)).then(|| (id, row.get("url")))
        })
        .collect();

    let now = Utc::now().timestamp_millis();
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    for (id, channel) in &updates {
        let headers_json = channel.extra_headers.as_deref().unwrap_or("{}");
        sqlx::query(
            "UPDATE channels SET name = ?, group_title = ?, tvg_id = ?, tvg_logo = ?, \
             language = ?, extra_headers = ?, is_live = ? WHERE id = ?",
        )
        .bind(&channel.name)
        .bind(&channel.group_title)
        .bind(&channel.tvg_id)
        .bind(&channel.tvg_logo)
        .bind(&channel.language)
        .bind(headers_json)
        .bind(channel.is_live.map(i64::from))
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;
    }

    db::insert_channel_rows(&mut tx, playlist_id, &inserts).await?;

    db::tombstone_favorites_for_removed_channels(&mut tx, pool, &removed, now).await?;
    if !removed.is_empty() {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("DELETE FROM channels WHERE id IN (");
        {
            let mut separated = qb.separated(", ");
            for (id, _) in &removed {
                separated.push_bind(*id);
            }
        }
        qb.push(")");
        qb.build()
            .execute(&mut *tx)
            .await
            .map_err(|e| PlayerError::Sql(e.to_string()))?;
    }

    // Covers every channel row that now exists under this playlist — the just-updated and the
    // newly-inserted ones alike (the removed ones are already gone) — the same upsert-based
    // logic `import_playlist` itself uses, safe to re-run.
    db::backfill_titles_for_playlist(&mut tx, playlist_id).await?;

    sqlx::query("UPDATE playlists SET last_refreshed_at = ? WHERE id = ?")
        .bind(now)
        .bind(playlist_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    let stats = RefreshStats {
        added: inserts.len() as i64,
        updated: updates.len() as i64,
        removed: removed.len() as i64,
    };

    tx.commit()
        .await
        .map_err(|e| PlayerError::Sql(e.to_string()))?;

    // Any orphan tombstones just queued are picked up by the caller's own sync::schedule, the
    // same way delete_playlist_cmd and every other write-path command already does — this
    // library function stays Tauri/AppHandle-agnostic, same as the rest of lumen-core's db layer.
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A one-route HTTP server on loopback serving `body` on every request, counting hits.
    async fn serve(body: String) -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let _ = stream.write_all(body.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (port, hits)
    }

    fn playlist_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: audio/mpegurl\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn test_pool() -> sqlx::Pool<Sqlite> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON;")
            .execute(&pool)
            .await
            .unwrap();
        db::run_migrations(&pool).await.unwrap();
        // backfill_titles_for_playlist reads the TMDB enrichment columns, which init_db adds
        // after migrating, same as db.rs's own tests do.
        db::migrate_tmdb_columns(&pool).await.unwrap();
        pool
    }

    async fn channel_rows(pool: &Pool<Sqlite>, playlist_id: i64) -> Vec<(i64, String, String)> {
        sqlx::query_as("SELECT id, name, url FROM channels WHERE playlist_id = ? ORDER BY id")
            .bind(playlist_id)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn refreshing_a_file_imported_playlist_is_refused() {
        let pool = test_pool().await;
        let playlist = crate::playlist::m3u::Playlist {
            id: None,
            name: "Local".into(),
            source: "file".into(),
            origin: Some("/tmp/does-not-matter.m3u".into()),
            created_at: 0,
            updated_at: 0,
        };
        let playlist_id = db::import_playlist(&pool, &playlist, &[]).await.unwrap();

        let error = refresh_playlist(&pool, playlist_id, false)
            .await
            .expect_err("a file-imported playlist cannot be refreshed");
        assert!(String::from(error).contains("URL"));
    }

    #[tokio::test]
    async fn a_channel_present_in_both_fetches_keeps_its_id_so_its_favorite_survives() {
        let (port, _hits) = serve(playlist_response(
            "#EXTM3U\n#EXTINF:-1,Stays\nhttp://example.test/stays.m3u8\n",
        ))
        .await;
        let url = format!("http://127.0.0.1:{port}/list.m3u");

        let pool = test_pool().await;
        let playlist = crate::playlist::m3u::Playlist {
            id: None,
            name: "Source".into(),
            source: "url".into(),
            origin: Some(url.clone()),
            created_at: 0,
            updated_at: 0,
        };
        let channels = vec![Channel {
            id: None,
            playlist_id: 0,
            name: "Stays".into(),
            url: "http://example.test/stays.m3u8".into(),
            group_title: None,
            tvg_id: None,
            tvg_logo: None,
            language: None,
            extra_headers: None,
            last_ok: None,
            last_error: None,
            is_live: None,
        }];
        let playlist_id = db::import_playlist(&pool, &playlist, &channels)
            .await
            .unwrap();
        let before = channel_rows(&pool, playlist_id).await;
        let (channel_id, _, _) = before[0];

        db::create_profile(&pool, "Default", None).await.unwrap();
        let profile_id: i64 = sqlx::query_scalar("SELECT id FROM profiles LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        db::toggle_favorite(&pool, profile_id, channel_id)
            .await
            .unwrap();

        let stats = refresh_playlist(&pool, playlist_id, true).await.unwrap();
        assert_eq!(
            stats,
            RefreshStats {
                added: 0,
                updated: 1,
                removed: 0
            }
        );

        let after = channel_rows(&pool, playlist_id).await;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].0, channel_id, "the matched channel keeps its id");

        let still_favorite: bool = sqlx::query_scalar(
            "SELECT active FROM favorites WHERE profile_id = ? AND channel_url = ?",
        )
        .bind(profile_id)
        .bind("http://example.test/stays.m3u8")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            still_favorite,
            "the favorite on a still-present channel is untouched"
        );
    }

    #[tokio::test]
    async fn a_channel_dropped_upstream_is_removed_and_its_favorite_is_tombstoned() {
        let (port, _hits) = serve(playlist_response(
            "#EXTM3U\n#EXTINF:-1,New\nhttp://example.test/new.m3u8\n",
        ))
        .await;
        let url = format!("http://127.0.0.1:{port}/list.m3u");

        let pool = test_pool().await;
        let playlist = crate::playlist::m3u::Playlist {
            id: None,
            name: "Source".into(),
            source: "url".into(),
            origin: Some(url.clone()),
            created_at: 0,
            updated_at: 0,
        };
        let channels = vec![Channel {
            id: None,
            playlist_id: 0,
            name: "Gone".into(),
            url: "http://example.test/gone.m3u8".into(),
            group_title: None,
            tvg_id: None,
            tvg_logo: None,
            language: None,
            extra_headers: None,
            last_ok: None,
            last_error: None,
            is_live: None,
        }];
        let playlist_id = db::import_playlist(&pool, &playlist, &channels)
            .await
            .unwrap();

        db::create_profile(&pool, "Default", None).await.unwrap();
        let profile_id: i64 = sqlx::query_scalar("SELECT id FROM profiles LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        let before = channel_rows(&pool, playlist_id).await;
        db::toggle_favorite(&pool, profile_id, before[0].0)
            .await
            .unwrap();

        let stats = refresh_playlist(&pool, playlist_id, true).await.unwrap();
        assert_eq!(
            stats,
            RefreshStats {
                added: 1,
                updated: 0,
                removed: 1
            }
        );

        let after = channel_rows(&pool, playlist_id).await;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].2, "http://example.test/new.m3u8");

        let still_active: bool = sqlx::query_scalar(
            "SELECT active FROM favorites WHERE profile_id = ? AND channel_url = ?",
        )
        .bind(profile_id)
        .bind("http://example.test/gone.m3u8")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            !still_active,
            "a favorite whose only channel disappeared is tombstoned"
        );

        // Profiles are assigned a sync key at creation (see create_profile), so toggling the
        // favorite and then tombstoning it both queue under the same (kind, item_key) — the
        // ON CONFLICT upsert in sync::enqueue collapses them to this one, deleted, row.
        let deleted: i64 =
            sqlx::query_scalar("SELECT deleted FROM sync_queue WHERE kind = 'favorite'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            deleted, 1,
            "the favorite's sync record is a tombstone, not a fresh push"
        );
    }
}
