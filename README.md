# Lumen

[![CI](https://github.com/GeneticxCln/Lumen/actions/workflows/ci.yml/badge.svg)](https://github.com/GeneticxCln/Lumen/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A Linux-first media library and player for **your own** sources. Lumen keeps the playlists, streams and files you point it at in one place, plays them with [mpv](https://mpv.io), remembers where you stopped, follows series, finds subtitles without an account, and can send what is playing to a TV on your network. There are no accounts, no ads and no bundled channels.

Built with Tauri v2, React 19, TypeScript, Rust, SQLite and native mpv.

## Contents

- [Features](#features) · [Install](#install) · [Updates](#updates) · [Subtitles](#subtitles) · [Player settings](#player-settings) · [Linux integration](#linux-integration) · [Casting to a TV](#casting-to-a-tv) · [Sync](#sync) · [Translating Lumen](#translating-lumen)
- [Development](#development) · [Checks](#checks) · [Contributing](#contributing) · [License](#license)

## Features

- **Your sources, one library.** Add M3U/M3U8 playlists (file or address), pages to extract a stream from (through a verified `yt-dlp`), and local folders. Movies, series and live TV are sorted from names and groups; there is no catalog service behind it.
- **Discovery that works offline.** Video thumbnails from local FFmpeg, folders, audio and playlists, resume points per file, favorites, recently watched and recommendations from your own history, a poster-frame chooser.
- **Series state.** Watched marks per episode, the next episode, upcoming and new episodes (with an optional TMDB key), and opt-in desktop notifications when a followed series airs.
- **Search** across your sources and files from the top bar (Ctrl/Cmd + K).
- **Native playback with mpv.** Your own `mpv.conf` and scripts apply. Optional video inside the Lumen window, on X11 and native Wayland.
- **Subtitles** from the file, next to the file, or found online with no account or API key.
- **Profiles** with optional PINs, per-profile language, subtitle and playback preferences.
- **Encrypted sync, no accounts**, between your own devices.
- **Casting** to UPnP/DLNA TVs and receivers.
- **Themes** (Cinema, Midnight, Forest, Daylight, System), keyboard shortcuts, a message catalog ready for translations.
- **Updates from GitHub releases** with one click.

Not included, on purpose: a Netflix-style catalog, paid-channel access, DRM bypass, or an external recommendation service. Artwork fallbacks are original local SVGs.

## Install

Releases are published at <https://github.com/GeneticxCln/Lumen/releases>. Lumen starts `mpv` from your `PATH` rather than bundling it, so install `mpv` (and `ffmpeg` for thumbnails). `yt-dlp` is downloaded and verified by Lumen itself (Settings → Metadata → Update extractor).

| Package | How |
| --- | --- |
| **AppImage** | Download `lumen_<version>_amd64.AppImage`, `chmod +x` it and run it. This is the only package that updates itself. |
| **.deb** | `sudo apt install ./lumen_<version>_amd64.deb` (depends on `mpv`, recommends `ffmpeg`). |
| **Flatpak** | Built by CI from `lumen/packaging/flatpak/` as a bundle (includes mpv); not on Flathub. |
| **Arch** | `lumen/packaging/aur/PKGBUILD` builds from the tagged source. |

Build status: the `.deb` and AppImage have been built and inspected, but none of the packages has been installed on a clean machine yet, and no release has been published. See [lumen/AUDIT.md](lumen/AUDIT.md) for exactly what is and is not verified.

## Troubleshooting

- **"mpv stopped before it was ready" / "did not open its control socket".** The message now includes the last lines mpv printed. Run `LUMEN_DEBUG_MPV=1 lumen` (or `LUMEN_DEBUG_MPV=1 ./lumen_*.AppImage`) to see everything mpv says. Check that `mpv` runs on its own (`mpv --version`). Programs Lumen starts do not inherit the AppImage's own library paths.
- **Scrolling is choppy.** Lumen asks the page engine for smooth, hardware-accelerated scrolling. If it is still slow, try `LUMEN_WEBKIT_SAFE=1 lumen` (turns off WebKitGTK's DMA-BUF renderer, which some NVIDIA and Wayland setups handle badly). The AppImage runs through XWayland on a Wayland session; `GDK_BACKEND=wayland ./lumen_*.AppImage` uses native Wayland.

## Updates

Settings → **About & updates** shows the version and has one button, **Update to the latest version**. Click it and Lumen looks up the newest published release of this repository on GitHub, and if its tag is higher than the version you are running, downloads that release, checks it and installs it. A banner then offers a restart. There is no account, no signing key and no update server: GitHub's releases are the update channel.

- **AppImage**: the file for your machine is downloaded next to the running AppImage, checked against the release's `SHA256SUMS`, and swapped in with a single rename. The running copy keeps working until you restart.
- **Anything else** (a `.deb`, a Flatpak, the Arch package, a build you made): Lumen tells you a newer version exists and where it is; the system that installed it does the updating. The button reads *Check for updates* there.
- **Automatic checks.** At start (after about 20 seconds, then every few hours, at most once a day) Lumen looks by itself. The same page chooses what it does with a newer release: *install it automatically* (the default), *only tell me*, or *never look*.
- **What is fetched.** Only the release's own files, from `github.com` and GitHub's file hosts. A file that does not match its checksum, is larger than announced or is not a program is deleted and nothing is replaced. Drafts and pre-releases are never offered, so a release reaches users when it is **published**, not when its tag is pushed.

The checksum protects against a damaged or truncated download. It does not prove who published the file: whoever can publish a release to the repository can publish an update, because the checksums come from the same place as the files.

## Subtitles

Local subtitle handling needs nothing from the internet: tracks inside the file, a `.srt`/`.ass` file next to a local video (loaded automatically), drag and drop, and a file you pick in the player. Settings → Playback sets the preferred language, size and position per profile, and the player adjusts delay and size while playing.

**Find subtitles online** (the *Find…* button in the player's subtitle tools) needs no account and no API key. It is off until you agree, and it only searches when you press Search; picking a result saves the file on this computer and loads it automatically the next time you play that title (no network). What it uses:

- **Stremio's OpenSubtitles mirror** (`opensubtitles-v3.strem.io`) for movies and series, found by IMDb id. The id comes from TMDB when you have set a TMDB token, otherwise from IMDb's public suggestion list.
- **Gestdown** (`api.gestdown.info`, a mirror of Addic7ed) for TV episodes.
- Subtitles a web page offers itself, for entries added from a page (through yt-dlp); they are picked by your preferred subtitle language.

A search sends the title's name, year, season and episode, the language, and for a local file a fingerprint of it (the OpenSubtitles hash, never the file) to those services. Each service can be switched off in Settings → Playback. Downloads are limited to each service's own servers and 10 MB, checked to really be subtitles (an error page is refused), converted to UTF-8 and kept in a 50 MB cache. These services are run by other people and are not official interfaces for apps like Lumen, so they can change or stop working; Gestdown covers TV only. OpenSubtitles' own old API is not used: it was shut down for third-party apps.

## Player settings

Per profile, in Settings → Playback: preferred audio and subtitle language, subtitle size, position, text and outline colour, an optional background box, bold and font (these style text subtitles; styled `.ass` subtitles keep their author's look), how far the skip buttons and arrow keys jump (5 to 60 seconds), and the next-episode offer. In the player: playback speed (0.5× to 2×, back to normal for the next file) and, for series, a card in the last seconds of an episode ("Next episode in 10 s", with Play now and Cancel) that starts the next episode when this one ends. The countdown can be set to 5–30 seconds or turned off; the default is 10 seconds.

Lumen starts the regular `mpv`, so your own `~/.config/mpv/mpv.conf` and scripts apply too; Lumen's settings are applied on top, and "Extra mpv arguments" in Settings → Playback can add more.

## Linux integration

- **Open with Lumen.** The `.deb` declares the common video, audio and playlist types, so Lumen appears in "Open with". Video and audio files play, `.m3u`/`.m3u8` files are added as sources, and a subtitle file is added to what is playing.
- **One window, one instance.** Opening something while Lumen is running hands it to the running window (and brings it forward) instead of starting a second copy.
- **Links.** `lumen://play?url=https://…` plays a public web address. Because any web page can trigger such a link, Lumen always asks before playing it, and addresses on your local network are never opened this way.
- **Drag and drop.** Drop video, audio, playlist or subtitle files on the window.
- **Media keys.** Lumen registers with MPRIS, so media keys, the GNOME/KDE media widget and `playerctl` can see what is playing and play, pause, seek, change the volume and stop it.
- **Package.** The `.deb` depends on `mpv` and recommends `ffmpeg` (used for thumbnails).
- The OS keychain (used for the sync key and API keys) is the Secret Service on Linux; a bare window-manager session needs GNOME Keyring or KWallet running.

### Playing inside the window (experimental)

By default video plays in a separate mpv window. **Settings → Playback → Play video inside the Lumen window** puts the video in the main window instead, above the controls. It is off by default and works in two ways:

- **X11 / XWayland** (`GDK_BACKEND=x11 lumen`): mpv draws into a native area above the page.
- **Native Wayland**: Lumen loads `libmpv` (the `libmpv2` package on Debian and Ubuntu, `mpv-libs` on Fedora, `mpv` on Arch) at run time and draws the video into an OpenGL area in the window. Nothing is needed at build time, and without `libmpv` Settings says so and the separate window is used. The same mpv settings, subtitles, media keys and controls apply; Lumen talks to the in-window player through the same control socket.

Controls are shown beside the video, not drawn over it. Fullscreen in this mode makes the whole window fullscreen. `LUMEN_GL_DEBUG=1` prints the size of the picture and how much of it is lit, for diagnosing a black video.

## Casting to a TV

In the player's settings, **Cast…** lists TVs, receivers and speakers on your network that accept video over UPnP / DLNA and sends what is playing to the one you pick. Lumen then stops its own player and shows a small control bar (play, pause, skip, stop) while the device plays it.

- A web address is fetched by the device itself, so streams that need special headers or a sign-in may not play there. A title already playing in Lumen starts where it was.
- A file on this computer is served to that one device only, under a random address, for as long as the cast lasts. Only media files are served.
- Devices must be on the local network: anything that announces a public address is ignored, and addresses on your network are only used as sources if you turned on local-network sources in Settings.
- Chromecast and AirPlay are different protocols and are not supported. Subtitles added in Lumen are not sent to the device.

## Sync

Sync is optional and there are no accounts. The library, files, and playback all work without it.

Turn it on in **Settings → Profiles & sync**. Lumen makes a random secret key on that device and shows it once (14 groups of four characters, with a checksum so a typo is caught). On another device choose **Add this device** and type the key. Nothing else is exchanged: no email, no sign-in page, no code that can expire.

Synced devices share:

- **Profiles**: name and colour. PIN codes never leave the device.
- **Favorites**: the stream URL, its title key, and the profile it belongs to.
- **Watch progress**: the stream URL, resume position, and duration.

Deletions sync too. Conflicts resolve last-write-wins by timestamp. Playlists and imported sources are not synced and stay on this device, as do local files and their thumbnails.

### What the server can and cannot see

Every record is encrypted on the device (XChaCha20-Poly1305) before it is uploaded. The key is split with HKDF into separate keys for the group name, for proving you belong to the group, for encryption, and for naming records, so the server learns the first two uses at most and can never decrypt.

| The server can see | The server cannot see |
| --- | --- |
| a random group id, and when a group was last used | stream URLs, titles, profile names, positions |
| how many records there are and how large each is | what kind of record each one is |
| an opaque id and a timestamp per record, and whether it is a deletion | the sync key |
| your network address, as any web server does | |

A server can also refuse to answer, drop records, or serve an older valid copy of a record. It cannot alter or move a record without the app noticing: such records fail verification, are ignored, and are reported in Settings.

### Things to know

- **No account means no recovery.** If you lose the key and every device that has it, the synced copy is gone. Your library on each computer is not affected.
- Anyone who has the key can read and change the synced data. **Show key** displays it again; only type it into your own devices.
- **Turn off** forgets the key on this device and leaves the synced copy for other devices. **Delete synced data** also erases the copy from the server for everyone.
- The key is kept in the OS keychain, never in the settings database. The server address must start with `https://` because the app sends a bearer credential to it (plain `http://` is accepted only for a server on the same computer, for development).
- Abuse limits replace sign-in: a proof of work to create a group, a per-network daily cap, and size limits per group. They slow abuse down; they do not stop a determined attacker.
- Data synced by older versions through the Freebuff account is not migrated. Turn sync on, and this device uploads what is in its local library.

### Running your own sync server

The server is a small Cloudflare Worker with a D1 database in `sync-server/`. Deploy it with `npx wrangler d1 create lumen-sync`, put the database id in `sync-server/wrangler.jsonc`, and run `npx wrangler deploy`. Enter its address in **Settings → Profiles & sync → Sync server**, or build Lumen with `LUMEN_SYNC_URL=https://your-server` to make it the default.

## Translating Lumen

All interface text lives in a message catalog (`src/i18n/messages/`, one file per area, put together in `src/i18n/en.ts`) and screens show it with `t("key")` (`tn("key", count)` for plurals, `{name}` for values, `<strong>…</strong>` markup through `<Rich>` or `tRich`). A wrong key or a missing value is a type error. To add a language, write a `Catalog` for it (the type lists every key) and register it with `addLocale("de", catalog)`. The page starts in the language chosen in Settings → Appearance (shown once a second language exists), else the system's best match (`de-AT` finds `de`), else English. Language names come from `Intl.DisplayNames`. Only English exists, so nothing has been tested against a real second language: plural rules beyond one/other, word order and text length are unverified.

`npm test` runs a gate (`src/i18n/gate.test.ts`) that fails if a component gains hard-coded text (JSX text and the `aria-label`, `title`, `placeholder`, `alt` and `label` attributes) or if a catalog message is unused. `npm run i18n:hits -- <file>` lists what a file still hard-codes. Not translated: messages that come from the backend (error text), genre and category labels derived from English words in playlist group names (`lib/discovery.ts`), and text from your own playlists and files.

## Development

Needs Node 24+, Rust (stable), the platform's Tauri/WebKitGTK build dependencies, `mpv`, and `yt-dlp` for extraction (or use *Update extractor* in Settings). On Debian/Ubuntu the libraries are `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev`.

```bash
cd lumen
npm ci
npm run tauri dev
```

Layout:

| Path | What it is |
| --- | --- |
| `lumen/src` | The React interface and the message catalog (`src/i18n`). |
| `lumen/src-tauri` | The Tauri shell: windows, mpv control, MPRIS, in-window video, commands. |
| `lumen/crates/lumen-core` | The UI-agnostic core: playlists, SQLite, search, TMDB, extraction, subtitles, sync, casting, updates, network guard. Builds and tests without GTK. |
| `lumen/sync-server` | The optional sync server (a Cloudflare Worker with D1). |
| `lumen/packaging` | Flatpak manifest, Arch `PKGBUILD`, desktop file and app data. |
| `.github/workflows` | CI, packaging and release workflows. |

`npx tauri build --bundles deb,appimage` (from `lumen/`) produces a `.deb` and an AppImage in `target/release/bundle/`. Pushing a tag such as `v0.1.0` runs `.github/workflows/release.yml`, which builds both and attaches them, with a `SHA256SUMS` file, to a published GitHub release (a tag with a suffix such as `v0.2.0-rc.1` becomes a pre-release, which the updater ignores). Lumen updates itself from the latest published release, so a tag is a release: check the version in `tauri.conf.json` and `Cargo.toml` matches it first. Set the repository variable `LUMEN_SYNC_URL` to bake in a default sync server.

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

`cargo test --lib` needs the Tauri build dependencies, plus `ffmpeg` for the poster tests and `mpv` for the playback tests (those skip with a message when `mpv` is missing).

The sync server has its own checks. From `lumen/sync-server/`, `npm run typecheck` checks the types, and `npm run dev` starts it locally with small limits (in another terminal, `npm run test:sync` runs plain-HTTP checks against it: group creation and proof of work, authentication, isolation between groups, last-write-wins, paging, quotas and deletion). The Rust side is tested against an in-process server that speaks the same API (`cargo test -p lumen-core sync`).

See [lumen/AUDIT.md](lumen/AUDIT.md) for verified behavior, security boundaries and open limitations.

## Contributing

Issues and pull requests are welcome. Run the checks above before opening a pull request; CI runs the same ones. New interface text goes in the message catalog (a test fails on hard-coded text). Describe in the pull request what you verified and what you could not.

## License

Lumen is released under the [MIT License](LICENSE). Copyright (c) 2026 Lumen Contributors.

Lumen starts, but does not include, `mpv` (GPL/LGPL), `ffmpeg` and `yt-dlp` (Unlicense); they keep their own licenses. The Flatpak bundles `mpv`, `ffmpeg` and `libass` built from source under theirs.
