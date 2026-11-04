//! Login autostart: creates or removes an XDG autostart entry.
//!
//! Written as a plain `.desktop` file in `$XDG_CONFIG_HOME/autostart`, which
//! GNOME, KDE and most other desktops honour. Unlike a systemd user unit this
//! needs no service manager API and no extra dependency.

use std::path::PathBuf;

/// Apply the requested autostart state. Returns an error string suitable for
/// showing in the UI; a no-op when the state already matches.
pub fn apply(enabled: bool) -> Result<(), String> {
    let Some(path) = entry_path() else {
        return Err("no config directory for the autostart entry".into());
    };

    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not remove autostart entry: {e}")),
        };
    }

    let exe = std::env::current_exe()
        .map_err(|e| format!("could not resolve the executable path: {e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Palisade\n\
         Comment=Open-source GlobalProtect VPN client\n\
         Exec={}\n\
         Icon=network-vpn\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    std::fs::write(&path, contents).map_err(|e| format!("could not write autostart entry: {e}"))
}

/// Whether an autostart entry currently exists.
pub fn is_enabled() -> bool {
    entry_path().map(|p| p.exists()).unwrap_or(false)
}

fn entry_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join("palisade.desktop"))
}
