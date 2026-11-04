# AGENTS.md

Guidance for AI coding agents working in this repository.

## Project overview

GPConnect is an **open-source, cross-platform GUI client for Palo Alto
GlobalProtect VPN** — a free alternative to the official GlobalProtect client.
Written in Rust with a native egui (eframe, glow backend) UI: no webview, no
Qt/GTK, single small binary. Windows is the primary target, Linux/macOS
best-effort.

The GlobalProtect protocol itself is **not reimplemented** — it is delegated to
an external `openconnect` binary (`--protocol=gp`), the same approach gpclient
uses. GPConnect manages the OpenConnect child process (spawn, password/cookie
on stdin, output parsing, kill-on-drop, reconnect-on-drop), the connection
state machine, config, and the OS keyring.

## Repository layout

- `Cargo.toml` — workspace (members: `gpconnect-core`, `gpconnect-gui`).
- `gpconnect-core/` — library:
  - `src/session.rs` — `Session` spawns a background thread with its own
    current-thread tokio runtime (the GUI is synchronous). Spawns
    `openconnect --protocol=gp` with `kill_on_drop(true)`; the pump task feeds
    `--passwd-on-stdin` (password auth) or `--external-browser=xdg-open|open|
    cmd` (SAML auth), parses "Connected as …"/auth-failure lines into
    `ConnState`, and restarts the child on unexpected exit when
    `settings.reconnect_on_drop` is set.
  - `src/config.rs` — `AppConfig` (portals, last used, settings) as JSON in the
    platform config dir. **Contains no secrets.**
  - `src/lib.rs` — keyring helpers (`keyring` crate, service `gpconnect`,
    account `portal|username`). Secrets never touch disk.
- `gpconnect-gui/` — egui binary (`gpconnect`): portal list editor, password
  prompt window, connect/disconnect, rolling log view, settings.
  `src/app.rs` holds all UI code.
- `docs/protocol-notes.md` — REA-backed reverse-engineering notes from the
  official GlobalProtect 5.2.13-48 installers in `win_mac_binaries/` (kept
  out of git; `win_mac_binaries/re/` is the extraction scratch dir).
  Installers and extracted binaries must not be committed (proprietary).

## Build / test

- Build: `cargo build` (no OpenConnect needed to build; needed at runtime).
- Lint/fmt: `cargo clippy -D warnings`, `cargo fmt`. Keep both clean.
- Tests: `cargo test` (unit tests in the crates; no integration tests yet —
  real VPN connectivity is verified manually against a portal).

## Conventions and cautions

- Keep the GUI dependency-light: no webview, no heavy widget frameworks. egui
  is used with `default-features = false` and only `glow` + `x11`/`wayland`.
- Nothing async in the GUI crate; all async work happens inside
  `gpconnect-core`'s session thread. UI communicates via `mpsc` channels and
  `try_recv` each frame.
- The session worker must never die from a failed openconnect run — failures
  become `ConnState::Error` events, not panics.
- Passwords are only in the OS keyring. Never add code that writes secrets to
  config files or logs (the log view shows raw openconnect output — do not
  echo stdin into it).
- `is_auth_failure` pattern matching is heuristic; extend the patterns rather
  than parsing openconnect output rigidly (its wording changes between
  versions).

## Deployment

There is no installer automation yet. Release builds are plain
`cargo build --release`; Windows builds are unsigned (document SmartScreen in
the README when adding an installer).
