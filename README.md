# GPConnect

GlobalProtect is Palo Alto's corporate VPN. The official client is closed,
heavy and Windows-only. **GPConnect is an open-source alternative**: a small
native GUI client that speaks the GlobalProtect protocol through
[OpenConnect](https://www.infradead.org/openconnect/) — no webview, no Qt/GTK
runtime, single binary. NixOS is a first-class target: the repo is a flake with
a package, a NixOS module, and a dev shell.

## ✨ Features

| | |
| --- | --- |
| 🪶 **Lightweight** | Pure-Rust [egui](https://github.com/emilk/egui) interface (glow backend). One native binary, no webview, no widget-runtime. |
| 🔓 **Open source** | GPL-3.0-or-later. The protocol is handled by OpenConnect, the free GlobalProtect implementation. |
| 🔒 **No secrets on disk** | Passwords live in the OS keyring (Windows Credential Manager / Secret Service / macOS Keychain). Config files contain no secrets. |
| 🔑 **Two auth modes** | Username/password and SAML SSO via your own browser (OpenConnect `--external-browser`). |
| 🧅 **Real VPN** | Portal discovery, gateway connect, ESP tunnel, reconnect on unexpected drop, live log view. |
| 🐧 **NixOS-first** | `nix build`, a `services.gpconnect` NixOS module (declarative portals, sudo wrapper, keyring), and a ready dev shell. |
| 🪟 **Windows target** | Builds for Windows; OpenConnect handles the tunnel (unsigned builds for now). |
| 📋 **Error mapping** | Failure messages mapped to the official client's taxonomy (see `docs/protocol-notes.md`, REA-backed RE notes). |

## 🚀 Usage

1. Start the app, click **Add portal…**, enter the portal hostname (e.g.
   `vpn.example.com`) and your username.
2. Choose **Password** or **SAML (browser)** auth.
3. Click **Connect**. For password auth you can choose to remember the password
   in the system keyring.

Config lives in the platform config dir (`~/.config/gpconnect/config.json` on
Linux, `%APPDATA%\GPConnect\gpconnect\` on Windows). When there is no user
config, the app falls back to `/etc/gpconnect/config.json` — that is how the
NixOS module provisions portals declaratively.

## 🐧 NixOS

This repo is a flake. Install the app on NixOS with the module:

```nix
{
  inputs.gpconnect.url = "github:varsoug/GPConnect";

  outputs = { nixpkgs, gpconnect, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        gpconnect.nixosModules.default
        {
          services.gpconnect = {
            enable = true;
            allowedUsers = [ "alice" ];  # sudo NOPASSWD for openconnect
            portals = [
              { name = "Work"; portal = "vpn.example.com"; username = "alice"; auth = "password"; }
            ];
          };
        }
      ];
    };
  };
}
```

The module installs the package, loads the `tun` kernel module, enables
GNOME keyring (Secret Service for stored passwords), writes the portal config
to `/etc/gpconnect/config.json`, and — when `allowedUsers` is non-empty —
installs a `gpconnect-openconnect` sudo wrapper plus a matching NOPASSWD
sudoers rule, so the unprivileged GUI can spawn openconnect as root.

## 🔍 How it works

GPConnect does not reimplement the GlobalProtect protocol. It spawns
`openconnect --protocol=gp` as a managed child process (password on stdin for
password auth, `--external-browser` for SAML), parses its output into a
connection state machine, and kills/restarts it as needed. Tunnel I/O, TLS,
ESP framing and HIP stay in battle-tested OpenConnect code. The official
client's wire vocabulary — `prelogin.esp` → `getconfig.esp` →
`ssl-tunnel-connect.sslvpn` → `hipreport.esp` — is exactly what OpenConnect
speaks (confirmed by REA-backed analysis of the official 5.2.13-48 binaries,
see `docs/protocol-notes.md`).

## 🔁 Differences from the official GlobalProtect client

| | Official client | GPConnect |
| --- | --- | --- |
| Source available | ❌ | ✅ GPL-3.0-or-later |
| SAML via your own browser | ❌ (embedded IE WebBrowser) | ✅ |
| Secrets on disk | registry/config files | ❌ keyring only |
| Declarative NixOS setup | ❌ | ✅ module |
| Telemetry | ✅ | ❌ |
| Windows code signing | ✅ | ❌ (unsigned builds) |
| Vendor support | ✅ | ❌ community |

## 🔧 Building from source

Requires a Rust toolchain. On NixOS use the dev shell (rustup does not run there):

```
nix develop        # or: nix-shell
cargo build --release
```

On other systems: install Rust via [rustup](https://rustup.rs/), then
`cargo build --release`. OpenConnect is not needed to build, only at runtime.

Other checks:

```
cargo clippy --all-targets -- -D warnings
cargo test
nix build && nix flake check     # package + NixOS module type-check
```

### Windows

Install [Rust via rustup](https://rustup.rs/) (x86_64-pc-windows-msvc), then:

```
cargo build --release --target x86_64-pc-windows-msvc
```

OpenConnect on Windows needs a TUN/TAP driver; see the OpenConnect
documentation. Builds are unsigned — expect SmartScreen warnings.

## 🤝 Contributing

Bug reports and pull requests are welcome. Please open an issue first for
larger changes. Keep `cargo clippy -- -D warnings` and `nix flake check` green.

## 📄 License

GPL-3.0-or-later. Not affiliated with or endorsed by Palo Alto Networks.
GlobalProtect is a trademark of Palo Alto Networks.
