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
- **Sync.** End-to-end encrypted; the server holds opaque blobs (see the README's "Sync" for exactly what it can see). Tested against an in-process server and, with `SYNC_TEST_URL`, against the real Worker.
- **Subtitle lookup.** Off until the user agrees; searches only on request. Every request goes through `netguard` to hosts the service owns (checked again after redirects), answers and downloads are size-capped, and a download must decode to subtitle text before the player sees it. Covered by in-process server tests (oversize, redirect off-host and to a private address, forged download address) and an ignored live test against the real services.
- **ffmpeg.** Local files only, with a timeout, a concurrency limit and a bounded, stable-named thumbnail cache.
- **mpv.** IPC socket in `$XDG_RUNTIME_DIR` (or a private directory), bounded pending requests.

## Identity and extraction

- Library entries are keyed by stable URLs and `content_key`s (`titles`, `title_sources`); favorites and history store a `content_key` and are matched on it as well as on the URL.
- An extracted page is saved as the **page** URL. Its stream link (which expires and may carry tokens) is resolved when the entry plays and is never stored, so replays work and split audio is never lost.
- When a stream fails, playback moves to the next ranked source of the same title and shows a notice.

## Known limitations and open work

- **Release packaging is not proven.** The `.deb` and AppImage build in CI's release workflow definition and were inspected locally, but no release has been published, and no package (`.deb`, AppImage, Flatpak, Arch) has been installed on a clean machine. The Flatpak and Arch package are built from the checkout by `.github/workflows/packaging.yml`; check that workflow's latest run for whether they build.
- **Native playback is not verified on every desktop.** Rust tests pass, mpv IPC and MPRIS are exercised against a real mpv and a real D-Bus session, and the whole path (launch, page, mpv, MPRIS) was run in the real app under a virtual X display, but playback on real GNOME, KDE and Hyprland sessions has not been checked.
- **Single-window playback is an experiment.** On X11/XWayland it was checked by screenshot under Xvfb: one window, video above the controls, setting off gives the separate mpv window. On native Wayland the video renders through libmpv into a GTK GL area; in the real app under a headless Weston (software OpenGL) the frame had video content (about 57% of the pixels lit, the centre matching the test pattern). Not verified: real GNOME, KDE or Hyprland sessions, hardware decoding on this path (the Wayland display is not passed to libmpv, so it may fall back to software decoding), and fullscreen on a real window manager. Controls are beside the video rather than over it. A headless Weston that has run many windows can stop delivering frame callbacks; restart it before testing.
- **Casting is tested against a fake device only.** Discovery, the control calls and serving a file (ranges, one allowed peer, media files only) are tested against an in-process UPnP renderer; no real TV, receiver or speaker has been tried, and TVs differ in what formats, ranges and metadata they accept. Subtitles are not sent, and the device's own volume is not controlled. Chromecast and AirPlay are not supported.
- **Self-update has not replaced a real AppImage.** The release lookup, version comparison, checksum, size and redirect limits and the file swap are tested against an in-process server and temporary files, but no release has been published yet and no AppImage has been updated by a running Lumen, so the restart handoff (a new copy started after the old one exits) is untested. Integrity is a checksum from the same release, not a signature. An AppImage in a folder the user cannot write to cannot update itself and falls back to telling the user.
- **mpv start failures and choppy scrolling (0.1.1).** A released AppImage's environment (library and tool paths into the AppImage) used to be inherited by the host `mpv`; children now get it without those entries (checked in the real app with a simulated `APPDIR`, not in a real AppImage on another distribution). mpv start errors now carry mpv's own last lines and a dead mpv is reported at once. Scrolling changes (no blur on the sticky header, async image decoding, cheaper hover effects, off-screen shelves skipped, smooth scrolling and hardware acceleration requested from WebKitGTK) were checked for rendering only; smoothness itself was not measured and needs a real display. The in-window libmpv path still loads the host's libmpv with the AppImage's own library path in this process.
- **mpv dying mid-session (0.1.3).** A command sent after mpv has already exited used to surface as a bare "Failed to write to mpv socket". It now carries mpv's own last lines, the same way a failed start does ("mpv stopped responding (its control connection closed): ..."). Checked with a fake control socket that answers the startup calls and then closes (not a real mpv crash).
- **Adding many links is checked against an in-process server and a mocked page, not at scale.** Link finding, the playlist check (valid list, HLS, 404, an HTML page, a redirect to a private address, the local-network switch), sampling and cancelling are unit-tested; the dialog was driven in a browser with a mocked backend. Not tried: hundreds of real IPTV links (speed, hosts that block repeated requests), links that need special headers or a login, and the file picker in the real window. Only five sample channels per playlist are tried, so "4 of 5 respond" is a hint, not a guarantee.
 Synced records still carry the (credential-stripped) stream URL beside the `content_key`; the server cannot read either. Query parameters whose names contain `key`, `auth`, `token`, `secret` or `pass` are removed from synced URLs, which can break a stream whose identity lives in such a parameter on the other device.
- **Search and discovery** classify movies, series and live TV from names and groups, not catalog metadata. All library rows are held in memory; shelves virtualize the DOM but very large libraries need pagination.
- **Online subtitles depend on unofficial services.** The Stremio mirror and Gestdown are not offered to third-party apps, can change or block without notice, and cover different things (Gestdown is TV only). Matching a title to an IMDb id by name is fuzzy. Podnapisi and OpenSubtitles.com were not added (Podnapisi could not be reached from the test machine; OpenSubtitles.com needs each user's own key). yt-dlp's subtitle fields were tested on recorded JSON, not against the pinned binary on a real site. While the Find dialog is open over single-window playback, the video can sit above it (untested on a real window manager).
- **Interface language.** Every screen reads its text from the catalog and a test enforces it, but only English exists. Backend error text, genre/category labels derived from English playlist group names, and the strings of your own playlists stay as they are. The rendered text of 21 screens was compared before and after the migration (identical except one fixed plural and the new notification setting).
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
