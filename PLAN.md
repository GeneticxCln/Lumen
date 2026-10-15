# Lumen — Linux Media Player

**Status:** working plan · Oct 2026
**Target:** Linux-first (Wayland and X11)
**Stack:** Tauri v2 · React · Rust · libmpv · Python sidecar

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
┌──────────────────────────────────────────────────────────────────┐
│                         Tauri v2 app                             │
│                                                                  │
│  ┌─────────────┐   invoke / events    ┌───────────────────────┐  │
│  │  Frontend   │ ◄──────────────────► │   Rust core           │  │
│  │  React + TS │                      │                       │  │
│  │             │                      │  - playlist / M3U     │  │
│  │  player UI  │                      │  - SQLite             │  │
│  │  library    │                      │  - sidecar lifecycle  │  │
│  │  sources    │                      │  - mpv control        │  │
│  │  settings   │                      │  - file dialogs       │  │
│  └─────────────┘                      └──────────┬────────────┘  │
│                                                  │               │
│                         ┌────────────────────────┼────────────┐  │
│                         ▼                        ▼            │  │
│                  ┌─────────────┐          ┌─────────────┐     │  │
│                  │  mpv        │          │  Python     │     │  │
│                  │  JSON IPC   │          │  sidecar    │     │  │
│                  │  → libmpv   │          │  127.0.0.1  │     │  │
│                  └─────────────┘          │             │     │  │
│                                           │  yt-dlp     │     │  │
│                                           │  harvest    │     │  │
│                                           │  probe      │     │  │
│                                           └─────────────┘     │  │
└──────────────────────────────────────────────────────────────────┘
```

**Rule:** if it touches the network in a page-specific way, it is Python. Everything else is Rust. The UI is React. The video engine is mpv.

| Job | Owner |
|---|---|
| Window, IPC, FS, dialogs | Tauri / Rust |
| M3U / IPTV tag parse | Rust |
| Playlist DB, favorites, history | Rust + SQLite |
| Playback | mpv (JSON IPC first, in-process libmpv later) |
| yt-dlp extract, HTML M3U harvest, live-probe | Python sidecar |
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

## 4. Python sidecar

### 4.1 API

- `POST /extract` — yt-dlp on a URL the user pasted
- `POST /harvest` — fetch **one** user-supplied page, pull `.m3u` / `.m3u8` / obvious stream URLs from HTML / JSON-LD
- `POST /probe` — HEAD/GET a list of URLs, report which look live
- `GET  /health`

Does **not** parse M3U libraries, store playlists, crawl the web, or talk to the frontend. Rust is the only client.

### 4.2 Process contract

```
Rust spawns sidecar
  → sidecar prints one JSON line on stdout:
     {"ready": true, "port": 43127, "token": "…"}
  → all HTTP calls send Authorization: Bearer <token>
  → on exit: write "SHUTDOWN\n" to stdin, wait 2s, kill the process group
```

- Bind `127.0.0.1` only
- Random free port
- PyInstaller **`--onedir`**, named with the Rust target triple:

  `src-tauri/binaries/extractor-x86_64-unknown-linux-gnu/`

  `--onefile` is unusable here: Tauri sees the bootloader PID, `kill()` leaves the real Python child running.

### 4.3 Stack

```
fastapi
uvicorn
httpx
beautifulsoup4
yt-dlp
lxml
```

Pin yt-dlp. Rebuild the sidecar when extractors rot.

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
src-tauri/src/
  main.rs          Tauri builder, plugins, sidecar spawn/shutdown
  playback/
    mod.rs         PlaybackBackend trait
    mpv_ipc.rs     JSON IPC client (MVP)
    libmpv.rs      in-process (Phase 3)
  playlist/
    m3u.rs         EXTINF / tvg-* / group-title / EXTVLCOPT
    import.rs      file + URL import
  db.rs            SQLite
  sidecar.rs       spawn, ready-line, HTTP client, shutdown
  commands.rs      all #[tauri::command]
  error.rs
```

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

This is the **channel list**. HLS master/media playlists (`#EXT-X-STREAM-INF`) are playback URLs — mpv already understands those. Write the IPTV dialect in Rust (~200 lines). Do not round-trip it through Python.

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
5. **Settings** — hwdec on/off, cache, extra mpv args, HTTP proxy, sidecar log.

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
│   ├── binaries/               # gitignored sidecar build
│   ├── capabilities/default.json
│   └── tauri.conf.json
├── sidecar/
│   ├── app/
│   │   ├── main.py
│   │   ├── extract.py
│   │   ├── harvest.py
│   │   └── probe.py
│   ├── pyproject.toml
│   └── scripts/build.sh        # PyInstaller --onedir + target triple
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
UI ──extract_url──► Rust ──HTTP+token──► sidecar /extract (yt-dlp)
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

Add plugins: `dialog`, `fs`, `shell`. Python 3.12 venv under `sidecar/`.

### Phase 1 — MVP (~2 weeks)

Must be usable alone.

- Local file / folder playback through mpv JSON IPC
- Transport in the Tauri window driving mpv
- Import an M3U file and an M3U URL, show groups, click to play
- SQLite playlists + history resume
- Settings: hwdec toggle, extra mpv args

**Done when:** a local MKV and a public `.m3u8` both play from the UI on Hyprland and on GNOME.

### Phase 2 — sidecar (~1–2 weeks)

- FastAPI sidecar, ready-line, token, stdin shutdown
- PyInstaller `--onedir` hooked into `beforeBuildCommand`
- `/extract`, `/harvest`, `/probe` (cap 8 concurrent, 5s timeout, no off-host redirects)
- UI: Add source dialog + live-dot on channels

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
| Python sidecar | Ship the `--onedir` tree next to the binary. No system Python |

Do not enable `bundleMediaFramework`. That is GStreamer for WebKit `<video>`, which this app does not use.

Capabilities:

```json
"permissions": [
  "core:default",
  "dialog:default",
  "fs:allow-read",
  {
    "identifier": "shell:allow-spawn",
    "allow": [
      { "name": "binaries/extractor", "sidecar": true },
      { "name": "mpv", "args": true }
    ]
  }
]
```

Do not grant `shell:allow-execute` on the whole PATH.

---

## 11. Security

- Sidecar binds `127.0.0.1`, random port, bearer token from the ready-line
- Frontend never calls the sidecar; only Rust does
- `harvest` / `extract` only accept `http` / `https` (and `file` for local M3U)
- Probe: same-host redirects only, timeout 5s, max 8 in flight
- No telemetry. Crash reports later, opt-in (GDPR)
- Redact query strings when logging stream URLs (tokens live there)

---

## 12. Risks

| Risk | What to do |
|---|---|
| Linux mpv embedding is a time sink | MVP uses a separate mpv window. Embedding is Phase 3 |
| yt-dlp extractors break | Sidecar is independently rebuildable. Pin + update cadence |
| Public M3U links die constantly | Probe in the background, show `last_ok`, never block UI |
| VA-API fails on NVIDIA / some Wayland | Default `hwdec=auto-safe`, one-click software fallback |
| PyInstaller `--onefile` zombies | `--onedir` + stdin `SHUTDOWN` + kill process group |
| Giant playlists freeze UI | Virtualize, parse in `spawn_blocking`, batch SQLite inserts |

---

## 13. Libraries

| Need | Use |
|---|---|
| Shell | Tauri v2 |
| UI | React 19 + TypeScript + Vite + Tailwind |
| Playback MVP | system `mpv` + JSON IPC (thin tokio Unix-socket client) |
| Playback Phase 3 | custom libmpv plugin (MaxVideoPlayer Linux renderer or GtkGLArea overlay) |
| DB | `sqlx` (sqlite) or `rusqlite` |
| HTTP in Rust | `reqwest` |
| Sidecar | FastAPI + uvicorn + yt-dlp + httpx + bs4 |
| Sidecar pack | PyInstaller `--onedir` |
| Dialogs | `tauri-plugin-dialog` |

---

## 14. v1 done when

- [ ] Plays local video/audio on Arch (Hyprland) and Ubuntu
- [ ] Imports a 2k-entry M3U without locking the UI
- [ ] Plays an HLS `.m3u8` via mpv
- [ ] Extracts a YouTube URL through the sidecar and plays it
- [ ] Harvests M3U links from a user-pasted HTML page
- [ ] Survives sidecar crash (Rust restarts it, UI shows a toast)
- [ ] `.deb` and AppImage install on a clean VM
- [ ] No inbound port on `0.0.0.0`, no pirate playlist in the repo

---

## 15. First commands

```bash
sudo pacman -S mpv          # or: sudo apt install mpv libmpv-dev

npm create tauri-app@latest lumen -- --template react-ts
cd lumen

python3.12 -m venv sidecar/.venv
```

Start with a Tauri button that tells mpv to `loadfile` a local video. If that loop is solid, everything else is I/O.
