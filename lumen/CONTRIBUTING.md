# Contributing to Lumen

Issues and pull requests are welcome.

## One logical unit per pull request

Each pull request should do one thing: one bug fix, one feature, one refactor. Keep it reviewable — if a change grew to touch several unrelated areas, it's usually a sign it should be split. A PR's description should say what changed and why, and what was verified (see "What to verify" below) and what could not be.

## Before opening a pull request

Run the checks for whatever you touched. CI runs the same ones, so running them locally first saves a review round trip.

**Frontend** (`src/`, `src-tauri/`'s TS-facing bits):

```bash
cd lumen
npm run typecheck
npm run lint
npm test
npm run build
```

`npm test` includes an i18n gate: a component that gains hard-coded text (JSX text, or the `aria-label`/`title`/`placeholder`/`alt`/`label` attributes) fails it. New interface text goes in the message catalog (`src/i18n/messages/`) and is read with `t("key")`/`tn("key", count)`, not written inline. `npm run i18n:hits -- <file>` lists what a file still hard-codes.

**Rust** (`crates/lumen-core`, `src-tauri`):

```bash
cd lumen
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p lumen-core
(cd src-tauri && cargo test --lib)
```

`cargo test --lib` needs the Tauri build dependencies, plus `ffmpeg` for the poster tests and `mpv` for the playback tests (those skip with a message when `mpv` is missing, rather than failing).

**Sync server** (`sync-server/`), only if you touched it:

```bash
cd lumen/sync-server
npm run typecheck
npm run dev          # starts it locally with small limits
# in another terminal:
npm run test:sync    # plain-HTTP checks: group creation and proof of work,
                      # authentication, isolation between groups, last-write-wins,
                      # paging, quotas and deletion
```

The Rust side of sync is also tested against an in-process server that speaks the same API (`cargo test -p lumen-core sync`), so a protocol change needs both: the in-process mock kept in sync with any wire-format change, and (if the change reaches the real Worker) a live check against `wrangler dev`.

**E2E smoke tests** (`lumen/e2e/`), only if you touched a flow one of the four tests covers (import a playlist, play a channel, favorite + relaunch, a Settings value round-trip) — otherwise CI's own gated `e2e` job already covers you:

```bash
# System packages (Debian/Ubuntu names; adjust for your distro)
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev \
  mpv ffmpeg webkit2gtk-driver xvfb

cargo install tauri-driver --locked

cd lumen
npm ci && npm run build
cargo build -p lumen

cd e2e
npm ci
npm run test:e2e
```

The harness starts its own throwaway Xvfb display, Vite dev server and `tauri-driver` instance and tears them down itself — no `xvfb-run` wrapper needed. It drives the real built debug binary, not a mock.

## What to verify

A check command passing proves the code compiles and existing behavior didn't regress. It doesn't prove the new behavior works. For anything you can run — a UI change, a new command, a new background task — actually exercise it (in the running app, or against a real local server for sync-server changes) and say so in the PR description, not just "tests pass." If you can't run something (no GPU, no real IPTV source, a service you don't have a key for), say that too, plainly, rather than asserting it works.

## Conventions this codebase already follows

- **One content identity, `content_key`**, not the raw stream URL: favorites, history and watched-marks key off it so they survive a provider rotating a URL or a `tvg_id`. See `ARCHITECTURE.md`.
- **`lumen-core` stays UI-agnostic.** It doesn't know about Tauri; `src-tauri/src/commands.rs` is a thin `#[tauri::command]` wrapper layer over it. Keep new logic in `lumen-core` when it isn't actually about the desktop shell, so it stays unit-testable without a window.
- **Everything the user types that reaches a URL goes through `netguard`** (`crates/lumen-core/src/netguard.rs`): public addresses only unless "allow local network sources" is on, every redirect hop checked again. Don't add a new `reqwest` call that bypasses it.
- **Sync records never carry anything the server could read.** If you add a new synced field, make sure it's inside the sealed payload, not a bare column the Worker stores in the clear.
- **New schema changes are guarded, additive migrations**, not edits to an already-shipped migration file (see `crates/lumen-core/migrations/` for the pattern, or `migrate_tmdb_columns`/`row_count` in `sync-server/src/worker.ts` for the "add the column only if it's missing, backfill once" idiom used outside the migration system proper).

## License

By contributing, you agree your contribution is licensed under the project's [MIT License](LICENSE).
