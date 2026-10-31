# Architecture

A deeper look than [AUDIT.md](AUDIT.md)'s one-paragraph summary. AUDIT.md is the source of truth for what's actually verified; this document explains the design those verifications are checking.

## The shell/core split

```
src/              React UI — talks only to Tauri commands, never to the network or disk directly
  ↓ invoke() / events
src-tauri/        Tauri shell: windows, mpv process control, MPRIS, in-window video,
                  #[tauri::command] wrappers (commands.rs) over lumen-core
  ↓
crates/lumen-core/  UI-agnostic core: playlists, SQLite, search, TMDB, extraction, subtitles,
                    sync, casting, updates, network guard. Builds and tests without GTK.
```

`lumen-core` doesn't know Tauri exists. `commands.rs` is a thin translation layer: it resolves the database pool, calls into `lumen-core`, and maps its `Result` into whatever the frontend expects. This split is why `cargo test -p lumen-core` runs in a few seconds without a display — the logic worth unit-testing doesn't depend on a window existing.

The React side never makes its own network requests for anything the backend could do instead (TMDB art, `tvg-logo` images) — see `fetch_remote_image` below. Keeping all outbound traffic in Rust is what lets `netguard` and the network-privacy relay be the single choke point instead of two.

## Playback: a separate process, not an embedded widget

Lumen doesn't play video through WebKit's `<video>` element — WebKitGTK doesn't reliably handle MKV, HEVC, MPEG-TS, or a broken HLS stream, and bundling a codec pack for it is its own maintenance burden. Instead:

- A real `mpv` process is spawned, controlled over its own JSON IPC socket (`src-tauri/src/playback/mpv_ipc.rs`). `loadfile`, `set_property`, `seek` go out; `pause`, `time-pos`, `duration`, `track-list` come back as events, forwarded to the frontend as `player://state` — the player is driven by events, not polled.
- On Linux, the video can also render in-process (`src-tauri/src/embed.rs`), into a GTK GL area under the webview, for a single-window experience; `embed_unsupported.rs` is the no-op fallback on other platforms. Either way the control path is the same `PlaybackBackend` trait and the same IPC socket.
- A dropped HTTP/HLS stream gets two independent recovery layers: mpv's own libavformat reconnect flags (`--stream-lavf-o=reconnect=1,...`, a no-op for local files) try first; if the app sees the stream fail anyway, `usePlayer.ts` retries the same URL a few times with backoff before falling through to the next ranked source for the same title.

## Content identity: `content_key`, not the raw URL

A stream URL is disposable — a provider rotates it, a `tvg_id` changes, a better TMDB match becomes available later. Favorites, history and watched-marks all key off a derived `content_key` instead (`content_key.rs`):

```
tmdb:movie:<id>              — matched to a TMDB movie
tmdb:tv:<id>:S<season>E<ep>  — matched to a TMDB episode
tvg:<tvg_id>                 — no TMDB match, but the playlist gives a stable tvg_id
file:<fingerprint>           — a local file
stream:<sha256_of_url>       — the weakest fallback: nothing better to go on
```

`ContentKey::derive` always prefers the strongest identity available. When a title's identity *improves* later — a TMDB match lands, or a provider rotates a `tvg_id` — `db::rekey_content` moves every affected row to the new key and, for sync, tombstones the old key's record while pushing a fresh one under the new key, so other devices converge too instead of the old key's record lingering as an orphan forever.

## Sync: end-to-end encrypted, no accounts

A sync group is one random 32-byte root, shown to the user as a typo-checked key. Everything else is derived from it with HKDF into four separate keys (group id, authentication, encryption, item naming), each with its own label, so knowing one reveals nothing about another — in particular, the server is shown the *authentication* key to prove the caller belongs to the group, but never the *encryption* key (`sync_crypto.rs`'s own doc comment has the detail).

Every record is sealed with XChaCha20-Poly1305 before it ever leaves the device; the authenticated data binds the ciphertext to its group, item id, timestamp and deleted flag, so a malicious or compromised server can drop records or serve an older valid version, but cannot move a blob to another item or forge its timestamp without the open step failing on every client. See the README's "Sync" section ("What the server can/cannot see") for the exact split.

The server side (`sync-server/`) is a small Cloudflare Worker with a D1 database — opaque blobs in, opaque blobs out, with abuse limits (proof-of-work group creation, a per-network daily group-creation cap, a per-group item cap enforced by an atomic conditional `UPDATE` rather than a read-then-write check) replacing the need for accounts. Pulls page by a server-assigned sequence number, not a client timestamp, so a device that joins late and re-uploads older-timestamped data is never invisible to a device whose cursor has already moved past that timestamp.

Conflicts resolve last-write-wins by timestamp on the client; the server never arbitrates. A profile created independently on two devices before they ever synced has no way to recognize the other as the same person on its own (each gets its own random key at creation) — `db::merge_profiles` is the deliberate, user-driven way to collapse that pair back into one afterward.

## Network guard and network privacy: two different boundaries

These solve different problems and sit at different layers — don't confuse them:

- **`netguard`** (`crates/lumen-core/src/netguard.rs`) stops a hostile *URL* from reaching somewhere it shouldn't: a playlist link, a harvested page, a provider endpoint is text from the internet, and fetching it blindly would let a page aim the app at `http://127.0.0.1:…`, a router's admin page, or a cloud metadata address. It checks three places independently — the URL as typed, every redirect hop, and the address a hostname actually resolves to when the socket opens (closing the DNS-rebinding window where a name answers with a public address first and a private one second). This runs *always*, regardless of whether network privacy is on.
- **Network privacy** (`crates/lumen-core/src/privacy/`) is optional and user-configured: an all-or-nothing local relay that Lumen's own metadata requests, mpv's playback traffic, yt-dlp's extraction requests, and remote image fetches are routed through, which in turn forwards to a SOCKS5/HTTP proxy the user points it at (their own VPN, Tor, Shadowsocks). It's fail-closed throughout — a guarded call errors rather than ever falling back to a direct connection if the relay isn't running or privacy's on/off state hasn't been read from settings yet (`Route::Pending` at startup). mpv gets `--http-proxy`/`--ytdl=no`/`--load-unsafe-playlists=no` so it can't reach the network another way, and both mpv's and yt-dlp's child environments have ambient proxy variables stripped so a system proxy can't override what Lumen explicitly decided.

A request can pass `netguard`'s check and still go through the privacy relay (or not, if a local-network source is being deliberately bypassed like a VPN split-tunnel) — the two checks are independent and both apply where relevant.

## How a PR becomes a release

1. A tag like `v0.1.0` pushed to `main` triggers `.github/workflows/release.yml`.
2. It builds the `.deb` and AppImage, computes `SHA256SUMS`, and signs them: a minisign secret key (held only as a repository secret, generated offline, never committed) signs `SHA256SUMS` into `SHA256SUMS.minisig`.
3. Both the binaries and both checksum files are attached to a published GitHub release. A tag with a pre-release suffix (`v0.2.0-rc.1`) becomes a pre-release, which the updater ignores.
4. A running Lumen checks the latest published release against its own version (`update.rs`). For an AppImage, it downloads the matching asset, fetches `SHA256SUMS` *and* `SHA256SUMS.minisig`, and refuses the whole update unless the signature verifies against the public key baked into the binary (`update_sig.rs`) — the checksum alone would only prove the download wasn't corrupted, not who published it, since the checksums ship from the same place as the files. Packages managed by a system (`.deb`, Flatpak) are never replaced this way; they only learn a newer version exists.
5. `LUMEN_SYNC_URL`, a plain repository *variable* (not a secret), is baked in at compile time via `option_env!` and becomes `sync_keys::DEFAULT_SERVER` — this is what makes an official build default to a server that already exists instead of asking the user to deploy one first.
