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
   **Open, confirmed** (`content_key.rs`, `for_stream_url`).
7. **`tvg-id="None"` merges unrelated titles.** `for_tvg` accepts any non-empty string, including
   the literal placeholder. **Open, confirmed** (`content_key.rs`, `for_tvg`).
8. **Removing a favorite through another source doesn't remove it from My List.** **Open,
   reported** (`db.rs:1326` at `a95e0ab`).
9. **A favorite removal on one device may not reach another** when the devices use different
   stream URLs for the same favorite. **Open, reported** (`sync.rs:474`).
10. **Resume doesn't follow a title across sources**, and switching source loses it. **Open,
    reported** (`db.rs:2099`).
11. **Finished titles resume at the end**, so they stop immediately. **Open, reported**
    (`db.rs:2101`).
12. **Harvested streams run yt-dlp on the page** instead of playing the stream that was found.
    **Open, reported** (`commands.rs:1017`, `commands.rs:227`).
13. **Merging profiles doesn't queue the moved data for sync.** Only the profile deletion is
    queued, so an already-synced favorite/history row under the removed profile's key can be lost
    on other devices. **Open, confirmed** (also noted at the end of `AUDIT.md`).
14. **An old progress deletion from sync erases newer local history.** `apply_progress` deletes
    history by `(profile_id, content_key)` with no timestamp guard. **Open, confirmed**
    (`sync.rs`, `apply_progress`).
15. **Fallback and "new since last visit" cannot work on an existing library.** `titles` is only
    populated by a one-time backfill gated on `titles_backfill_v1`; import now writes it, but a
    database whose channels predate that never runs it again, so `titles` stays empty. **Open,
    confirmed** (`db.rs`, `TITLES_BACKFILL_SETTING`).

## Open — playback and updates (16–22)

16. **The same-URL retry doesn't fire on load failures.** Real mpv load errors ("loading failed",
    "failed to open") match none of `backoff.ts`'s patterns. **Open, reported** `[Ran]`.
17. **A failed first load gets no fallback**, and failing over from a playing channel can target
    the wrong (previous) channel because the current URL is only stored after a successful load.
    **Open, reported** (`usePlayer.ts:191`).
18. **Closing mpv or an mpv crash leaves the UI showing "playing".** The reader loop sets
    `connection_alive=false` and breaks without emitting any event. **Open, confirmed**
    (`mpv_ipc.rs`, reader loop).
19. **Nothing shows while a stream loads** — no spinner or Stop; the action can block about 30s.
    **Open, reported** (`App.tsx:195`).
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
    HTTP(S)). The docs' "fail-closed throughout" does not hold for those schemes. **Open,
    reported** `[Ran]`; the mechanism matches the code. This also means `AUDIT.md`'s
    network-privacy paragraph overstates its coverage.
24. **Playlist entries have no scheme or address check before reaching mpv.** A remote playlist
    can aim playback at `file://` paths or LAN addresses even with local sources off. **Open,
    reported** (`m3u.rs:189`, `commands.rs:259`).
25. **The PIN lock fails open.** Any error loading profiles sets `locked=false` (the comment
    defends a browser bridge that no longer exists), and the lock starts open until profiles load.
    **Open, confirmed** (`App.tsx`, `loadProfiles`).
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
2. Identity and resume (6–15). These change stored data keys and sync records, so they need a
   migration and cross-device tests before shipping.
3. Player state (16–19) and privacy (23–25), including an allowlist of stream schemes for mpv.
4. Rework library loading (5) before adding features; until then the real library stays too big to
   load, and only trimming it from the UI helps.
