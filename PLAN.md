# Lumen — Linux Media Player

**Status:** working plan · Oct 2026
**Target:** Linux-first (Wayland and X11)
**Stack:** Tauri v2 · React · Rust · libmpv · yt-dlp

---

## 1. Product

A native Linux media player that:

1. Plays **local files and folders**
2. Plays **user-provided M3U / M3U8 playlists** (file or URL)
3. **Extracts stream URLs** from a page the user pastes (yt-dlp + HTML harvest)
4. Optionally imports **public free-to-air catalogs** (iptv-org country / category slices)

Sources are always something the user pointed at, or a known public FTA catalog. No bundled paid channels, no open-web IPTV crawler, no DRM bypass.

Working name: **Lumen**. Rename before release if you want.

---

## 2. Architecture

```
Tauri v2 app

  Frontend (React + TS)  ⇄ invoke / events ⇄  Rust core (src-tauri + lumen-core)
                                                ├── playlist / M3U, SQLite, sync, account
                                                ├── extraction (in-process): yt-dlp subprocess,
                                                │     HTML harvest, live probe (reqwest)
                                                └── mpv control ──► mpv (JSON IPC) ──► libmpv (Phase 3)
```

**Rule:** page-specific network work (extract, harvest, probe) lives in Rust `lumen-core` and goes through `netguard`. The UI is React. The video engine is mpv.

| Job | Owner |
|---|---|
| Window, IPC, FS, dialogs | Tauri / Rust |
| M3U / IPTV tag parse | Rust |
| Playlist DB, favorites, history | Rust + SQLite |
| Playback | mpv (JSON IPC first, in-process libmpv later) |
| yt-dlp extract, HTML M3U harvest, live-probe | Rust (`lumen-core`), yt-dlp as a subprocess |
| UI | React 19 + TypeScript + Vite |

Do not play media through `<video>` / WebKit. WebKitGTK will not reliably do MKV, HEVC, MPEG-TS, or broken HLS.

---

## 3. Playback

Engine: **mpv**. Integration is staged so the app is usable before Linux window embedding is done.

`--wid` child-window embedding is X11-only. It does not work on native Wayland (Hyprland, GNOME Wayland). The public `tauri-plugin-libmpv` crate still does not embed on Linux. In-process video is custom work: EGL + X11 child / Wayland subsurface, or a GtkGLArea under a transparent WebKitGTK webview.

### 3.1 Phase 1 — JSON IPC (MVP)

Spawn:

```
mpv --input-ipc-server=/tmp/lumen-mpv.sock \
    --idle=yes --force-window=yes \
    --hwdec=auto-safe --vo=gpu-next \
    --keep-open=yes
```

Drive it with JSON IPC: `loadfile`, `set_property`, `seek`. Observe `pause`, `time-pos`, `duration`, `paused-for-cache`, `track-list`.

Fallback if VA-API misbehaves: `--hwdec=no`.

### 3.2 Phase 3 — in-process libmpv

Same `PlaybackBackend` trait, new implementation. Video fills the Tauri window; React chrome overlays it.

Study (do not rewrite from scratch):

- MaxVideoPlayer `crates/tauri-plugin-mpv` — EGL + X11 / Wayland subsurface
- Grid — GtkGLArea under transparent WebKitGTK (works on Hyprland)

```rust
#[async_trait]
trait PlaybackBackend: Send + Sync {
    async fn load(&self, url: &str, headers: Option<&[(&str, &str)]>) -> Result<(), PlayerError>;
    async fn pause(&self) -> Result<(), PlayerError>;
    async fn seek(&self, seconds: f64) -> Result<(), PlayerError>;
    async fn set_volume(&self, vol: i64) -> Result<(), PlayerError>;
    async fn set_property(&self, name: &str, value: MpvValue) -> Result<(), PlayerError>;
    async fn stop(&self) -> Result<(), PlayerError>;
}
```

HLS live: `--cache=yes`, tune demuxer backlog. Pass `#EXTVLCOPT` user-agent / referrer as mpv `http-header-fields`.

---

## 4. Extraction services (Rust)

### 4.1 Commands

- `extract_url` — runs yt-dlp on a URL the user pasted
- `harvest_page` — fetches **one** user-supplied page, pulls `.m3u` / `.m3u8` / obvious stream URLs from HTML / JSON-LD
- `probe_urls` — GET/HEAD a list of URLs, reports which look live

These live in `lumen-core` (`extract.rs`, `harvest.rs`, `probe.rs`) and are exposed to the frontend as Tauri commands. They do **not** parse M3U libraries, store playlists, or crawl the web. The frontend reaches them only through `invoke()`.

### 4.2 Execution model

- No separate process, no listening port, no bearer token. The only child process is yt-dlp.
- `extract_url` runs `yt-dlp -J --no-playlist` with format spec `YTDLP_FORMAT_SPEC`, a 30 s timeout (`EXTRACT_TIMEOUT`), and an 8 MiB stdout cap (`MAX_EXTRACT_JSON_BYTES`).
- yt-dlp resolution: the managed copy at `<app data>/bin/yt-dlp` first, then `yt-dlp` on `PATH`.
- `harvest_page` (20 s timeout, 5 MiB cap) and `probe_urls` (5 s per request, 12 s overall deadline) use `reqwest` through the `netguard` client.

The Python sidecar was removed in commit `bb4ccf4`. Its ready-line, token, stdin shutdown, and PyInstaller packaging went with it.

### 4.3 Stack

- `reqwest` and `tokio` (in `lumen-core`)
- `yt-dlp`: external binary, not bundled

Update yt-dlp when extractors rot (Settings → Update extractor).

### 4.4 Harvest rules

A harvest / extract request must include an explicit URL, or an allowlisted catalog URL.

Allowed:

- User pasted `https://…`
- User pasted a raw `.m3u` / `.m3u8` URL
- Optional built-in source: iptv-org country or category playlist URLs

Not in the product:

- “Search the web for BBC HD”
- Bundled lists of paid / cable channels
- Stolen Xtream credentials

---

## 5. Rust core

### 5.1 Modules

```
src-tauri/src/            Tauri shell
  main.rs          entry point
  lib.rs           Tauri builder, plugins, command registration
  commands.rs      #[tauri::command] wrappers
  playback/
    mod.rs         PlaybackBackend trait
    mpv_ipc.rs     JSON IPC client (MVP)
  account.rs, sync.rs   sign-in and sync glue

crates/lumen-core/src/    core logic
  playlist/
    m3u.rs         EXTINF / tvg-* / group-title / EXTVLCOPT
    import.rs      file + URL import
  db.rs            SQLite (sqlx)
  extract.rs       yt-dlp extraction (Phase 2)
  harvest.rs       HTML stream harvesting (Phase 2)
  probe.rs         live probing (Phase 2)
  netguard.rs      URL validation and SSRF guard
  sync.rs, account.rs, auth.rs, secrets.rs
  error.rs
```

`libmpv.rs` (Phase 3) does not exist yet.

### 5.2 SQLite

```sql
playlists(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  source TEXT NOT NULL,          -- 'file' | 'url' | 'extract' | 'catalog'
  origin TEXT,
  created_at INTEGER,
  updated_at INTEGER
);

channels(
  id INTEGER PRIMARY KEY,
  playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  url TEXT NOT NULL,
  group_title TEXT,
  tvg_id TEXT,
  tvg_logo TEXT,
  extra_headers TEXT,            -- JSON: user-agent, referrer
  last_ok INTEGER,
  last_error TEXT
);

favorites(channel_id INTEGER PRIMARY KEY);
history(
  id INTEGER PRIMARY KEY,
  channel_id INTEGER,
  path TEXT,
  position REAL,
  duration REAL,
  played_at INTEGER
);

settings(key TEXT PRIMARY KEY, value TEXT);
```

Index `(playlist_id, group_title)` and `channels.name`. Parse M3U on a `spawn_blocking` thread; insert in batches.

### 5.3 M3U parsing

Handle:

- `#EXTM3U` + `url-tvg`
- `#EXTINF:-1 tvg-id="…" tvg-logo="…" group-title="…",Name`
- `#EXTGRP`
- `#EXTVLCOPT:http-user-agent=…` / `http-referrer=…`
- quoted attributes, commas in names, `#EXTINF` without attributes

This is the **channel list**. HLS master/media playlists (`#EXT-X-STREAM-INF`) are playback URLs — mpv already understands those. Write the IPTV dialect in Rust (~200 lines). Do not round-trip it through an external process.

### 5.4 Commands

```ts
play_url(url: string, headers?: Record<string, string>)
pause()
seek(seconds: number)
set_volume(vol: number)
stop()

import_playlist_file(path: string)
import_playlist_url(url: string)
list_playlists()
list_channels(playlistId: number, query?: string, group?: string)

extract_url(url: string)
harvest_page(url: string)
probe_urls(ids: number[])

list_local_dir(path: string)
```

Playback state is events, not polling:

`player://state` → `{ paused, time, duration, buffering, title, error }`

---

## 6. Frontend

React 19 + TypeScript + Vite. Tailwind + a small component set (Radix / shadcn-style). No Next.js.

Screens:

1. **Player** — transport, volume, seek, now-playing, buffering. MVP: chrome in the Tauri window, video in the mpv window. Phase 3: chrome overlays the video.
2. **Library** — playlists, groups, search, virtualized channel list, logo, live-dot from last probe.
3. **Add source** — file picker, URL field, “extract this page”, optional catalog picker (country / category).
4. **Local files** — folder browser, last-played, resume position.
5. **Settings** — hwdec on/off, cache, extra mpv args, HTTP proxy, extractor (yt-dlp) version and update.

Notes:

- Virtualize lists (`@tanstack/react-virtual`). 10k channels will freeze a naive `.map`.
- Keyboard: Space pause, `f` fullscreen, arrows seek, `m` mute, `9`/`0` volume. Bind in the Tauri window so focus does not matter.
- All `invoke()` calls live in one module (`src/lib/tauri.ts`).

---

## 7. Layout

```
lumen/
├── src/                        # React frontend
│   ├── lib/
│   │   ├── tauri.ts            # every invoke()
│   │   └── types.ts
│   ├── components/
│   │   ├── Transport.tsx
│   │   ├── ChannelList.tsx
│   │   ├── SourceDialog.tsx
│   │   └── LocalBrowser.tsx
│   ├── hooks/
│   │   ├── usePlayer.ts
│   │   └── useChannels.ts
│   └── App.tsx
├── src-tauri/
│   ├── src/                    # §5.1
│   ├── capabilities/default.json
│   └── tauri.conf.json
├── crates/
│   └── lumen-core/             # extraction, playlist, DB, netguard
├── docs/
│   └── PLAN.md
└── package.json
```

---

## 8. Data flow

**User M3U URL**

```
UI ──import_playlist_url──► Rust ──GET──► playlist body
                              │
                              ▼
                         parse M3U → SQLite
                              │
                              ▼
                         UI lists channels
User clicks channel
                              │
UI ──play_url──────────────► Rust ──JSON IPC──► mpv loadfile
```

**Extract a page**

```
UI ──extract_url──► Rust ──yt-dlp -J subprocess──► (title, url, headers)
                                              │
                                              ▼
                                        {title, url, headers}
                                              │
                         Rust saves as a 1-item playlist, then play_url
```

**Local file**

```
UI ──play_url(path)──► Rust ──► mpv loadfile
```

---

## 9. Phases

### Phase 0 — bootstrap (half a day)

```bash
# Debian/Ubuntu
sudo apt install mpv libmpv-dev

# Arch
sudo pacman -S mpv

npm create tauri-app@latest lumen -- --template react-ts
```

Add plugins: `dialog`, `opener`.

### Phase 1 — MVP (~2 weeks)

Must be usable alone.

- Local file / folder playback through mpv JSON IPC
- Transport in the Tauri window driving mpv
- Import an M3U file and an M3U URL, show groups, click to play
- SQLite playlists + history resume
- Settings: hwdec toggle, extra mpv args

**Done when:** a local MKV and a public `.m3u8` both play from the UI on Hyprland and on GNOME.

### Phase 2 — extraction services (~1–2 weeks)

**Status: complete, implemented in Rust.** The Python sidecar was replaced by in-process Rust services (commit `bb4ccf4`), so the sidecar-process items below are superseded and do not apply.

- ~~FastAPI sidecar, ready-line, token, stdin shutdown~~ — superseded: no separate process, so no ready-line, bearer token, or stdin shutdown
- ~~PyInstaller `--onedir` hooked into `beforeBuildCommand`~~ — superseded: `beforeBuildCommand` is `npm run build` and nothing is bundled
- `extract_url`, `harvest_page`, `probe_urls` in `lumen-core` (`extract.rs`, `harvest.rs`, `probe.rs`): probe caps at 8 concurrent, 5s per request, same-host redirects only (`MAX_CONCURRENT_PROBES`, `PROBE_TIMEOUT`); extract and harvest validate URLs through `netguard`
- UI: Add source dialog + live-dot on channels (`live-indicator` / `dead-indicator`)
- Verified: `cargo test -p lumen-core` passes, and `npm run typecheck` passes

### Phase 3 — one window (~2 weeks)

- In-process libmpv or GtkGLArea overlay; React controls on top
- Buffering from `paused-for-cache`
- Audio / subtitle track picker (`track-list`)
- Fullscreen covering the video surface
- Live HLS cache flags

### Phase 4 — ship (~2 weeks)

- `.deb` with `Depends: mpv` (or `libmpv2`)
- AppImage: bundle `libmpv.so` + deps with `ldd` + `patchelf`
- AUR `PKGBUILD` (system mpv)
- Smoke on Ubuntu LTS, Fedora, Arch
- Crash log in `~/.local/share/lumen/` + “copy debug dump”

Not in v1: EPG / XMLTV, Xtream Codes (only later, and only for accounts the user owns).

---

## 10. Packaging

| Format | What to do |
|---|---|
| **.deb / .rpm** | Depend on distro `mpv` or `libmpv2` |
| **AppImage** | Bundle `libmpv.so.*` + FFmpeg/va libs via `patchelf --set-rpath`. Test VA-API on Intel and AMD |
| **AUR** | `depends=(mpv webkit2gtk-4.1)` |
| yt-dlp | Not bundled. Use the managed copy from Settings, or system `yt-dlp` |

Do not enable `bundleMediaFramework`. That is GStreamer for WebKit `<video>`, which this app does not use.

Capabilities:

```json
"permissions": [
  "core:default",
  "opener:default",
  "dialog:default"
]
```

The `fs` and `shell` plugins are not used, so they are not granted. Do not add a shell or filesystem plugin permission without a reason. Spawning yt-dlp and mpv happens in Rust, not through the webview.

---

## 11. Security

- No local server: extraction runs in-process, so nothing listens on a port
- The frontend reaches extraction only through Tauri commands
- `harvest` / `extract` only accept `http` / `https` (and `file` for local M3U)
- Probe: same-host redirects only, timeout 5s, max 8 in flight
- No telemetry. Crash reports later, opt-in (GDPR)
- Redact query strings when logging stream URLs (tokens live there)

---

## 12. Risks

| Risk | What to do |
|---|---|
| Linux mpv embedding is a time sink | MVP uses a separate mpv window. Embedding is Phase 3 |
| yt-dlp extractors break | Update the managed yt-dlp binary from Settings. Pin + update cadence |
| Public M3U links die constantly | Probe in the background, show `last_ok`, never block UI |
| VA-API fails on NVIDIA / some Wayland | Default `hwdec=auto-safe`, one-click software fallback |
| yt-dlp hangs or floods output | 30 s timeout and 8 MiB output cap on every run |
| Giant playlists freeze UI | Virtualize, parse in `spawn_blocking`, batch SQLite inserts |

---

## 13. Libraries

| Need | Use |
|---|---|
| Shell | Tauri v2 |
| UI | React 19 + TypeScript + Vite + Tailwind |
| Playback MVP | system `mpv` + JSON IPC (thin tokio Unix-socket client) |
| Playback Phase 3 | custom libmpv plugin (MaxVideoPlayer Linux renderer or GtkGLArea overlay) |
| DB | `sqlx` (SQLite) |
| HTTP in Rust | `reqwest` |
| Extraction | yt-dlp (external binary) + `reqwest` |
| Dialogs | `tauri-plugin-dialog` |

---

## 14. v1 done when

- [ ] Plays local video/audio on Arch (Hyprland) and Ubuntu
- [ ] Imports a 2k-entry M3U without locking the UI
- [ ] Plays an HLS `.m3u8` via mpv
- [ ] Extracts a YouTube URL through yt-dlp and plays it
- [ ] Harvests M3U links from a user-pasted HTML page
- [ ] A failed extraction shows a toast and the app keeps playing
- [ ] `.deb` and AppImage install on a clean VM
- [ ] No inbound port on `0.0.0.0`, no pirate playlist in the repo

---

## 15. First commands

```bash
sudo pacman -S mpv          # or: sudo apt install mpv libmpv-dev

npm create tauri-app@latest lumen -- --template react-ts
cd lumen
```

Start with a Tauri button that tells mpv to `loadfile` a local video. If that loop is solid, everything else is I/O.
