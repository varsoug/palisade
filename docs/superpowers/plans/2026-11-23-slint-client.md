# Slint Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the egui frontend with a Slint frontend matching the supplied 16 mockups while preserving `palisade-core` session/config/keyring behavior.

**Architecture:** Keep `palisade-core` unchanged. Add `.slint` UI files and a thin Rust bridge in `palisade-gui`; use Slint's `skia-software` renderer and a timer-driven event pump over `Session`'s existing `try_recv` channel.

**Tech Stack:** Rust, Slint 1.x, `slint-build`, `skia-software`/winit backend, existing `palisade-core`, Nix dev shell.

**Spec:** `docs/superpowers/specs/2026-11-23-slint-client-design.md`

## Global Constraints

- Work only in branch `sdd/slint-client` from `main`; do not merge Wayland daemon branch.
- App-visible name must be `Palisade` (not GPConnect).
- Match supplied mockups: states, dialogs, tabs, settings, log, empty state, portal list.
- Secrets: passwords only in OS keyring; never in `.slint`, config, argv, logs, or tests fixtures.
- Preserve existing `palisade-core` APIs; no session/protocol changes.
- Use Slint `skia-software` renderer with winit backend.
- Keep `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `nix flake check` green.
- In this environment cargo inside `nix develop` may need `unset NIX_LDFLAGS` or a space-free `CARGO_TARGET_DIR`.

## Review Focus

- Empty portal list must show Add portal CTA, not blank window. Task 2 test: state mapping for empty list.
- Password dialog must not print/store password unless Remember checked. Task 3 test: helper `password_store_decision`.
- OTP challenge visible while connected/auth states. Task 3 test: state mapping for Challenge.
- Portal editor must reject empty host/username. Task 3 test: portal validation helper.
- ANSI/log lines must be stripped before display. Task 3 test: existing/new strip_ansi test.

---

### Task 1: Slint scaffold and visual shell

**Files:**
- Modify: `palisade-gui/Cargo.toml` — replace egui/eframe deps with `slint` + `slint-build`; keep core deps.
- Create: `palisade-gui/build.rs`
- Create: `ui/theme.slint`, `ui/app.slint`, `ui/dialogs.slint`, `ui/theme.slint`
- Create: `palisade-gui/src/main.rs` minimal window bootstrap.
- Test: `cargo check -p palisade-gui`

**Interfaces:**
- Consumes: mockup PNGs in `docs/design-mockups/`.
- Produces: `AppWindow` component with properties/callbacks used by Task 2.

- [ ] **Step 1: Add Slint deps and build script**

Set Slint features to `["std", "compat-1-2", "backend-winit", "renderer-skia"]`; build.rs uses `slint_build::compile("ui/app.slint")`.

- [ ] **Step 2: Create declarative UI**

Implement `AppWindow` with: title `Palisade`, portal combo, big Connect/Disconnect button, status card (dot/title/subline), tab bar Portals/Log/Settings, main content area, and dialogs (password, verification, portal editor). Use theme tokens from mockups.

- [ ] **Step 3: Minimal bootstrap**

`main.rs` selects `skia-software` backend and shows `AppWindow`; no core wiring yet.

- [ ] **Step 4: Verify**

Run `nix develop --no-write-lock-file -c sh -c 'unset NIX_LDFLAGS; cargo check -p palisade-gui'`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add palisade-gui ui
git commit -m "feat(slint): add declarative UI scaffold"
```

---

### Task 2: Core state bridge and event pump

**Files:**
- Modify: `palisade-gui/src/main.rs`
- Create: `palisade-gui/src/bridge.rs`
- Test: `palisade-gui/src/bridge.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `palisade_core::{AppConfig, ConnEvent, ConnState, Session}` from core and `AppWindow` from Task 1.
- Produces: `fn status_for(state: &ConnState) -> (String, String, u32)` returning title/subline/dot RGB; `fn strip_ansi(line:&str)->String`; `fn portal_valid(name,portal,username)->bool`.

- [ ] **Step 1: Write failing helper tests**

Test exact strings: `Not Connected`, `Authenticating…`, `Connecting…`, `Connected`, `Error`; portal validation rejects empty host/username; ANSI stripping removes CSI/OSC.

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p palisade-gui bridge`
Expected: FAIL.

- [ ] **Step 3: Implement bridge helpers**

Use core `ConnState` variants and RGB colors from mockups (primary blue `0x2F6FDB`; status green/orange/red/gray).

- [ ] **Step 4: Wire session events**

Instantiate `Session`, keep `AppConfig`, `Timer` polls `events.try_recv()` every 50 ms and updates labels/log. `connect/disconnect` callbacks call `Session`, reading password from keyring.

- [ ] **Step 5: Verify**

Run: `cargo test -p palisade-gui bridge && cargo clippy -p palisade-gui --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add palisade-gui/src
git commit -m "feat(slint): bridge session state into UI"
```

---

### Task 3: Dialogs/portal/settings behaviors and Nix packaging

**Files:**
- Modify: `ui/dialogs.slint`, `ui/app.slint`, `palisade-gui/src/bridge.rs`, `nix/package.nix`, `README.md`
- Test: `palisade-gui/src/bridge.rs`

**Interfaces:**
- Consumes: Task 2 helpers and Slint component callbacks.
- Produces: callbacks for portal CRUD, save settings, submit/close dialogs.

- [ ] **Step 1: Write failing tests**

`password_store_decision(remember:bool)->bool`; `config_save_roundtrip(AppConfig)->bool`; portal CRUD keeps no secrets.

- [ ] **Step 2: Implement dialogs and CRUD**

Password dialog: Enter/Cancel/Connect; OTP dialog: OK/Cancel; portal editor: save/cancel with validation; settings: theme/reconnect/launch/connect-at-login/tray/bin paths matching mockups; log view copyable.

- [ ] **Step 3: Package**

Ensure `nix/package.nix` still wraps `palisade`; add any Slint runtime libs to `LD_LIBRARY_PATH` if build/launch requires them.

- [ ] **Step 4: Verify**

Run: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `nix flake check`.
Expected: PASS. Launch with `DISPLAY=:0 nix develop -c ... ./target/debug/palisade` if GUI environment allows.

- [ ] **Step 5: Commit**

```bash
git add ui palisade-gui nix README.md
git commit -m "feat(slint): complete UI behaviors and packaging"
```

## Final Verification

- Branch `sdd/slint-client` builds, tests, clippy/fmt, `nix flake check`.
- App title is `Palisade`.
- All 16 mockup states/dialogs represented.
- No passwords in config, UI source constants, logs, or argv.
