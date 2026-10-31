# Open issues: external audit of 2026-10-31

An independent, hands-on audit of `a95e0ab` (release v0.1.11) was run against a real library —
186 playlists, 14,764,xxx channels. It found the release broken in real use. Its first four
critical findings, plus one test-harness hang, are fixed in v0.1.12 (`570bf9f`); everything else
below is still open.

`AUDIT.md` remains the design and test record, but read its "verified" language as "covered by the
tests named there", not as real-world proof: this audit reproduced failures those tests miss.

**Status key**
- **Fixed in 0.1.12** — closed by the named commit and test.
- **Open, confirmed** — re-checked in the current tree on 2026-10-31.
- **Open, reported** — the auditor's finding, marked `[Ran]`/`[Read]` as they did; not re-run here.
- **Deferred** — needs a schema or sync-protocol change and a data migration.

**Phase 1 of the identity/resume batch landed on main on 2026-10-31 (unreleased).** Issues 8–14
are fixed, with six regression tests in `crates/lumen-core/src/identity_tests.rs`. Issues 6, 7
and 15 need the v2 `content_key` migration, which is designed in
[IDENTITY-MIGRATION.md](IDENTITY-MIGRATION.md) but deliberately not enabled yet: flipping the
derivation without the migration would orphan existing My List and history rows.

**The player/privacy batch (16–19, 23–25) landed on main on 2026-10-31 (unreleased).** The player
now opens with its spinner before mpv's load, records the attempted URL so a failed first load
falls back, and emits an ended state when the control connection closes. The retry classifier
learned mpv's real load-failure wording. PIN loading is fail-closed. A new
`src-tauri/src/playback_policy.rs` allowlists playback schemes, rejects local files from fetched
playlists, applies `netguard` to LAN addresses, and refuses non-HTTP protocols while network
privacy is on.

**State of the real library after the audit's own mistake** (it ran the release binary with
`--version`, which opened the full app for about 3 minutes): SQLite `quick_check` is `ok`; the 186
lists and 14.76M channels are intact. Settings changed: `last_visit_1`, `update_last_check`, and
possibly `update_notified_tag`, so the v0.1.11 update notice may not show again. Sync and
auto-refresh were off. Nothing in the repo was modified by the audit.

## Closed by v0.1.12

| # | Finding | Fix |
|---|---|---|
| 1 | HLS never started: `reconnect_at_eof=1` made mpv re-request a stream at its end forever (about 2,800 requests/s on a server without Range). | Reconnect flags removed from `mpv_ipc.rs`; tests assert no reconnect option is forced. |
| 2 | v0.1.11's binary reported 0.1.10; the tag, `PKGBUILD` and metainfo disagreed. | All declarations bumped to 0.1.12; `scripts/check-version.mjs` gates CI and the release (`verify-version`, which `linux` needs). |
| 3 | The published Flatpak's ffmpeg had no TLS, so bundled mpv could not play HTTPS. | Flatpak manifest now builds ffmpeg with `--enable-gnutls`; the v0.1.12 Flatpak built and published. Runtime playback on it is still unverified. |
| 4 | Playlists with 16,384+ channels could not be deleted, and a refresh removing that many failed the same way ("too many SQL variables"). | Tombstone lookup chunks at 400 URLs; new shared `db::delete_channel_rows` chunks at 900 ids. Two 16,384-channel tests in `db.rs`. |
| 32a | The desktop test hung forever because ffmpeg 9 deadlocks on its fixture command. | Fixture drops `-shortest` and waits at most 30s, killing the child on timeout. |

Cleanup also done in v0.1.12: the stale `lumen/src-tauri/Cargo.lock` was removed (the crate is a
workspace member, so the root lock is authoritative), `ARCHITECTURE.md`'s removed-README-table
reference was corrected, and `AUDIT.md`'s packaging/self-update entries were updated.

## Open — critical

**5. The app cannot handle the real library size.** `src/hooks/useChannels.ts` calls
`listChannels` for every source at every start and holds every channel in one in-memory array;
`list_channels` returns every row. At 14.76M channels that is roughly 7 GiB of JSON (extrapolated
from a real 120k-channel list); after 154s the page had loaded about 114 of 186 lists and never
finished. Nothing caps imports or warns about size. One 120k list on its own imports in about 2.5
minutes and works. **Open, confirmed.** Needs database paging, FTS5 search, and import
limits/warnings — not a small patch.

## Open — identity, history and sync (6–15)

6. **Different channels merge into one.** `ContentKey::for_stream_url` hashes the URL after
   stripping the query and fragment, so `…/play.php?id=1` and `…/?id=2` get the same key:
   favoriting one favorites both, history collapses into one row, fallback can jump between them.
   **Open — v2 key change designed, not enabled** (`content_key.rs`, `for_stream_url`; see
   [IDENTITY-MIGRATION.md](IDENTITY-MIGRATION.md)).
7. **`tvg-id="None"` merges unrelated titles.** `for_tvg` accepts any non-empty string, including
   the literal placeholder. **Open — v2 key change designed, not enabled** (`content_key.rs`,
   `for_tvg`; see [IDENTITY-MIGRATION.md](IDENTITY-MIGRATION.md)).
8. **Removing a favorite through another source doesn't remove it from My List.** **Fixed
   (unreleased):** toggling off now clears every active row sharing the title key;
   `identity_tests::unfavoriting_one_source_clears_the_title_everywhere`.
9. **A favorite removal on one device may not reach another** when the devices use different
   stream URLs for the same favorite. **Fixed (unreleased):** remote tombstones match by content
   key, not the sender's URL, and respect `updated_at`;
   `identity_tests::remote_favorite_tombstone_clears_a_different_url_for_the_same_title`.
10. **Resume doesn't follow a title across sources**, and switching source loses it. **Fixed
    (unreleased):** the lookup resolves through the playing channel's id, so another source's
    row derives the same key; `identity_tests::resume_follows_the_title_across_sources_and_ignores_the_last_seconds`.
11. **Finished titles resume at the end**, so they stop immediately. **Fixed (unreleased):** a
    position within five seconds of the end is treated as finished; same test as issue 10.
12. **Harvested streams run yt-dlp on the page** instead of playing the stream that was found.
    **Fixed (unreleased):** harvested playlists use their own `harvest` source, which
    `extract_origin_for_channel` does not resolve;
    `identity_tests::harvested_entries_play_their_found_stream_instead_of_resolving_the_page`.
13. **Merging profiles doesn't queue the moved data for sync.** Only the profile deletion is
    queued, so an already-synced favorite/history row under the removed profile's key can be lost
    on other devices. **Fixed (unreleased):** the merge re-queues favorites, history and episode
    marks under the surviving profile and drops the old profile's pending copies;
    `identity_tests::merging_profiles_queues_the_moved_data_under_the_surviving_profile`.
14. **An old progress deletion from sync erases newer local history.** `apply_progress` deletes
    history by `(profile_id, content_key)` with no timestamp guard. **Fixed (unreleased):** the
    delete now requires `played_at * 1000 <= tombstone.updated_at`;
    `identity_tests::an_old_progress_tombstone_does_not_delete_newer_history`.
15. **Fallback and "new since last visit" cannot work on an existing library.** `titles` is only
    populated by a one-time backfill gated on `titles_backfill_v1`; import now writes it, but a
    database whose channels predate that never runs it again, so `titles` stays empty. **Open —**
    the v2 backfill is part of [IDENTITY-MIGRATION.md](IDENTITY-MIGRATION.md); the current one-shot
    `fetch_all` must not be run against a 14.76M-row library as-is.

## Open — playback and updates (16–22)

16. **The same-URL retry doesn't fire on load failures.** Real mpv load errors ("loading failed",
    "failed to open") matched none of `backoff.ts`'s patterns. **Fixed (unreleased):** those
    patterns were added, bounded by the existing three-attempt budget; `backoff.test.ts`.
17. **A failed first load gets no fallback**, and failing over from a playing channel can target
    the wrong (previous) channel because the current URL is only stored after a successful load.
    **Fixed (unreleased):** `playChannel`/`playMedia` record the attempt and buffering state before
    awaiting the backend, and a rejected `playUrl` now feeds the retry/fallback effect; a new
    fallback URL is also current before it is loaded. No frontend unit test — this codebase's
    frontend tests cover pure logic only.
18. **Closing mpv or an mpv crash leaves the UI showing "playing".** **Fixed (unreleased):**
    `PlayerStateEvent` carries `ended`, set on read EOF, a read error, and a failed write, and
    emitted as `player://state`; test in `mpv_ipc.rs`.
19. **Nothing shows while a stream loads** — no spinner or Stop; the action can block about 30s.
    **Fixed (unreleased):** the overlay opens and buffering is set before the backend load, so
    the existing spinner and Stop button are available immediately (`App.tsx`, `usePlayer.ts`).
20. **After one self-update, Settings only offers Restart**, because `update_installed_tag` is
    never cleared. **Open, reported** (`updater.rs:189`, `UpdateSettings.tsx:78`).
21. **Fallback isn't ranked.** `title_sources.rank` is inserted as `0` for every row, so
    `ORDER BY rank DESC` is arbitrary, and fallback only moves forward — the last source wraps back
    to the first. **Open, confirmed** for the hardcoded `0` (`db.rs`, batch insert); ranking
    behavior is reported.
22. **Watched state fades.** It is derived from the last 200 history entries, and history is pruned
    at 500 entries or 90 days. **Open, reported** (`App.tsx:149`, `db.rs:1863`).

## Open — security and privacy (23–26)

23. **Network privacy leaks for non-HTTP streams.** With privacy on, `rtsp`, `rtmp`, `tcp`, `ftp`
    and `mms` streams connected directly, because mpv only receives `--http-proxy` (which covers
    HTTP(S)). **Fixed (unreleased):** when privacy is on, `playback_policy` refuses any URL that
    is not HTTP(S) or a local file, and the check runs before mpv starts; tests in
    `src-tauri/src/playback_policy.rs`.
24. **Playlist entries have no scheme or address check before reaching mpv.** A remote playlist
    can aim playback at `file://` paths or LAN addresses even with local sources off. **Fixed
    (unreleased):** a playback allowlist rejects unknown schemes, refuses local files from fetched
    playlists, and runs HTTP(S) through `netguard` (so private addresses need the local-network
    setting); tests in `src-tauri/src/playback_policy.rs`.
25. **The PIN lock fails open.** Any error loading profiles set `locked=false` (the comment
    defended a browser bridge that no longer exists), and the lock started open until profiles
    load. **Fixed (unreleased):** the app renders a neutral loading gate until the profile list is
    read, a failed read keeps/puts the gate up, and an empty profile list shows an error screen
    with Retry instead of the library (`App.tsx`).
26. **yt-dlp is about 9 months out of date.** The manifest pins `2026.02.04`; a yt-dlp found on
    `PATH` may run unverified. **Open, confirmed** for the pin
    (`crates/lumen-core/yt-dlp-manifest.json`); the `PATH` fallback is reported
    (`extract.rs:141`).

## Open — medium and low (27–33)

27. **The README's default-sync-server claim was false.** The `LUMEN_SYNC_URL` repository variable
    is not set (confirmed with `gh variable list`, which returns nothing), so the v0.1.12 release
    carries no default server. README corrected in this commit.
28. **A full sync group also blocks pulls**, because a failed push aborts before the pull.
    **Open, reported** (`sync.rs:1002`).
29. **Each sync can reload the whole library.** The app's own pushes count as "applied", so the
    sync-applied callback fires and re-runs the reload (with TMDB calls) roughly every 90s during
    playback when sync is on. **Open, reported** (`sync.rs:936`, `App.tsx:139`).
30. **Worker races:** the sequence number is taken before the write, so a device could miss rows;
    the quota counter can drift. **Open, reported, unconfirmed** (`worker.ts:294`) — local Wrangler
    serialized the requests, so the auditor could not force it.
31. **No request rate limit or global cap on the official server**; about 2.4 GB per IP per day is
    possible by the auditor's calculation. **Open, reported.**
32. **Crash logs are weak.** With `panic = "abort"`, 2 of 40 panics never reached the log; most
    errors use `eprintln!` (30 uses against 8 of `tracing`), so they never reach the log file.
    **Open, reported**; the test-harness hang (32a) is fixed.
33. **Smaller player issues:** mpv dying during a load makes Lumen wait the full 30s; a failed
    action can leave an empty mpv window open. **Open, reported** (`commands.rs:199`).

## Dependencies (audit's results, not re-run here)

- `cargo audit`: 4 advisories — `rustls-webpki`, `webpki-roots`, `webpki-root-certs`, `ctutils`.
- `npm audit`: 11 moderate, all in ESLint tooling.

## Missing capability (not bugs)

- **Library scale:** database paging, FTS5 full-text search, import limits, duplicate handling.
  Search truncates at 5,000 results by name, with no notice.
- **Features:** TV guide (`url-tvg` is ignored), catch-up, Xtream login.
- **Backup/export:** none; sync doesn't carry playlists either.
- **Diagnostics screen:** the backend commands exist, nothing uses them.
- **Tests:** none for the 39 component/hook files; E2E only plays a local file (which is how
  issue 1 shipped); no HTTP-streaming or scale tests in CI.
- **Real-world checks still open:** real TVs, real GNOME/KDE/Hyprland sessions, a real proxy, a
  real AppImage self-update.
- **Release process:** no dependency scanning or Dependabot; Actions not pinned to SHAs; one job
  holds the signing key; `main` is unprotected; no `SECURITY.md` or changelog; release notes empty.
- **Packaging:** no aarch64 builds even though `PKGBUILD` and the updater mention them; neither
  Flatpak nor `PKGBUILD` is published to Flathub/AUR.
- **Translations:** English only.

## Not re-verified here

`privacy/relay.rs`, the cast protocol, `subtitles/*`, `tmdb.rs`, search ranking, `embed.rs`,
`libmpv.rs`, `mpris.rs`. Checked and fine per the audit: the `lumen://` handler in the `.deb`, the
release signature files, and the published assets.

## Suggested order

1. ~~Release v0.1.12: remove the reconnect flags, enable TLS in the Flatpak, batch the delete
   query, add the tag/version check.~~ Done.
2. Identity and resume (6–15). Phase 1 (8–14) is on main; phase 2 (6, 7, 15) needs the v2 key
   migration before the derivation can change.
3. ~~Player state (16–19) and privacy (23–25), including an allowlist of stream schemes for
   mpv.~~ Done on main (unreleased).
4. Rework library loading (5) before adding features; until then the real library stays too big to
   load, and only trimming it from the UI helps.
