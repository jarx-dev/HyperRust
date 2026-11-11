# HyperRust

Push-to-talk dictation for Windows: a global hotkey records, streams to a speech-to-text service, optionally runs an LLM over the text and pastes it into the active window. Rust + Tauri 2, plain TypeScript UI, one portable EXE; Polish UI.

## Commands

- Checks (the same as `.githooks/pre-push` and CI): `pnpm build`, then in `src-tauri`: `cargo test --workspace --locked` and `cargo clippy --workspace --all-targets --locked -- -D warnings`. On this machine cargo is wrapped and needs `XDG_RUNTIME_DIR=/run/user/$(id -u)`.
- Windows code from Linux: `cargo xwin clippy --target x86_64-pc-windows-msvc --workspace --all-targets -- -D warnings`. The hook, microphone, paste and tray code compile only for Windows; Linux checks skip them.
- README pictures: `scripts/screenshots.sh` renders the UI with `scripts/screenshots/mock.js` (demo data) in headless Edge on the Windows laptop over ssh, so nothing shows on its screen. Never start the app itself on the laptop: it has no invisible test mode yet (load the `win-laptop-test` skill before any laptop run).

## Never

- Never commit addresses of private machines or networks, user names or paths, keys, real dictations or names of the owner's clients. This repository is public. Private defaults reach the EXE only through `option_env!` from `~/.config/hyperrust/build.env` (`HYPERRUST_DEFAULT_UPDATE_URL`, `_HISTORY_URL`, `_LLM_URL`; see `docs/wlasny-serwer.md`).
- Never publish a private build without `HYPERRUST_DEFAULT_UPDATE_URL`: its copies would look for updates on GitHub instead of the private server. `scripts/publish-dev.sh` refuses; don't work around it.
- Never put a dictated text into the app log (`hyperrust.log`); it goes only to `history.jsonl`.
- Never store a key outside the Windows Credential Manager (`keyring`, service `HyperRust`).

## Invariants

- Only builds made with `HYPERRUST_BUILD` set (release workflow, `publish-dev.sh`) update themselves (`HYPERRUST_SELF_UPDATE` from `build.rs`), so a local test build is never replaced under test.
- The updater takes plain HTTP only from Tailscale and loopback addresses and follows redirects only HTTPS→HTTPS. Don't loosen either: GitHub's `releases/latest/download` redirects twice, and an HTTP hop would let the manifest and its SHA-256 be swapped together.
- Nothing slow runs on the dictation path (history upload, balances, catalogs wait for a quiet moment). A change that adds time between key release and paste must say how much, measured before and after (`timings.jsonl`).
- `ranking.json` sits next to a private server's `latest.json`; builds updating from GitHub read the public copy (`ranking::GITHUB_URL`, asset of the pre-release `ranking`, uploaded by `scripts/ranking-nuc.sh`). NEVER publish the judge's `issues` there: they quote dictations and part of the set is the owner's private history. Keep `ranking` a pre-release, or `releases/latest` (the updater's manifest URL) would point at it.

## Key files

| File | What |
|---|---|
| `src-tauri/crates/core/src/modes.rs` | built-in prompts; the daily ranking re-tests models when `AGENT_PROMPT` changes |
| `src-tauri/crates/core/src/hotkey.rs` | push-to-talk state machine, unit-tested without Windows |
| `docs/windows-manual-test.md` | the manual Windows checklist for UI and hook changes |
