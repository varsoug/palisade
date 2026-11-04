# GPConnect

An open-source, cross-platform GUI client for **Palo Alto GlobalProtect VPN** —
a free alternative to the official GlobalProtect client. Written in Rust with a
native [egui](https://github.com/emilk/egui) interface: no webview, no Qt/GTK
runtime, single small binary.

The actual VPN protocol is handled by [OpenConnect](https://www.infradead.org/openconnect/)
(`openconnect --protocol=gp`), which speaks GlobalProtect including SAML SSO.
GPConnect manages the connection lifecycle, credentials, and UI.

## Features

- Portal list with username/password or SAML (browser) authentication
- Passwords stored in the OS keyring (Windows Credential Manager / Secret
  Service / macOS Keychain) — never on disk
- Connection state machine with live log view
- Reconnect on unexpected drop
- Small native binary (egui + glow, no webview)

## Building

Requirements: a Rust toolchain (`rustup` or your distro's cargo) and
**OpenConnect ≥ 9.x** installed and on `PATH` at runtime (not needed to build).

```sh
cargo build --release
```

The binary is `target/release/gpconnect`.

### Windows

Install [Rust via rustup](https://rustup.rs/) (x86_64-pc-windows-msvc), then:

```sh
cargo build --release --target x86_64-pc-windows-msvc
```

OpenConnect on Windows needs a TUN/TAP driver; see the OpenConnect
documentation. Builds are unsigned for now — expect SmartScreen warnings.

## Usage

1. Start the app, click **Add portal…**, enter the portal hostname (e.g.
   `vpn.example.com`) and your username.
2. Choose **Password** or **SAML (browser)** auth.
3. Click **Connect**. For password auth you can choose to remember the password
   in the system keyring.

Config lives in the platform config dir (`~/.config/gpconnect/config.json` on
Linux, `%APPDATA%\GPConnect\gpconnect\` on Windows). It contains no secrets.

## Repository layout

- `gpconnect-core` — library: OpenConnect child-process management, connection
  state machine, config store, keyring access.
- `gpconnect-gui` — the egui application (`gpconnect` binary).
- `docs/protocol-notes.md` — REA-backed reverse-engineering notes from the
  official GlobalProtect 5.2.13-48 installers (`win_mac_binaries/`), kept as
  the protocol/error-mapping reference.

## License

GPL-3.0-or-later.
