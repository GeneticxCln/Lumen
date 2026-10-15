# Lumen audit and frontend redesign

## Scope and acceptance

Reviewed React views/hooks/API bridge, Rust commands/SQLite/M3U parsing/mpv IPC/sidecar lifecycle, Python extraction/harvesting/probing/browser bridge, Tauri capabilities/configuration, and build/packaging scripts.

The requested frontend overhaul is implemented in the existing React + TypeScript + Vite app. It follows Netflix's visual browsing pattern while retaining Lumen branding and original local artwork, not Netflix's logo, catalog, or proprietary assets. This is not a pixel-for-pixel Netflix replica.

- Cinematic hero, dark navigation, media shelves, title details, responsive layouts.
- Real-library search across imported sources, category/source filters, saved favorites and history-based discovery.
- Virtualized horizontal shelves using the project's existing TanStack dependency.
- Device-local presentation/volume/keyboard preferences and SQLite-backed engine settings.
- Source import/removal, local file browsing, keyboard-accessible dialogs.
- Desktop remains native mpv; the concurrently added browser player is preserved and lazy-loaded. Browser codec/CORS/header restrictions still apply.

The shared checkout was edited concurrently during this task. The final navigation structure is Discovery, Movies, Series, Live TV and Settings: Discovery is the local media on this device, search lives in the top bar, and saved titles/local files are managed in Settings. No Git repository was available, so no commit or diff-based ownership review was possible.

## Resolved findings

1. **Critical: browser bridge exposed sensitive local commands.** The filesystem/database browser bridge is now registered only in explicit standalone development mode. Native sidecars retain bearer-protected extraction APIs and no longer import/initialize the Python bridge database. Browser development requests require loopback Host, an exact configured loopback Origin when supplied, and a non-simple client header. Wildcard CORS was removed. Media-element requests use a fixed client marker instead of a custom header (not a secret/auth credential); foreign Origins are still rejected. This is local-development defense, not authentication suitable for hosting.
2. **High: stale library data after import and out-of-order filter loads.** Full-library refresh is cancellation-safe; source reads are batched four at a time; search/filtering is local and works across playlists. New imports reload the actual rows, not only playlist names.
3. **High: failures were presented as success.** Changed playback/import/settings paths expose errors. File picker failures are caught. Playback commands do not blindly swallow failures. Stream-check timestamps are not portrayed as guaranteed current availability. Seeded streams are no longer automatically marked verified.
4. **Medium: database connections leaked in Python.** The bridge connection context manager now commits/rolls back and always closes connections; tests run with ResourceWarning treated as an error.
5. **Medium: mute volume 0 became 80.** The bridge now distinguishes zero from a missing volume.
6. **Medium: progress save interval reset with each time update.** Progress is read through a ref on a stable interval.
7. **Medium: inaccessible overlay dialogs and local list items.** Native dialogs provide modal focus behavior and Escape; local entries are buttons; controls have accessible names; focus outlines, reduced motion, and mobile layouts are supported.
8. **Medium: unbounded HLS recovery.** Browser-player fatal-error recovery is capped and produces a visible failure. The player remains mounted when its controls dialog closes so browsing does not immediately destroy playback.
9. **Low: template identity and unsuitable desktop size.** Branded document title/favicon and a 1440×900 default desktop window replace the starter template/800×600 dashboard.

## Remaining risks / next work

### High priority

- **Release packaging is not proven.** The sidecar build script's PyInstaller path runs a file with package-relative imports; the native-development wrapper is not a portable release bundle. Tauri's build/bundle configuration does not yet stage a verified sidecar artifact. Test .deb/AppImage on clean machines before claiming readiness.
- **Content Security Policy remains disabled** in Tauri configuration. Define a policy compatible with runtime media/logos and test in WebKitGTK; do not blindly apply a restrictive policy that breaks playback.
- **Network fetch hardening remains incomplete.** Extract/harvest/import accept user-directed destinations but do not consistently enforce private-network restrictions, response-size ceilings, or aggregate time limits. Local application use may intentionally need LAN sources; establish that policy before deploying any bridge. Never publish the development bridge.
- **Playback engine integration is not fully verified on Linux desktop.** Rust unit tests pass, but this task did not complete end-to-end native mpv playback on both Wayland and X11. Resume currently seeks immediately after load rather than explicitly waiting for file-loaded.

### Medium priority

- M3U parsing does not distinguish an HLS manifest from a channel playlist or resolve all relative stream URLs. A raw HLS manifest can be mis-imported as segment entries.
- History writes append rows without retention/compaction; imports create playlist/channel records across separate operations in Rust rather than one encompassing transaction.
- mpv IPC has unbounded command/pending structures; timeout cleanup and child-sidecar stdout/stderr draining should be reviewed for long sessions. Sidecar ready announcement precedes fully verified HTTP readiness.
- Python bridge duplicates Rust storage/import logic. Both share schema assumptions and can drift; keep parity tests or retire the bridge outside development.
- Arbitrary extra mpv arguments are an advanced trusted-local feature; whitespace splitting does not implement shell quoting. No shell is invoked for these args.
- Browser playback cannot forward arbitrary stream headers and may fail for CORS, codecs, mixed content, expired URLs, or split audio/video formats from extraction. Native mpv remains the dependable path for those formats. Browser resume/completion behavior needs further work.
- Movies/series/live classification is a keyword heuristic over names/groups, not authoritative catalog metadata or a live schedule. Discovery is deterministic local ranking, not Netflix-scale collaborative filtering.
- All library metadata is loaded in memory. Shelves virtualize DOM elements, but very large category counts and startup payloads need pagination/row virtualization.
- HLS player chunk is about 577 kB minified (180 kB gzip). It is lazy-loaded, but Vite still reports its large-chunk warning. No warning threshold was weakened.
- Python dependencies use loose lower bounds (including yt-dlp), despite the plan's pinning requirement. Lock and validate extraction updates before release.

### Low priority

- Locally generated video-frame thumbnails are now available for on-device files through FFmpeg and are cached by source file identity. A frame is a cinematic preview rather than official poster artwork; audio, playlists, and unsupported formats keep their branded fallbacks.
- Older dashboard components and a settings-dialog variant remain in the shared checkout. Remove obsolete code after the concurrent frontend changes settle.

## Verification

- `npm test`: deterministic filtering, ranking, preference validation, history ordering/deduplication, and a 10,000-entry correctness/timing check. Representative run: ~5–11 ms for filtering + ranking on this machine. This is not a claimed speedup or full-UI benchmark.
- `npm run typecheck` and `npm run build`: passing. The lazy HLS chunk warning remains explicitly noted.
- `cargo test --lib`: 3 passing tests covering M3U parsing, SQLite/favorites/settings, and actual sidecar lifecycle/probe.
- Python unittest suite: passing; covers Origin/Host/header rejection, native endpoint isolation and bearer auth, settings round trip, import/favorite/cascade removal, zero volume, and concurrently added stale-channel progress regression.
- Live browser checks at 1440×1000 and 390×844: screenshots inspected, no horizontal page overflow; search category matching and SQLite-backed save-to-list verified; preference persistence and dialog Escape verified.
- Imported an isolated test playlist through the real Add source UI and confirmed it immediately appeared. A generated local WebM played through the browser interface: 640px decoded video, readyState 4, 12-second duration, advancing playback time. This does not verify every external HLS catalog or native mpv playback.
- Preview uses an isolated `/tmp/lumen-preview-data` database rather than the user's media database. The project supports `LUMEN_DATA_DIR`, `LUMEN_BRIDGE_URL`, and `LUMEN_PREVIEW_PORT` for independent local previews.

This is a source-level review plus targeted tests, not a penetration test, release certification, or exhaustive verification of every stream/provider.
