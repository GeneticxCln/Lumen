# Identity migration plan (content keys, v2)

Status: **phase 1 landed, migration not yet run.** The resume, favorite, merge and sync fixes that
do not change stored keys are in the tree. Issues 6 and 7 *do* change `content_key` derivation, so
they are deliberately not switched on until the migration below exists and has been tested on a
copy of a real database. Shipping the derivation change alone would orphan existing My List and
history rows.

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

## Test plan before enabling

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

- Issue 15's backfill also needs its `fetch_all` replaced with keyset paging; do that in the same
  change or the migration will load 14.76M rows into memory.
- Issue 9's cross-URL *active* favorite is handled by the phase-1 `apply_favorite` fix (tombstones
  match by content key). v2 only changes which key that is.
- Playlist URLs themselves are not rewritten; only identity hashes change.
