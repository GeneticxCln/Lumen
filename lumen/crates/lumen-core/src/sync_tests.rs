//! Sync transport tests: real queue, real merge code and real sealing, against a small in-process
//! stand-in for the Worker (`sync-server/src/worker.ts`) that speaks the same HTTP API.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::db::{migrate_tmdb_columns, set_setting};
use crate::sync::{Remote, queue_favorite, sync_with};
use crate::sync_crypto::{SealedItem, SyncKeys, generate_root, pow_valid};
use crate::sync_keys::{self, MemoryStore};

const POW_BITS: u32 = 8;
const PAGE: usize = 2;

struct Group {
    verifier: String,
    /// item id -> (updated_at, deleted, blob, seq)
    items: BTreeMap<String, (i64, bool, String, i64)>,
    /// Mirrors the Worker's own `sync_seq` counter: bumped once per push that lands at least one
    /// write, independent of the client-supplied `updated_at` - see `worker.ts`'s `pushItems`.
    next_seq: i64,
}

#[derive(Default)]
struct Mock {
    groups: HashMap<String, Group>,
}

type Shared = Arc<Mutex<Mock>>;

fn reply(status: u16, body: Value) -> Vec<u8> {
    let body = body.to_string();
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn handle_request(
    mock: &mut Mock,
    method: &str,
    target: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> Vec<u8> {
    let url = url::Url::parse(&format!("http://mock{target}")).unwrap();
    let json_body: Value = serde_json::from_slice(body).unwrap_or(Value::Null);

    if method == "POST" && url.path() == "/v1/groups" {
        let sync_id = json_body["sync_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let verifier = json_body["verifier"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if let Some(existing) = mock.groups.get(&sync_id) {
            return if existing.verifier == verifier {
                reply(200, json!({ "created": false }))
            } else {
                reply(409, json!({ "error": "conflict" }))
            };
        }
        let nonce = json_body["pow_nonce"].as_u64().unwrap_or(u64::MAX);
        if !pow_valid(&sync_id, nonce, POW_BITS) {
            return reply(400, json!({ "error": "invalid_pow" }));
        }
        mock.groups.insert(
            sync_id,
            Group {
                verifier,
                items: BTreeMap::new(),
                next_seq: 0,
            },
        );
        return reply(201, json!({ "created": true }));
    }

    // Everything else needs a group and its token.
    let sync_id = headers.get("x-sync-id").cloned().unwrap_or_default();
    let token = headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    let Some(group) = mock.groups.get_mut(&sync_id) else {
        return reply(404, json!({ "error": "group_not_found" }));
    };
    if format!("{:x}", Sha256::digest(token.as_bytes())) != group.verifier {
        return reply(401, json!({ "error": "unauthorized" }));
    }

    match (method, url.path()) {
        ("DELETE", "/v1/groups") => {
            mock.groups.remove(&sync_id);
            reply(200, json!({ "ok": true }))
        }
        ("POST", "/v1/items") => {
            let items = json_body["items"].as_array().cloned().unwrap_or_default();
            if !items.is_empty() {
                group.next_seq += 1;
            }
            let seq = group.next_seq;
            for item in &items {
                let id = item["id"].as_str().unwrap().to_string();
                let updated_at = item["updated_at"].as_i64().unwrap();
                let deleted = item["deleted"].as_bool().unwrap();
                let blob = item["blob"].as_str().unwrap().to_string();
                // Last write wins, as in the Worker; `seq` advances whenever a write actually
                // lands, whether that's a brand-new item or a genuinely newer update to one.
                if group.items.get(&id).is_none_or(|held| updated_at > held.0) {
                    group.items.insert(id, (updated_at, deleted, blob, seq));
                }
            }
            reply(200, json!({ "ok": true, "received": items.len() }))
        }
        ("GET", "/v1/items") => {
            let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
            let after = query.get("after_id");
            // `since_seq`: the real client's current, server-assigned-sequence cursor (see
            // `worker.ts`'s `pullItems`). A bare `since`: the legacy, `updated_at`-based path,
            // kept here only so a test could still exercise it; nothing in this test file sends
            // it on purpose anymore.
            if let Some(since_seq) = query
                .get("since_seq")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|v| *v >= 0)
            {
                let mut rows: Vec<_> = group
                    .items
                    .iter()
                    .filter(|(id, (_, _, _, seq))| match after {
                        Some(after) => {
                            *seq > since_seq || (*seq == since_seq && id.as_str() > after.as_str())
                        }
                        None => *seq > since_seq,
                    })
                    .map(|(id, (at, deleted, blob, seq))| {
                        (*seq, id.clone(), *at, *deleted, blob.clone())
                    })
                    .collect();
                rows.sort();
                let has_more = rows.len() > PAGE;
                let items: Vec<_> = rows
                    .into_iter()
                    .take(PAGE)
                    .map(|(seq, id, at, deleted, blob)| {
                        json!({ "id": id, "updated_at": at, "deleted": deleted, "blob": blob, "seq": seq })
                    })
                    .collect();
                return reply(200, json!({ "items": items, "has_more": has_more }));
            }
            // The Worker refuses a missing, non-numeric or negative cursor; so does this stand-in.
            let Some(since) = query
                .get("since")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|v| *v >= 0)
            else {
                return reply(400, json!({ "error": "invalid_request" }));
            };
            let mut rows: Vec<_> = group
                .items
                .iter()
                .filter(|(id, (at, _, _, _))| match after {
                    Some(after) => *at > since || (*at == since && id.as_str() > after.as_str()),
                    None => *at > since,
                })
                .map(|(id, (at, deleted, blob, _))| (*at, id.clone(), *deleted, blob.clone()))
                .collect();
            rows.sort();
            let has_more = rows.len() > PAGE;
            let items: Vec<_> = rows
                .into_iter()
                .take(PAGE)
                .map(|(at, id, deleted, blob)| json!({ "id": id, "updated_at": at, "deleted": deleted, "blob": blob }))
                .collect();
            reply(200, json!({ "items": items, "has_more": has_more }))
        }
        _ => reply(404, json!({ "error": "not_found" })),
    }
}

async fn handle(socket: &mut TcpStream, mock: &Shared) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let mut lines = head.lines();
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let (method, target) = (
        request_line.next().unwrap_or_default(),
        request_line.next().unwrap_or_default(),
    );
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while buffer.len() < head_end + length {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    let response = {
        let mut guard = mock.lock().unwrap();
        handle_request(&mut guard, method, target, &headers, &buffer[head_end..])
    };
    socket.write_all(&response).await?;
    socket.shutdown().await
}

async fn start_mock() -> (String, Shared) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mock: Shared = Arc::default();
    let served = mock.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let served = served.clone();
            tokio::spawn(async move {
                let _ = handle(&mut socket, &served).await;
            });
        }
    });
    (origin, mock)
}

/// A device: its own database, with the default profile given a shared sync key so profiles line
/// up across devices the way they do after a first sync.
async fn device() -> Pool<Sqlite> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON;")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    migrate_tmdb_columns(&pool).await.unwrap();
    sqlx::query("UPDATE profiles SET sync_key = 'shared-profile' WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

fn remote(origin: &str, root: &[u8; 32]) -> Remote {
    Remote::new(origin, SyncKeys::from_root(root)).unwrap()
}

async fn favorite_rows(pool: &Pool<Sqlite>) -> Vec<(String, i64)> {
    sqlx::query_as("SELECT channel_url, active FROM favorites ORDER BY channel_url")
        .fetch_all(pool)
        .await
        .unwrap()
}

const URLS: [&str; 3] = [
    "https://cdn.example/movie-a/index.m3u8",
    "https://cdn.example/movie-b/index.m3u8",
    "https://cdn.example/movie-c/index.m3u8",
];

async fn add_favorites(pool: &Pool<Sqlite>, updated_at: i64) {
    for (index, url) in URLS.iter().enumerate() {
        sqlx::query("INSERT INTO favorites (profile_id, channel_url, content_key, active, updated_at) VALUES (1, ?, ?, 1, ?)")
            .bind(url)
            .bind(format!("tmdb:movie:{}", 500 + index))
            .bind(updated_at)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn two_devices_with_one_key_exchange_everything_and_the_server_sees_none_of_it() {
    let (origin, mock) = start_mock().await;
    let root = generate_root().unwrap();
    remote(&origin, &root).register(POW_BITS).await.unwrap();

    let a = device().await;
    add_favorites(&a, 5_000).await; // three rows share one timestamp: the pages cut through them
    sqlx::query("INSERT INTO history (profile_id, path, content_key, position, duration, played_at) VALUES (1, ?, 'tmdb:movie:500', 120.0, 7200.0, 4000)")
        .bind("https://cdn.example/movie-a/index.m3u8")
        .execute(&a)
        .await
        .unwrap();
    sqlx::query("UPDATE profiles SET name = 'Quinn' WHERE id = 1")
        .execute(&a)
        .await
        .unwrap();
    // Newer than device B's untouched default profile, so last-write-wins picks the rename.
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query("UPDATE profiles SET updated_at = ? WHERE id = 1")
        .bind(now + 1_000)
        .execute(&a)
        .await
        .unwrap();

    let status = sync_with(&a, &remote(&origin, &root)).await.unwrap();
    assert_eq!(status.pending, 0);
    assert_eq!(status.skipped, 0);

    let b = device().await;
    let status = sync_with(&b, &remote(&origin, &root)).await.unwrap();
    assert_eq!(status.skipped, 0);

    let favorites = favorite_rows(&b).await;
    assert_eq!(
        favorites,
        URLS.iter()
            .map(|url| (url.to_string(), 1))
            .collect::<Vec<_>>(),
        "all rows that share a timestamp arrive, across page boundaries"
    );
    let position: f64 = sqlx::query_scalar("SELECT position FROM history WHERE path = ?")
        .bind(URLS[0])
        .fetch_one(&b)
        .await
        .unwrap();
    assert_eq!(position, 120.0);
    let name: String =
        sqlx::query_scalar("SELECT name FROM profiles WHERE sync_key = 'shared-profile'")
            .fetch_one(&b)
            .await
            .unwrap();
    assert_eq!(name, "Quinn");

    // What the server holds: no URL, no content key, no profile name, no field names.
    let stored = {
        let guard = mock.lock().unwrap();
        let group = guard.groups.values().next().unwrap();
        serde_json::to_string(&group.items).unwrap()
    };
    for secret in [
        "cdn.example",
        "movie-a",
        "tmdb:movie",
        "Quinn",
        "favorite",
        "progress",
        "content_key",
        "shared-profile",
    ] {
        assert!(
            !stored.contains(secret),
            "the server must not see {secret:?}: {stored}"
        );
    }

    // A removal on one device reaches the other.
    let mut conn = a.acquire().await.unwrap();
    let removed_at = now + 10 * 60 * 1000;
    queue_favorite(&mut conn, 1, "tmdb:movie:500", URLS[0], false, removed_at)
        .await
        .unwrap();
    drop(conn);
    sqlx::query("UPDATE favorites SET active = 0, updated_at = ? WHERE channel_url = ?")
        .bind(removed_at)
        .bind(URLS[0])
        .execute(&a)
        .await
        .unwrap();
    sync_with(&a, &remote(&origin, &root)).await.unwrap();
    sync_with(&b, &remote(&origin, &root)).await.unwrap();
    let favorites = favorite_rows(&b).await;
    assert_eq!(
        favorites[0],
        (URLS[0].to_string(), 0),
        "the removal is applied"
    );
    assert_eq!(favorites[1].1, 1);
}

#[tokio::test]
async fn a_different_key_sees_nothing_and_cannot_use_the_group() {
    let (origin, _mock) = start_mock().await;
    let root = generate_root().unwrap();
    remote(&origin, &root).register(POW_BITS).await.unwrap();
    let a = device().await;
    add_favorites(&a, 5_000).await;
    sync_with(&a, &remote(&origin, &root)).await.unwrap();

    // Another key has its own group, and it is empty.
    let other = generate_root().unwrap();
    remote(&origin, &other).register(POW_BITS).await.unwrap();
    let c = device().await;
    sync_with(&c, &remote(&origin, &other)).await.unwrap();
    assert!(favorite_rows(&c).await.is_empty());

    // A key with no group is told so, and nothing is created for it.
    let stranger = remote(&origin, &generate_root().unwrap());
    assert!(!stranger.exists().await.unwrap());
    let d = device().await;
    let error = String::from(sync_with(&d, &stranger).await.unwrap_err());
    assert!(error.contains("deleted or has expired"), "{error}");
    assert!(
        !stranger.exists().await.unwrap(),
        "a failed sync must not create the group"
    );

    // A server that no longer accepts the key (here: its stored verifier changed) says so, in words
    // that tell the user what to do.
    {
        let mut guard = _mock.lock().unwrap();
        let group = guard
            .groups
            .get_mut(&SyncKeys::from_root(&root).sync_id())
            .unwrap();
        group.verifier = "0".repeat(64);
    }
    let e = device().await;
    let error = String::from(sync_with(&e, &remote(&origin, &root)).await.unwrap_err());
    assert!(error.contains("no longer accepts this sync key"), "{error}");
}

#[tokio::test]
async fn a_record_the_server_altered_is_skipped_and_counted_not_applied() {
    let (origin, mock) = start_mock().await;
    let root = generate_root().unwrap();
    remote(&origin, &root).register(POW_BITS).await.unwrap();
    let a = device().await;
    add_favorites(&a, 5_000).await;
    sync_with(&a, &remote(&origin, &root)).await.unwrap();

    // Flip one character of one stored favorite, as a malicious or faulty server might.
    {
        let keys = SyncKeys::from_root(&root);
        let mut guard = mock.lock().unwrap();
        let group = guard.groups.values_mut().next().unwrap();
        let target = group
            .items
            .iter()
            .find(|(id, (updated_at, deleted, blob, _))| {
                let sealed = SealedItem {
                    id: (*id).clone(),
                    updated_at: *updated_at,
                    deleted: *deleted,
                    blob: blob.clone(),
                    seq: None,
                };
                keys.open(&sealed).is_ok_and(|item| item.kind == "favorite")
            })
            .map(|(id, _)| id.clone())
            .unwrap();
        let blob = &mut group.items.get_mut(&target).unwrap().2;
        let last = blob.pop().unwrap();
        blob.push(if last == 'A' { 'B' } else { 'A' });
    }

    let b = device().await;
    let status = sync_with(&b, &remote(&origin, &root)).await.unwrap();
    assert_eq!(
        status.skipped, 1,
        "exactly the altered record fails verification"
    );
    assert_eq!(
        favorite_rows(&b).await.len(),
        URLS.len() - 1,
        "the other favorites still arrive: the pull carries on past the bad record"
    );
    // The count is kept so the UI can warn about it.
    let status = crate::sync::status(&b).await.unwrap();
    assert_eq!(status.skipped, 1);
}

/// The bug this file's `seq` handling exists to fix: a device joining a group late re-uploads
/// its own pre-existing data (first-sync seeding, via `queue_all`) under that data's real, old
/// `updated_at`. A cursor that had already advanced past that timestamp - exactly what device
/// A's has done below, from its own earlier, newer sync - must still see it.
#[tokio::test]
async fn a_device_joining_late_is_not_invisible_to_a_cursor_already_past_its_old_data() {
    let (origin, _mock) = start_mock().await;
    let root = generate_root().unwrap();
    remote(&origin, &root).register(POW_BITS).await.unwrap();

    // Device A syncs first. Its pull cursor now sits at the seq this push landed at.
    let a = device().await;
    add_favorites(&a, 9_000_000).await;
    sync_with(&a, &remote(&origin, &root)).await.unwrap();

    // Device B joins the same group later, but its own favorite predates when A's cursor already
    // advanced - exactly what a device holds if it starred something long before ever turning
    // sync on. Its first-ever sync seeds and pushes it under that old timestamp.
    let b = device().await;
    sqlx::query(
        "INSERT INTO favorites (profile_id, channel_url, content_key, active, updated_at) VALUES (1, ?, ?, 1, ?)",
    )
    .bind("https://cdn.example/old-favorite/index.m3u8")
    .bind("tmdb:movie:999")
    .bind(1_000) // long before device A's cursor position above
    .execute(&b)
    .await
    .unwrap();
    sync_with(&b, &remote(&origin, &root)).await.unwrap();

    // Device A syncs again. A timestamp-only cursor would never see this record (1_000 predates
    // everything A's cursor already passed); a seq cursor does, because seq only ever moves
    // forward with each push, independent of what `updated_at` says.
    sync_with(&a, &remote(&origin, &root)).await.unwrap();
    let favorites = favorite_rows(&a).await;
    assert!(
        favorites
            .iter()
            .any(|(url, _)| url == "https://cdn.example/old-favorite/index.m3u8"),
        "device A should receive device B's pre-existing, old-timestamped favorite: {favorites:?}"
    );
}

#[tokio::test]
async fn sync_keys_enable_join_show_and_disable() {
    let (origin, mock) = start_mock().await;

    let first = device().await;
    set_setting(&first, "sync_server_url", &origin)
        .await
        .unwrap();
    let first_store = MemoryStore::default();
    assert!(
        !sync_keys::state_with(&first, &first_store)
            .await
            .unwrap()
            .enabled
    );

    let key = sync_keys::enable_with(&first, &first_store, POW_BITS)
        .await
        .unwrap();
    assert!(
        sync_keys::state_with(&first, &first_store)
            .await
            .unwrap()
            .enabled
    );
    assert_eq!(sync_keys::show_key_with(&first_store).unwrap(), key);
    assert!(
        sync_keys::enable_with(&first, &first_store, POW_BITS)
            .await
            .is_err(),
        "already on"
    );
    assert!(
        sync_keys::set_server_with(&first, &first_store, "https://elsewhere.example")
            .await
            .is_err(),
        "the server cannot change under a live key"
    );
    assert_eq!(mock.lock().unwrap().groups.len(), 1);

    // Another device joins with the key text, typed in lowercase without the dashes.
    let second = device().await;
    set_setting(&second, "sync_server_url", &origin)
        .await
        .unwrap();
    let second_store = MemoryStore::default();
    let typed = key.replace('-', "").to_lowercase();
    sync_keys::join_with(&second, &second_store, &typed)
        .await
        .unwrap();
    assert_eq!(sync_keys::show_key_with(&second_store).unwrap(), key);

    // A well-formed key nobody has registered does not create a group.
    let third = device().await;
    set_setting(&third, "sync_server_url", &origin)
        .await
        .unwrap();
    let third_store = MemoryStore::default();
    let unknown = crate::sync_crypto::format_key(&generate_root().unwrap());
    let error = String::from(
        sync_keys::join_with(&third, &third_store, &unknown)
            .await
            .unwrap_err(),
    );
    assert!(error.contains("No synced data"), "{error}");
    assert!(
        !sync_keys::state_with(&third, &third_store)
            .await
            .unwrap()
            .enabled
    );
    assert_eq!(
        mock.lock().unwrap().groups.len(),
        1,
        "joining never creates a group"
    );

    // A mistyped key is refused before any network call.
    let mut typo = key.clone();
    typo.replace_range(0..1, if key.starts_with('1') { "2" } else { "1" });
    assert!(
        sync_keys::join_with(&third, &third_store, &typo)
            .await
            .is_err()
    );

    // Turning off with deletion erases the server copy for everyone and forgets the key here.
    add_favorites(&first, 5_000).await;
    let remote = sync_keys::remote(&first, &first_store)
        .await
        .unwrap()
        .unwrap();
    sync_with(&first, &remote).await.unwrap();
    sync_keys::disable_with(&first, &first_store, true)
        .await
        .unwrap();
    assert!(
        !sync_keys::state_with(&first, &first_store)
            .await
            .unwrap()
            .enabled
    );
    assert!(mock.lock().unwrap().groups.is_empty());
    assert_eq!(favorite_rows(&first).await.len(), 3, "local data is kept");
    let error = String::from(
        sync_with(
            &second,
            &sync_keys::remote(&second, &second_store)
                .await
                .unwrap()
                .unwrap(),
        )
        .await
        .unwrap_err(),
    );
    assert!(error.contains("deleted or has expired"), "{error}");

    // Without deletion the server copy stays.
    let again = sync_keys::enable_with(&first, &first_store, POW_BITS)
        .await
        .unwrap();
    assert_ne!(again, key, "a new key every time");
    sync_keys::disable_with(&first, &first_store, false)
        .await
        .unwrap();
    assert_eq!(mock.lock().unwrap().groups.len(), 1);
}

#[tokio::test]
async fn enabling_without_a_reachable_server_keeps_no_key() {
    let pool = device().await;
    // Nothing listens on the discard port.
    set_setting(&pool, "sync_server_url", "http://127.0.0.1:9")
        .await
        .unwrap();
    let store = MemoryStore::default();
    assert!(
        sync_keys::enable_with(&pool, &store, POW_BITS)
            .await
            .is_err()
    );
    assert!(
        !sync_keys::state_with(&pool, &store).await.unwrap().enabled,
        "a failed attempt leaves nothing behind"
    );
}

/// Two clients through the real Worker (`npm run dev` in `sync-server/`), not the in-process
/// stand-in. Ignored by default because it needs a running server:
/// `SYNC_TEST_URL=http://127.0.0.1:8787 cargo test -p lumen-core live_ -- --ignored`.
/// The proof-of-work difficulty must match the server's `POW_BITS` (`SYNC_TEST_POW_BITS`, default 8,
/// which is what `npm run dev` uses).
#[tokio::test]
#[ignore = "needs a running sync server; see the doc comment"]
async fn live_two_clients_through_the_real_worker() {
    let origin = std::env::var("SYNC_TEST_URL")
        .expect("set SYNC_TEST_URL to the running Worker, e.g. http://127.0.0.1:8787");
    let bits: u32 = std::env::var("SYNC_TEST_POW_BITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);

    let root = generate_root().unwrap();
    let live = remote(&origin, &root);
    live.register(bits).await.unwrap();
    live.register(bits)
        .await
        .expect("registering the same group again is accepted");

    let a = device().await;
    add_favorites(&a, 5_000).await;
    sqlx::query("UPDATE profiles SET name = 'Quinn' WHERE id = 1")
        .execute(&a)
        .await
        .unwrap();
    sqlx::query("UPDATE profiles SET updated_at = ? WHERE id = 1")
        .bind(chrono::Utc::now().timestamp_millis() + 1_000)
        .execute(&a)
        .await
        .unwrap();
    assert_eq!(sync_with(&a, &live).await.unwrap().skipped, 0);

    let b = device().await;
    assert_eq!(sync_with(&b, &live).await.unwrap().skipped, 0);
    assert_eq!(favorite_rows(&b).await.len(), URLS.len());

    // The bytes the Worker returns are the bytes it stores. None of them mention what was synced.
    let keys = SyncKeys::from_root(&root);
    let raw = reqwest::Client::new()
        .get(format!("{origin}/v1/items?since=0"))
        .header("X-Sync-Id", keys.sync_id())
        .bearer_auth(keys.auth_token())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        raw.contains("\"blob\""),
        "the server returned records: {raw}"
    );
    for secret in [
        "cdn.example",
        "tmdb:movie",
        "Quinn",
        "favorite",
        "content_key",
    ] {
        assert!(
            !raw.contains(secret),
            "the live server must not see {secret:?}"
        );
    }

    // A browser request is refused outright, and the right group with a wrong secret is not served.
    let browser = reqwest::Client::new()
        .get(format!("{origin}/v1/items?since=0"))
        .header("Origin", "https://evil.example")
        .header("X-Sync-Id", keys.sync_id())
        .bearer_auth(keys.auth_token())
        .send()
        .await
        .unwrap();
    assert_eq!(browser.status(), 403);
    let wrong = reqwest::Client::new()
        .get(format!("{origin}/v1/items?since=0"))
        .header("X-Sync-Id", keys.sync_id())
        .bearer_auth("not-the-token")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    live.delete_group().await.unwrap();
    assert!(
        !live.exists().await.unwrap(),
        "the group is gone after deletion"
    );
    live.delete_group()
        .await
        .expect("deleting a missing group is not an error");
}

#[tokio::test]
async fn hand_set_watched_marks_sync_between_devices_and_the_newer_one_wins() {
    let (origin, mock) = start_mock().await;
    let root = generate_root().unwrap();
    remote(&origin, &root).register(POW_BITS).await.unwrap();
    let a = device().await;
    let b = device().await;

    let now = chrono::Utc::now().timestamp_millis();
    crate::series::set_episodes_watched(&a, 1, "tmdb:tv:42", 1, &[1, 2], true, now)
        .await
        .unwrap();
    sync_with(&a, &remote(&origin, &root)).await.unwrap();
    sync_with(&b, &remote(&origin, &root)).await.unwrap();
    let marks = crate::series::list_episode_marks(&b, 1).await.unwrap();
    assert_eq!(
        marks
            .iter()
            .map(|m| (m.season, m.episode, m.watched))
            .collect::<Vec<_>>(),
        vec![(1, 1, true), (1, 2, true)]
    );

    // "Not watched" on B is newer, so it replaces A's mark on both devices.
    crate::series::set_episodes_watched(&b, 1, "tmdb:tv:42", 1, &[2], false, now + 60_000)
        .await
        .unwrap();
    sync_with(&b, &remote(&origin, &root)).await.unwrap();
    sync_with(&a, &remote(&origin, &root)).await.unwrap();
    let marks = crate::series::list_episode_marks(&a, 1).await.unwrap();
    assert_eq!(
        marks.iter().map(|m| m.watched).collect::<Vec<_>>(),
        vec![true, false]
    );

    // The server saw none of it.
    let stored = {
        let guard = mock.lock().unwrap();
        serde_json::to_string(&guard.groups.values().next().unwrap().items).unwrap()
    };
    assert!(
        !stored.contains("tmdb:tv:42") && !stored.contains("episode"),
        "{stored}"
    );
}
