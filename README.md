# Lumen

[![CI](https://github.com/GeneticxCln/Lumen/actions/workflows/ci.yml/badge.svg)](https://github.com/GeneticxCln/Lumen/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Logo:** [`lumen/src-tauri/icons/icon.png`](lumen/src-tauri/icons/icon.png)

A Linux-first media library and player for your own sources. Lumen keeps your playlists, streams and local files in one place, plays them with mpv, remembers where you stopped, follows series, finds subtitles without an account, and can cast to a TV on your network. No accounts, no ads, no bundled channels.

Built with Tauri v2, React 19, TypeScript, Rust, SQLite and native mpv.

## Contents

- [Features](#features) · [Install](#install) · [Updates](#updates) · [Subtitles](#subtitles) · [Linux integration](#linux-integration) · [Casting](#casting) · [Network privacy](#network-privacy) · [Sync](#sync) · [Development](#development) · [License](#license)

## Features

- **Your sources, one library.** M3U/M3U8 playlists (file or URL), pages to extract a stream from (verified yt-dlp), and local folders. Movies, series and live TV sorted from names and groups; no catalog service behind it.
- **Discovery that works offline.** Thumbnails from FFmpeg/folders, resume points per file, favorites, recently watched, recommendations from your own history, poster-frame chooser.
- **Series state.** Watched marks per episode, next episode, new/upcoming episodes (optional TMDB key), opt-in desktop notifications when a followed series airs.
- **Search** across sources and files (Ctrl/Cmd + K).
- **Native mpv playback.** Your own mpv.conf/scripts apply. Video in a separate mpv window (X11/Wayland) or inside the Lumen window (experimental).
- **Subtitles** from the file, next to it, or found online (no account/API key).
- **Profiles** with optional PINs, per-profile language/subtitle/playback preferences.
- **Encrypted sync, no accounts**, between your own devices.
- **Casting** to UPnP/DLNA TVs/receivers.
- **Themes** (Cinema, Midnight, Forest, Daylight, System), keyboard shortcuts, message catalog ready for translations.
- **Updates from GitHub releases** with one click.

Not included on purpose: a Netflix-style catalog, paid-channel access, DRM bypass, or an external recommendation service.

## Install

Releases: <https://github.com/GeneticxCln/Lumen/releases>. Install `mpv` (and `ffmpeg` for thumbnails); Lumen starts mpv from your PATH rather than bundling it. `yt-dlp` is downloaded and verified by Lumen itself.

| Package | How |
| --- | --- |
| **AppImage** | `chmod +x lumen_<version>_amd64.AppImage` then run. Self-updating. |
| **.deb** | `sudo apt install ./lumen_<version>_amd64.deb` (needs mpv; recommends ffmpeg). |
| **Flatpak** | CI-built bundle from `lumen/packaging/flatpak/` (includes mpv); not on Flathub. |
| **Arch** | `lumen/packaging/aur/PKGBUILD` builds from the tagged source. |

Details: [lumen/FAQ.md](lumen/FAQ.md)

## Troubleshooting

- **mpv stopped before it was ready / did not open its control socket / stopped responding.** Run `LUMEN_DEBUG_MPV=1 lumen` to see mpv's output. Check `mpv --version` works on its own.
- **Scrolling choppy.** Try `LUMEN_WEBKIT_SAFE=1 lumen` (disables WebKitGTK DMA-BUF renderer). On Wayland, `GDK_BACKEND=wayland ./lumen_*.AppImage` uses native Wayland.

## Updates

Settings → **About & updates** → **Update to the latest version**. Lumen checks GitHub releases; if a newer tag exists it downloads, verifies against `SHA256SUMS`, and installs (AppImage swaps in place; other packages tell you where to get it). Automatic checks run at start and every few hours (once a day max). Drafts and pre-releases are never offered.

## Subtitles

Local: tracks inside the file, `.srt`/`.ass` next to a video (auto-loaded), drag and drop, or pick in the player. Settings → Playback sets preferred language/size/position per profile.

Online (**Find…** button): no account/API key; off until you agree. Searches Stremio's OpenSubtitles mirror (movies/series by IMDb id) and Gestdown (TV). Sends title name/year/season/episode/language and, for local files, an OpenSubtitles hash (never the file). Results are saved and loaded automatically next time. Each service can be disabled in Settings → Playback.

## Player settings

Per profile (Settings → Playback): audio/subtitle language, subtitle size/position/colour/outline/bold/font, background box, skip/arrow jump (5–60 s), next-episode offer. In the player: playback speed (0.5×–2×) and a "Next episode in N s" card (countdown 5–30 s, default 10 s).

Your own `~/.config/mpv/mpv.conf` and scripts apply too; Lumen's settings are applied on top.

## Linux integration

- **Open with Lumen.** `.deb` declares video/audio/playlist types; `.m3u`/`.m3u8` files are added as sources; subtitle files are added to what's playing.
- **One instance.** Opening something while Lumen runs hands it to the running window.
- **Links.** `lumen://play?url=https://…` plays a public web address (Lumen asks first; local-network addresses are never opened this way).
- **Drag and drop** video/audio/playlist/subtitle files on the window.
- **Media keys** via MPRIS (play/pause/seek/volume/stop from GNOME/KDE widget or `playerctl`).
- **Package.** `.deb` depends on `mpv`, recommends `ffmpeg`.
- **Keychain** (sync key/API keys): Secret Service on Linux; bare window-manager sessions need GNOME Keyring or KWallet.

## Casting to a TV

Player → **Cast…** lists UPnP/DLNA devices on your network and sends what's playing. Lumen stops its own player and shows a small control bar while the device plays.

- A web address is fetched by the device itself (streams needing special headers/sign-in may not play).
- A local file is served to that device only, under a random address, for the cast duration; media files only.
- Devices must be on the local network (public addresses ignored). Chromecast/AirPlay not supported. Subtitles added in Lumen are not sent.

## Network privacy

Off by default; no overhead while off. Turn on in **Settings → Account → Network privacy** with a SOCKS5/HTTP proxy address (+ credentials if needed). **Test connection** before relying on it.

Once on, Lumen's metadata requests, mpv playback, yt-dlp extraction, and remote images route through a small local relay Lumen runs itself — the upstream proxy's address/credentials are never handed to mpv or yt-dlp directly. If the relay can't start or the proxy stops working, Lumen refuses those requests rather than falling back to direct.

Local-network sources always connect directly (like a VPN split tunnel). Casting and the "Open on GitHub" link are not covered.

## Sync

Optional; no accounts. The library/files/playback work without it. Current releases have no sync server baked in — the official server is wired in at build time through `LUMEN_SYNC_URL` and that variable is not set yet — so Sync asks for a server address. Self-host your own (see below), or set the variable when building.

Turn on in **Settings → Profiles & sync**. Lumen makes a random secret key on that device (shown once, 14 groups of four chars with a checksum). On another device choose **Add this device** and type the key. Nothing else exchanged: no email, no sign-in, no expiring code.

Synced devices share: profiles (name/colour; PINs never leave the device), favorites, watch progress. Deletions sync too (last-write-wins by timestamp). Playlists, imported sources, local files and thumbnails stay on this device.

**What the server can/cannot see:** every record is encrypted on-device (XChaCha20-Poly1305) before upload. The server sees a random group id, last-used time, record count/sizes, opaque ids/timestamps, and your network address — but not stream URLs, titles, profile names, positions, record kinds, or the sync key.

**Things to know:**
- No account = no recovery. Lose the key and every device that has it, and the synced copy is gone (your library on each computer is unaffected).
- Anyone with the key can read/change synced data. Only type it into your own devices.
- **Turn off** forgets the key on this device; **Delete synced data** erases the server copy for everyone.
- Key is in the OS keychain, never the settings DB. Server address must start with `https://` (plain `http://` only for same-computer dev).
- Abuse limits replace sign-in: proof of work to create a group, per-network daily cap, per-group size limits.

### Running your own sync server

A small Cloudflare Worker with a D1 database in `sync-server/`. Deploy: `npx wrangler d1 create lumen-sync`, put the database id in `sync-server/wrangler.jsonc`, `npx wrangler deploy`. Enter its address in **Settings → Profiles & sync → Sync server**.

## Translating Lumen

Interface text lives in a message catalog (`src/i18n/messages/`, assembled in `src/i18n/en.ts`), shown with `t("key")` / `tn("key", count)` / `{name}` / `<Rich>`. To add a language, write a `Catalog` and register it with `addLocale("de", catalog)`. Page starts in the language from Settings → Appearance, else the system's best match, else English. Only English exists so far; plural rules beyond one/other, word order and text length are unverified.

`npm test` runs a gate (`src/i18n/gate.test.ts`) that fails on hard-coded JSX text or unused catalog messages. `npm run i18n:hits -- <file>` lists what a file still hard-codes. Not translated: backend error text, genre/category labels from playlist group names, and text from your own playlists/files.

## Development

See [lumen/ARCHITECTURE.md](lumen/ARCHITECTURE.md) for the shell/core split, playback/sync, network boundaries, and how a PR becomes a release.

Needs Node 24+, Rust (stable), Tauri/WebKitGTK build deps, `mpv`, and `yt-dlp` (or *Update extractor* in Settings). On Debian/Ubuntu: `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev`.

```bash
cd lumen
npm ci
npm run tauri dev
```

Layout:

| Path | What it is |
| --- | --- |
| `lumen/src` | React interface + message catalog (`src/i18n`). |
| `lumen/src-tauri` | Tauri shell: windows, mpv control, MPRIS, in-window video, commands. |
| `lumen/crates/lumen-core` | UI-agnostic core: playlists, SQLite, search, TMDB, extraction, subtitles, sync, casting, updates, network guard. |
| `lumen/sync-server` | Optional sync server (Cloudflare Worker + D1). |
| `lumen/packaging` | Flatpak manifest, Arch `PKGBUILD`, desktop file, app data. |
| `.github/workflows` | CI, packaging, release workflows. |

`npx tauri build --bundles deb,appimage` (from `lumen/`) produces `.deb` + AppImage in `target/release/bundle/`. Pushing a tag `v0.1.0` runs `release.yml`, which builds both and attaches them (with `SHA256SUMS`) to a published GitHub release. A tag with a suffix (`v0.2.0-rc.1`) becomes a pre-release (ignored by the updater). Check the version in `tauri.conf.json`/`Cargo.toml` matches the tag first. The repo variable `LUMEN_SYNC_URL` bakes in a default sync server for release builds.

## Checks

```bash
cd lumen
npm test
npm run typecheck
npm run lint
npm run build
cargo fmt --check
cargo clippy -p lumen-core --all-targets -- -D warnings
cargo test -p lumen-core
(cd src-tauri && cargo test --lib)
```

`cargo test --lib` needs Tauri build deps, plus `ffmpeg` (poster tests) and `mpv` (playback tests; skip with a message when missing).

Sync server checks: from `lumen/sync-server/`, `npm run typecheck` checks types; `npm run dev` starts it locally; `npm run test:sync` runs plain-HTTP checks (group creation/proof of work, auth, group isolation, last-write-wins, paging, quotas, deletion). Rust side tested against an in-process server (`cargo test -p lumen-core sync`).

See [lumen/AUDIT.md](lumen/AUDIT.md) for verified behavior, security boundaries, and open limitations.

## Contributing

Issues and pull requests welcome. See [lumen/CONTRIBUTING.md](lumen/CONTRIBUTING.md) — one logical unit per PR, checks per area, running the E2E suite locally, and what to verify in the PR description.

## License

Lumen is released under the [MIT License](LICENSE). Copyright (c) 2026 Lumen Contributors.

Lumen starts, but does not include, `mpv` (GPL/LGPL), `ffmpeg` and `yt-dlp` (Unlicense); they keep their own licenses. The Flatpak bundles `mpv`, `ffmpeg` and `libass` built from source under theirs.
