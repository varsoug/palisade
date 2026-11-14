//! Start one privileged helper for the lifetime of this GUI process.
//!
//! On non-NixOS Linux the GUI runs unprivileged while VPN backends need root.
//! `pkexec` starts `palisade-helper` once at application startup; every
//! connect, disconnect and reconnect then talks to that already-root process
//! through a user-private Unix socket. No further password dialogs appear.

use std::process::{Command, Stdio};

const HELPER_BIN: &str = "/usr/local/libexec/palisade-helper";

fn socket_path() -> String {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    format!("{base}/palisade-helper.sock")
}

/// Start the privileged helper once, before the first VPN command is spawned.
/// The helper watches this GUI's PID and exits shortly after the GUI closes,
/// giving session cleanup time to disconnect the tunnel first.
pub fn start_helper() -> Option<HelperGuard> {
    if !std::path::Path::new(HELPER_BIN).exists() {
        return None;
    }
    let socket_path = socket_path();
    let _child = Command::new("pkexec")
        .arg(HELPER_BIN)
        .arg(&socket_path)
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| tracing::info!("could not start privileged helper: {e}"))
        .ok()?;
    for _ in 0..40 {
        if std::path::Path::new(&socket_path).exists() {
            return Some(HelperGuard);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    tracing::info!("privileged helper did not create its socket");
    Some(HelperGuard)
}

pub struct HelperGuard;
