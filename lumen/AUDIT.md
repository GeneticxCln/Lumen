# Lumen audit

Current state of correctness, security boundaries and release readiness. The earlier version of this file described the Python sidecar and browser bridge, which no longer exist.

## Architecture in one paragraph

Tauri v2 shell (`src-tauri`) over a UI-agnostic Rust core (`crates/lumen-core`: M3U parsing, SQLite, search, TMDB, extraction, harvesting, probing, sync, network guard). Video plays in a separate native mpv window controlled over JSON IPC. The React UI talks only to Tauri commands. There is no browser mode and no local HTTP server. Extraction runs a pinned, checksum-verified `yt-dlp` binary as a subprocess.

## Security boundaries (verified by tests unless noted)

- **Network guard.** User-directed fetches (playlist import, harvest, search providers, extractor update, probes) go through `netguard`: public addresses only unless "allow local network sources" is on, every redirect hop and every resolved address is checked.
- **Profile PINs.** Enforced in the backend (`authorize_profile`) with a process-wide attempt limiter. A PIN protects against other people using the app, not against reading the database file.
- **Settings.** Only an allowlist of keys is writable from the UI; internal state (active profile, sync cursors) is not.
- **Secrets.** TMDB and search-provider tokens and the sync key live in the OS keychain, never in the settings table. Error text from HTTP failures has the request URL removed.
- **yt-dlp.** The managed binary must match the SHA-256 pinned in `crates/lumen-core/yt-dlp-manifest.json` and the release's published `SHA2-256SUMS`. A test ties the manifest to the upstream checksums, and an ignored live test downloads, verifies, installs and runs it. The pin moves only by editing the manifest in a commit.
- **Sync.** End-to-end encrypted; the server holds opaque blobs (see README "Sync" for exactly what it can see). Tested against an in-process server and, with `SYNC_TEST_URL`, against the real Worker.
- **ffmpeg.** Local files only, with a timeout, a concurrency limit and a bounded, stable-named thumbnail cache.
- **mpv.** IPC socket in `$XDG_RUNTIME_DIR` (or a private directory), bounded pending requests.

## Identity and extraction

- Library entries are keyed by stable URLs and `content_key`s (`titles`, `title_sources`); favorites and history store a `content_key` and are matched on it as well as on the URL.
- An extracted page is saved as the **page** URL. Its stream link (which expires and may carry tokens) is resolved when the entry plays and is never stored, so replays work and split audio is never lost.
- When a stream fails, playback moves to the next ranked source of the same title and shows a notice.

## Known limitations and open work

- **Release packaging is not proven.** No `.deb`, AppImage or Flatpak has been built or installed on a clean machine, and there is no CI workflow in the repository yet.
- **Native playback is not verified on every desktop.** Rust tests pass and mpv IPC is exercised, but end-to-end playback on GNOME Wayland, Hyprland and X11 has not been checked here. Video is still a separate mpv window (single-window playback is not done).
- **Sync data model.** Synced records still carry the (credential-stripped) stream URL beside the `content_key`; the server cannot read either. Query parameters whose names contain `key`, `auth`, `token`, `secret` or `pass` are removed from synced URLs, which can break a stream whose identity lives in such a parameter on the other device.
- **Search and discovery** classify movies, series and live TV from names and groups, not catalog metadata. All library rows are held in memory; shelves virtualize the DOM but very large libraries need pagination.
- **Clippy** reports a few style lints in `extract.rs`, `harvest.rs` and `search.rs`; they are not enforced.
- Extra mpv arguments are a trusted-local feature: whitespace-split, no shell involved.

## Checks

```bash
npm run typecheck && npm test && npm run build
cargo test -p lumen-core          # needs no GTK/WebKit
cd src-tauri && cargo test --lib  # needs the Tauri system libraries, ffmpeg; mpv tests skip without mpv
```

Ignored tests that need the network or a running server:

```bash
cargo test -p lumen-core live_ -- --ignored   # downloads yt-dlp; set SYNC_TEST_URL for the sync server test
```

This is a source-level review plus targeted tests, not a penetration test or release certification.
