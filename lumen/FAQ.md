# FAQ

Things people actually hit, as questions. For what's been verified and what hasn't, see [AUDIT.md](AUDIT.md); this is the user-facing version of the same ground.

## Playback

### mpv won't start, or says "stopped before it was ready" / "stopped responding"

The first two happen when mpv can't start at all; the third when it was running and then exited (a bad stream, a crash) partway through a session. All three now show mpv's own last lines. Run `LUMEN_DEBUG_MPV=1 lumen` (or `LUMEN_DEBUG_MPV=1 ./lumen_*.AppImage`) to see everything mpv says, and confirm `mpv --version` works on its own. Programs Lumen starts don't inherit the AppImage's own bundled library paths.

### Scrolling is choppy

Lumen asks the page engine for smooth, hardware-accelerated scrolling. If it's still slow, try `LUMEN_WEBKIT_SAFE=1 lumen` — this turns off WebKitGTK's DMA-BUF renderer, which some NVIDIA and Wayland setups handle badly. The AppImage runs through XWayland on a Wayland session by default; `GDK_BACKEND=wayland ./lumen_*.AppImage` uses native Wayland instead.

### Stop or Pause didn't respond right away

This was a real bug (fixed in 0.1.4): starting mpv for a new stream used to hold the same lock Stop and Pause wait on, so pressing either while a slow or unreachable stream was still starting could block for up to the full startup timeout. Starting a stream no longer holds that lock.

### A dropped stream auto-reconnects — how far does that go?

Two independent layers. mpv's own libavformat reconnect flags try first (a no-op for local files). If the app still sees the stream fail, it retries the exact same URL a few times with backoff (1s/3s/7s) before falling through to the next ranked source for the same title. There's no attempt to simulate an actual mid-stream connection drop in the automated test suite — this is covered by unit tests of the retry logic and the reconnect flags separately, not an end-to-end "pull the cable" test.

### Is single-window playback (video inside the Lumen window) reliable?

It's marked experimental for a reason. On X11/XWayland it's been checked by screenshot under a virtual display: one window, video above the controls, and turning the setting off correctly falls back to the separate mpv window. On native Wayland, the video renders through `libmpv` into a GTK GL area — checked under a headless Weston compositor with software rendering, where the frame did show real video content. Not checked: real GNOME, KDE or Hyprland sessions, hardware decoding on this specific path (the Wayland display isn't passed to `libmpv`, so it may fall back to software decoding), and fullscreen on a real window manager. If you hit a black frame, `LUMEN_GL_DEBUG=1` prints how much of the picture is actually lit, which helps tell "nothing is rendering" from "it's rendering but to the wrong place."

## Casting

### Does casting work with my TV?

It's been tested against an in-process fake UPnP/DLNA renderer, covering discovery, the control calls, and serving a local file (byte ranges, one allowed peer, media files only) — not against a real TV, receiver or speaker. Real devices vary in what formats, byte-range behavior and metadata they accept, so results may differ. Chromecast and AirPlay are different protocols entirely and aren't supported. Subtitles aren't sent to the device, and Lumen doesn't control the device's own volume.

## Updates

### How do I know an update is really from this project, not someone who compromised the release pipeline?

Since the signed-updates change: every update check fetches both `SHA256SUMS` and a `SHA256SUMS.minisig` signature, and refuses the whole update unless that signature verifies against a public key baked into the binary — only whoever holds the matching secret key (generated offline, never committed to the repository, held only as a GitHub Actions secret) can produce a valid one. Before that change, the checksum alone only proved a download wasn't corrupted, not who published it, since the checksums shipped from the same place as the files.

There's one unavoidable bootstrap gap: a Lumen build from before the signing change has no baked-in public key yet, so it keeps accepting checksum-only updates (signed or not) until it updates past that point. This is called out in that release's own notes rather than patched around with a soft-fail compromise mode.

### Has a real update actually been tested end to end?

The release lookup, version comparison, checksum and signature verification, and the file-swap logic are all tested against an in-process fake server and temporary files. What hasn't been directly observed: a published AppImage actually being downloaded and swapped in by a running Lumen, including the restart handoff where a freshly-installed copy starts after the old one exits. If you hit something odd during an update, that's the likeliest unverified edge.

## Sync

### What happens if I lose my sync key?

The synced copy becomes permanently unreachable — there's no account and no recovery mechanism by design. Your library on each device is unaffected either way; only the cross-device sharing stops working. **Show key** in Settings re-displays the current key at any time (type it only into your own devices — anyone who has it can read and change the synced data).

### I joined a sync group and now I have two profiles that should be the same person

Every profile gets its own random identity key the moment it's created, so two devices that each made their own default profile before ever syncing have no way to recognize each other as the same person automatically. Right after joining, if this situation is detected, Lumen offers a one-time merge prompt; "not the same person" is also a valid answer if a group genuinely holds distinct people.

### Does the sync server see my watch history or favorites?

No — everything is encrypted on your device before it's uploaded (XChaCha20-Poly1305), with the encryption key itself never leaving your devices. See the README's "What the server can and cannot see" table for the exact breakdown of what the server does learn (an opaque record count, sizes, timestamps, your network address) versus what it never does (stream URLs, titles, profile names, positions, or even what *kind* of record something is).

### Do I have to trust the server you run?

No — self-hosting your own is one setting away (Settings → Profiles & sync → Sync server), and it's the same open-source Cloudflare Worker either way, so it learns exactly as little as the default one. See the README's "Running your own sync server" section.

## Network privacy

### Does turning on network privacy slow things down?

Possibly, depending on your proxy — but only once it's on. Before you turn it on, nothing has changed: images and metadata load exactly as they did before this feature existed.

### What's *not* covered by network privacy?

Device casting (it only ever talks to devices on your local network anyway) and the **Open on GitHub** link in Settings → About, which opens in your system's own browser, not inside Lumen.

### Has this actually been checked against a real proxy?

The relay, and the mpv/yt-dlp wiring into it, are covered by unit tests and fake-upstream tests, and the app builds and runs with the feature compiled in — but nobody has pointed it at a real SOCKS5/HTTP proxy (a real VPN client, Tor, Shadowsocks) and watched the actual network traffic with a packet capture to directly confirm nothing leaks. If this matters to your threat model, verify it yourself before relying on it for anything sensitive.

## Interface language

### Why is everything in English even though there's a translation system?

Because only an English catalog has been written so far — the plumbing (message catalog, pluralization, a language selector that appears once a second language exists) is built and enforced by a test that fails if a screen gains hard-coded text, but nothing has exercised it against a *real* second language. Plural rules beyond "one/other", word order, and text length in a language that reads noticeably longer or shorter than English are all unverified. See "Translating Lumen" in the README if you want to add one.

## Keychain / secrets

### Lumen says it can't save the sync key or an API key

The OS keychain Lumen uses is the Secret Service on Linux. A minimal window-manager session (no full desktop environment) may not have anything providing that service running — install and start GNOME Keyring or KWallet.
