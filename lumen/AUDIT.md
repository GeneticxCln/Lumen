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
- **Subtitle lookup.** Off until the user agrees; searches only on request. Every request goes through `netguard` to hosts the service owns (checked again after redirects), answers and downloads are size-capped, and a download must decode to subtitle text before the player sees it. Covered by in-process server tests (oversize, redirect off-host and to a private address, forged download address) and an ignored live test against the real services.
- **ffmpeg.** Local files only, with a timeout, a concurrency limit and a bounded, stable-named thumbnail cache.
- **mpv.** IPC socket in `$XDG_RUNTIME_DIR` (or a private directory), bounded pending requests.

## Identity and extraction

- Library entries are keyed by stable URLs and `content_key`s (`titles`, `title_sources`); favorites and history store a `content_key` and are matched on it as well as on the URL.
- An extracted page is saved as the **page** URL. Its stream link (which expires and may carry tokens) is resolved when the entry plays and is never stored, so replays work and split audio is never lost.
- When a stream fails, playback moves to the next ranked source of the same title and shows a notice.

## Known limitations and open work

- **Release packaging is not proven.** No `.deb`, AppImage or Flatpak has been built or installed on a clean machine, and there is no CI workflow in the repository yet.
- **Native playback is not verified on every desktop.** Rust tests pass, mpv IPC and MPRIS are exercised against a real mpv and a real D-Bus session, and the whole path (launch, page, mpv, MPRIS) was run in the real app under a virtual X display, but playback on real GNOME, KDE and Hyprland sessions has not been checked.
- **Single-window playback is an experiment.** Playing inside the Lumen window works on X11/XWayland (verified by screenshot under Xvfb: one window, video above the controls, setting off gives the separate mpv window). It is unavailable in a native Wayland window, controls are beside the video rather than over it, and fullscreen in this mode is untested on a real window manager.
- **Sync data model.** Synced records still carry the (credential-stripped) stream URL beside the `content_key`; the server cannot read either. Query parameters whose names contain `key`, `auth`, `token`, `secret` or `pass` are removed from synced URLs, which can break a stream whose identity lives in such a parameter on the other device.
- **Search and discovery** classify movies, series and live TV from names and groups, not catalog metadata. All library rows are held in memory; shelves virtualize the DOM but very large libraries need pagination.
- **Online subtitles depend on unofficial services.** The Stremio mirror and Gestdown are not offered to third-party apps, can change or block without notice, and cover different things (Gestdown is TV only). Matching a title to an IMDb id by name is fuzzy. Podnapisi and OpenSubtitles.com were not added (Podnapisi could not be reached from the test machine; OpenSubtitles.com needs each user's own key). yt-dlp's subtitle fields were tested on recorded JSON, not against the pinned binary on a real site. While the Find dialog is open over single-window playback, the video can sit above it (untested on a real window manager).
- **Interface language.** The catalog and a gate against new hard-coded text exist (README, "Translating Lumen"), but only English is shipped and most screens (Settings sections, local files, search engine, source dialog, profile management, home) still hold their text in the components, about 390 strings recorded in `src/i18n/baseline.json`. Text built in code (notices, errors, language names) is outside the gate.
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
