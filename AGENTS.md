# AGENTS.md

Guidance for AI coding agents working in this repository.

## Project overview

Palisade is an **open-source, cross-platform GUI client for Palo Alto
GlobalProtect VPN** — a free alternative to the official GlobalProtect client.
Written in Rust with a native egui (eframe, glow backend) UI: no webview, no
Qt/GTK, single small binary. Windows is the primary target, Linux/macOS
best-effort.

The GlobalProtect protocol itself is **not reimplemented** — it is delegated to
an external program, chosen per portal (`PortalConfig.backend`):

- `openconnect --protocol=gp` (default), or
- `gpclient` (yuezk's GlobalProtect-openconnect), which does the portal login
  over HTTP itself and runs the tunnel through the openconnect library.

Either way Palisade manages the child process (spawn, password/OTP on the PTY,
output parsing, kill-on-drop, reconnect-on-drop), the connection state machine,
config, and the OS keyring.

## Repository layout

- `Cargo.toml` — workspace (members: `palisade-core`, `palisade-gui`).
- `palisade-core/` — library:
  - `src/session.rs` — `Session` spawns a background thread with its own
    current-thread tokio runtime (the GUI is synchronous). Builds the command
    line for the portal's `Backend` and spawns it with `kill_on_drop(true)`:
    - `Backend::Openconnect` — `openconnect --protocol=gp --os=win --user U`
      with optional `--csd-wrapper` (HIP), `--external-browser=xdg-open|open|cmd`
      (SAML) and the portal host last; password goes over stdin/PTY.
    - `Backend::Gpclient` — `gpclient connect <portal> --os=Windows --user U --hip`
      (note: gpclient wants `Windows`, openconnect wants `win`).
      gpclient does the portal login over HTTP and submits HIP itself. It
      requires a TTY for its prompts (`inquire` bails with "The input device is
      not a TTY"), which is why the PTY above is mandatory for this backend;
      `--passwd-on-stdin` does not work.
    On Unix the child runs on a PTY so follow-up prompts can be answered; echo
    is disabled before spawn and the password/OTP are only ever written to the
    PTY, never passed as argv. The pump task parses "Connected as …"/auth-failure
    lines into `ConnState`, emits `ConnEvent::Challenge` for OTP prompts, and
    restarts the child on unexpected exit when `settings.reconnect_on_drop` is
    set.
  - `src/config.rs` — `AppConfig` (portals, last used, settings) as JSON in the
    platform config dir. **Contains no secrets.**
  - `src/lib.rs` — keyring helpers (`keyring` crate, service `palisade`,
    account `portal|username`). Secrets never touch disk.
- `palisade-gui/` — egui binary (`palisade`): portal list editor, password
  prompt window, connect/disconnect, rolling log view, settings.
  `src/app.rs` holds all UI code.
- `flake.nix`, `module.nix`, `shell.nix`, `nix/` — NixOS-first packaging:
  `nix/package.nix` (buildRustPackage + wrapProgram: openconnect on PATH,
  GL/Wayland/X11 runtime libs), `module.nix` (NixOS module:
  `services.palisade.*` options; writes /etc/palisade/config.json, sudo
  wrapper + NOPASSWD sudoers for openconnect, `tun` module, keyring),
  `nix/dev-shell.nix` (verified dev environment), `shell.nix` (nix-shell alias).
  Flakes only see git-tracked files — `git add` new Nix files before building.
- `docs/protocol-notes.md` — REA-backed reverse-engineering notes from the
  official GlobalProtect 5.2.13-48 installers in `win_mac_binaries/` (kept
  out of git; `win_mac_binaries/re/` is the extraction scratch dir).
  Installers and extracted binaries must not be committed (proprietary).

## Build / test

Primary (Nix):

- `nix develop` (or `nix-shell`) — dev environment with the pinned toolchain
  deps; then `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo fmt`, `cargo test`. Keep clippy and fmt clean.
- `nix build` — package; `nix flake check` — package + NixOS module type-check.

Plain cargo on non-Nix systems works too (needs a Rust toolchain and
`dbus-1`/`openssl` dev packages for the keyring crate).

Tests: `cargo test` (unit tests in the crates; no integration tests yet —
real VPN connectivity is verified manually against a portal). On this host,
the rustup binary does NOT run (NixOS); use the dev shell.

## Conventions and cautions

- Keep the GUI dependency-light: no webview, no heavy widget frameworks. egui
  is used with `default-features = false` and only `glow` + `x11`/`wayland`.
- Nothing async in the GUI crate; all async work happens inside
  `palisade-core`'s session thread. UI communicates via `mpsc` channels and
  `try_recv` each frame.
- The session worker must never die from a failed openconnect run — failures
  become `ConnState::Error` events, not panics.
- Passwords are only in the OS keyring. Never add code that writes secrets to
  config files or logs (the log view shows raw child output — do not echo
  stdin into it), and never put a password in argv: it is visible in `ps` and
  `/proc/<pid>/cmdline`.
- On Unix the child runs on a PTY. Its echo must stay disabled (`spawn_openconnect`
  does this before the spawn, on the slave fd): otherwise the password we write
  is echoed back through the master and lands in the log. If you touch the PTY
  setup, keep that ordering.
- `is_auth_failure` pattern matching is heuristic; extend the patterns rather
  than parsing openconnect output rigidly (its wording changes between
  versions).

## Deployment

There is no installer automation yet. Release builds are plain
`cargo build --release`; Windows builds are unsigned (document SmartScreen in
the README when adding an installer).
