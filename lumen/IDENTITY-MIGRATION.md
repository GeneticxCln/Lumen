# Identity migration plan (content keys, v2)

Status: **implemented and verified on a copy of the real 14.76M-channel database.** The v2
derivation and the resumable runner are live; issues 6 and 7 are fixed. The snapshot used for the
real run had no favorites, history or title sources, so those rekey paths are covered by the
synthetic migration tests, not by that snapshot. Issue 15's title backfill is still only rebuilt
when `title_sources` exists; the real library's empty title tables need the paged backfill planned
with the library-scale work.

Related: [OPEN-ISSUES.md](OPEN-ISSUES.md) issues 6, 7, 8, 9, 15.

## Why a migration is required

`content_key` is a stored identity, not a derived-on-read one. It exists in:

- `favorites.content_key` (with `channel_url` as the row key)
- `history.content_key` (with `path`)
- `titles.content_key`, `title_sources.content_key`
- `category_overrides.content_key`
- `sync_queue` item keys (`<profile sync key>:<sha256(content_key)>`) and payloads

Changing what `ContentKey::derive` returns therefore does not just affect new rows: old favorites
and history would keep an old key, no longer match `title_sources`, and stop appearing in My List
or resume. The sync queue would also keep pushing the old identity, and the server holds records
under it.

## v2 key rules

Two changes, both in `crates/lumen-core/src/content_key.rs`:

1. **Stop discarding the whole query string.** `for_stream_url` currently hashes the URL with
   `?…` and `#…` removed, so `play.php?id=1` and `play.php?id=2` collide. v2 keeps distinguishing
   query parameters and only removes parameters whose names mark them as credentials or
   volatility (`token`, `auth`, `secret`, `key`, `pass`, `signature`, `sig`, `expires`, `expiry`,
   `exp`). Fragments are ignored (they do not reach a server). If the URL cannot be parsed, strip
   the fragment only.
2. **Reject placeholder `tvg-id`s.** `for_tvg` currently accepts any non-empty string, so
   `tvg-id="None"` becomes `tvg:none` for every such channel. v2 rejects `none`, `null`, `nil`,
   `n/a`, `na`, `-`, `unknown` and `undefined` (case-insensitive, trimmed), falling through to the
   stream key instead.

Use a **new prefix for stream fallbacks**, `stream2:` rather than `stream:`. It makes migrated rows
identifiable, prevents a new key from colliding with an old one for a different URL, and lets the
migration be re-run safely. `tvg:`, `tmdb:`, `file:` keep their prefixes; their values only change
for placeholder IDs.

`ContentKey::for_stream_url` gains a v2 implementation; keep a `legacy_stream_key` helper that
reproduces today's behavior for the migration (it must not be used by new writes).

## Migration design

A versioned, resumable background migration. It must never block startup, never run twice
concurrently, and never leave the database half-migrated without a recorded cursor.

### Settings

- `content_key_version`: absent/`1` = legacy; `2` = complete. Written only at the end.
- `content_key_migration_cursor`: last `channels.id` processed (integer string).
- `content_key_migration_started_at`: diagnostics only.

### Steps (run in this order)

1. **Backup.** `VACUUM INTO '<app data>/lumen-pre-key-v2-<timestamp>.db'` before the first write.
   Skip the whole migration if the backup fails.
2. **Channels, keyset-paginated.** `SELECT … FROM channels WHERE id > ? ORDER BY id LIMIT 500`.
   For each row compute `legacy` and `v2` keys from the same fields. If they differ:
   - Rekey that channel's favorites rows *by URL*: `UPDATE favorites SET content_key = v2 WHERE
     profile_id = ? AND channel_url = channel.url AND content_key = legacy` — one row at a time so
     two channels that previously shared a legacy key split correctly.
   - Rekey that channel's history rows the same way, by `path`.
   - Queue sync tombstones under `legacy` and fresh pushes under `v2` for each affected row, with
     one `now` per batch. Do not queue anything when the row's profile has no `sync_key`.
   - Update `title_sources` rows with `channel_id = channel.id` to `v2`.
   - Commit every 500 rows and write the cursor in the same transaction.
3. **Titles.** Derived from `title_sources`; do not try to rename rows in place (a legacy key can
   map to several v2 keys). `DELETE FROM titles` after step 2, then run the batched titles backfill
   (`backfill_titles_and_sources` split into pages) to rebuild it from the now-correct
   `title_sources`. This is also the fix for issue 15: make the backfill resumable and run it when
   `content_key_version` is 2, not only once ever.
4. **Category overrides.** They have no URL or channel column, so a shared legacy key cannot be
   split. Leave them under the old key (readers ignore an override once TMDB matches anyway) and
   document that a user may need to re-apply a manual correction for a title whose key changed.
5. **Sync queue.** Delete any queued rows whose `item_key` is `<profile>:<sha256(legacy)>` after
   step 2 has queued the replacements. Do **not** reset the pull cursor: the server's records are
   keyed by content key, and step 2's tombstones retire the old ones normally.
6. **Verify, then mark complete.** In one transaction: every `favorites.content_key` and
   `history.content_key` is non-empty and starts with a known prefix; `COUNT(*) FROM
   title_sources WHERE content_key LIKE 'stream:%'` is 0; no `content_key_migration_cursor` row
   remains. Only then write `content_key_version = 2`.

### Resumability and failure

- The cursor advances only with the batch's data changes, so a crash re-does at most 500 channels.
  Re-running a batch is idempotent: legacy→v2 updates match only rows still on the legacy key.
- Any error stops the job, leaves `content_key_version` at 1, and logs through `tracing`. The app
  keeps working on the partially migrated database because both key shapes remain readable
  (favorites/history are matched by URL/path as a fallback in the read paths).
- A second app instance must not start the job: use the existing process-level init lock, and
  re-check the setting after acquiring it.

### Scale

The audit's library is ~14.76M channels. At 500 rows per transaction, that is ~29,500 commits.
Budget minutes-to-an-hour in release builds; the job must run with the same WAL + `busy_timeout`
pool as the app and should throttle (one batch, then `yield_now`) so playback and browsing stay
usable. Never `fetch_all` the channels table. Progress is the cursor, not a row count in memory.

### Rollback

There is no in-place undo for a destructive rekey. The `VACUUM INTO` backup is the rollback: quit
the app, replace `lumen.db` with the backup. Do not delete the backup automatically; keep the most
recent one and mention its path in the log.

## Implementation and results

- `crates/lumen-core/src/content_migration.rs`: versioned runner with `content_key_version`,
  `content_key_migration_cursor` and `content_key_migration_started_at` settings. It starts in
  `init_db`'s background task before the title backfill.
- `VACUUM INTO` a sibling `lumen.db.pre-key-v2-<timestamp>.db` before the first write. If the
  backup fails, the migration stops before touching anything.
- Keyset batches of 500 channels; the cursor is committed in the same transaction as that batch's
  rekeys. Empty tables are detected once, so a 14.76M-channel library with no favorites/history/
  title sources is a scan rather than three lookups per channel.
- Favorites and history are scoped by both URL/path and the legacy key, so query-distinct streams
  split correctly. `title_sources` gets its v2 `titles` row first (foreign key) and is rebuilt in
  pages. Legacy `titles` rows are removed only when no source references them.
- Sync tombstones/fresh pushes are queued only when this device has synced at least once; a
  sync-disabled library does not accumulate millions of queue rows.
- Completion requires all three identity tables to have no legacy keys and `PRAGMA quick_check`
  to return `ok`.

### Real database copy

Run against a snapshot of `~/.local/share/com.quinton.lumen/lumen.db` (14,760,400 channels):

- **143.6 s** in a release build, including the `VACUUM INTO` backup (5.4 GB).
- `content_key_version = 2`, cursor absent, zero legacy keys, `quick_check = ok`.
- The snapshot had 0 favorites, 0 history, 0 titles and 0 title_sources, so it exercised the
  channel scan, backup, verification and completion path. The rekey paths for the other tables are
  covered by the synthetic fixture (`content_migration_tests.rs`), which includes two query-distinct
  URLs sharing the old `stream:` key and a `tvg-id="None"` group.
- The real library's `titles` table was already empty (`titles_backfill_v1 = done`), and with no
  title sources the runner correctly skips an hours-long 14.76M-row title rebuild. The paged
  startup backfill now gates on `titles_backfill_v2`, so that stale marker no longer suppresses the
  rebuild; it runs in the background on the next launch.

Rollback: quit Lumen, then replace `lumen.db` with the `.pre-key-v2-*.db` file named in the log.

## Test plan

1. **Unit:** v2 stream keys separate `?id=1`/`?id=2`; rotating `token`/`expires` keeps one key;
   fragments ignored; placeholder `tvg-id`s fall through; real IDs unchanged.
2. **Migration on synthetic data:** 100,000 channels, favorites and history across two profiles,
   both a shared legacy `stream:` key (two query-distinct URLs) and a `tvg:none` group. Assert:
   each favorite/history row moves to the right per-URL key; counts are unchanged; `titles` is
   rebuilt; no `stream:` fallback keys remain.
3. **Interrupted migration:** kill after N batches, restart, assert the cursor resumes and the final
   state equals an uninterrupted run.
4. **Sync:** two devices, one migrated first. The old key's records are tombstoned, the new key's
   arrive, and a device that merges a favorite under a different URL still converges.
5. **Real database copy:** run against a copy of the 14.76M-channel database, measure time, then
   verify `PRAGMA quick_check`, row counts, and a sample of known favorites/history. Only after
   that should `content_key_version = 2` ship in a release.

## Not in this migration

- Issue 15's backfill still needs its `fetch_all` replaced with keyset paging. The migration
  already pages its own title rebuild, but with no `title_sources` it skips that rebuild rather
  than materializing 14.76M rows; the real library's empty title tables need the paged backfill.
- Issue 9's cross-URL *active* favorite is handled by the phase-1 `apply_favorite` fix (tombstones
  match by content key). v2 only changes which key that is.
- Playlist URLs themselves are not rewritten; only identity hashes change.
