# Slint client redesign design

## Intent

Replace the egui frontend with a Slint frontend that follows the supplied 16-screen mockup set (`Kimi_Agent_Webapp响应式构建(1).zip`) exactly, while keeping `palisade-core` as the source of truth for config, keyring, and VPN session management.

## Goals

- New `palisade-gui` binary built with Slint + skia-software renderer.
- Screens and dialogs mirror the mockups: disconnected/authenticating/connecting/connected/error, password dialog, verification dialog, portal editor, settings, log, empty state, portal list.
- Preserve portal CRUD, settings, connect/disconnect, password storage, OTP response, log view, close-to-tray setting, and keyring-only secrets.
- Work in a separate branch from `main`, not the unfinished Wayland daemon branch.

## Non-goals

- No new daemon, no changes to privileged helper or NixOS module.
- No protocol/backend changes in `palisade-core`.
- No pixel-perfect extraction from web CSS; source is the supplied PNG mockups.

## Anti-goals

- Putting passwords/OTP into logs or config.
- Rebuilding the app around immediate-mode layout math.

## Constraints

- Rust workspace already has `palisade-core` and `palisade-gui`; keep core APIs intact.
- Slint must compile .slint via `slint-build`; renderer `skia-software` to keep memory low per Habr article.
- Tray icons in `assets/` remain available.
- Desktop mockup labels are English; app name changes from “GPConnect” to “Palisade” everywhere visible.
- NixOS dev shell has no rustup; use `nix develop` and unset `NIX_LDFLAGS` when path has spaces.

## Approaches

1. **Full Slint frontend, core unchanged (chosen).** Replace egui UI with declarative `.slint` files and a thin Rust bridge to `palisade-core`. Matches article and user request; moderate amount of code.
2. **Hybrid egui+Slint.** Lower short-term cost but keeps two UI stacks and fails “rewrite”.
3. **Webview wrapper around mockup HTML.** Rejected: article explicitly moves away from browser/Electron.

## Decisions

- Add `ui/*.slint` files for all 16 screens/dialogs; `ui/theme.slint` centralizes colors from mockups.
- `palisade-gui` keeps `Session`, `AppConfig`, `get/store/delete_password`; on each Slint callback it updates state and event channel.
- Use Slint `Timer` to poll `ConnEvent::try_recv` every 50–100 ms; no blocking UI.
- Password dialog writes to keyring only when “Remember in keyring” is checked; OTP dialog calls `Session::respond`.
- Provide a `#[cfg(test)]` module for pure helpers: state → status label/color, ANSI stripping, portal validation.
- Nix package wraps `palisade` with same runtime libs; slint uses winit; no daemon changes.

## What is left to the builder

- Create branch `sdd/slint-client` from `main`.
- Add Slint dependency/build script and `.slint` files.
- Rewrite `palisade-gui` app code around Slint callbacks/state.
- Update README and Nix files for Slint-specific deps if needed.
- Verify cargo fmt/clippy/test, nix flake check, and launch app.
