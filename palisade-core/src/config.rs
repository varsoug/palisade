use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Which program drives the tunnel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    /// Plain `openconnect --protocol=gp` CLI (default).
    #[default]
    Openconnect,
    /// `gpclient` (yuezk's GlobalProtect-openconnect). It does the portal
    /// login itself and speaks the MFA challenge forms that make some
    /// portals (e.g. `vpn.etsp.ru`) stall plain openconnect after the code
    /// is sent.
    Gpclient,
}

/// How a portal authenticates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMode {
    /// Username + password (password comes from the OS keyring or a prompt).
    Password,
    /// SAML SSO via an external browser (OpenConnect `--external-browser`).
    Saml,
}

/// One saved portal entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalConfig {
    /// Display name shown in the UI (defaults to the portal host).
    pub name: String,
    /// Portal hostname, e.g. `vpn.example.com`.
    pub portal: String,
    pub username: String,
    pub auth: AuthMode,
    /// Tunnel backend. Configs written before this field existed load as
    /// `Openconnect`.
    #[serde(default)]
    pub backend: Backend,
    /// Answer repeated "Enter login credentials" prompts with the saved
    /// username and the keyring password instead of making the user retype
    /// them. The password is never stored in the config.
    #[serde(default = "default_true")]
    pub reuse_credentials: bool,
}

fn default_true() -> bool {
    true
}

/// Colour scheme for the app window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    /// Follow the desktop's dark/light preference.
    #[default]
    System,
    Dark,
    Light,
}

/// Misc app settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Reconnect automatically when the tunnel drops unexpectedly.
    pub reconnect_on_drop: bool,
    /// Start the GUI at login (installs/removes an XDG autostart entry).
    pub autostart: bool,
    /// Connect to the last used portal as soon as the app starts, e.g. right
    /// after logging in when combined with `autostart`.
    pub connect_at_startup: bool,
    /// OpenConnect binary to spawn. Empty string means "find `openconnect`
    /// on PATH". The NixOS module sets this to a sudo wrapper.
    pub openconnect_bin: String,
    /// gpclient binary to spawn (`Backend::Gpclient` portals). Empty string
    /// means "find `gpclient` on PATH".
    pub gpclient_bin: String,
    /// Closing or minimising hides the window to the tray instead of quitting,
    /// so the VPN can keep running without a taskbar entry.
    pub close_to_tray: bool,
    /// Colour scheme. Defaults to Following the desktop.
    #[serde(default)]
    pub theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            reconnect_on_drop: true,
            autostart: false,
            connect_at_startup: false,
            close_to_tray: false,
            openconnect_bin: String::new(),
            gpclient_bin: String::new(),
            theme: Theme::System,
        }
    }
}

impl Settings {
    /// Configured binary for `backend`, or None to find it on PATH.
    pub fn bin_for(&self, backend: &Backend) -> Option<PathBuf> {
        let s = match backend {
            Backend::Openconnect => self.openconnect_bin.trim(),
            Backend::Gpclient => self.gpclient_bin.trim(),
        };
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }
}

/// The whole on-disk config (no secrets — those live in the keyring).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub portals: Vec<PortalConfig>,
    /// Index into `portals` of the last successfully used portal.
    pub last_portal: Option<usize>,
    pub settings: Settings,
}

impl AppConfig {
    fn config_path() -> Option<PathBuf> {
        ProjectDirs::from("com", "Palisade", "palisade").map(|d| d.config_dir().join("config.json"))
    }

    pub fn load() -> Self {
        let user = Self::config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok());
        if let Some(cfg) = user {
            return cfg;
        }
        // System-wide fallback (written by the NixOS module): only used when
        // the user has no config of their own. Overridable for tests.
        let system_path = std::env::var("GPCONNECT_SYSTEM_CONFIG")
            .unwrap_or_else(|_| "/etc/palisade/config.json".into());
        std::fs::read_to_string(system_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::config_path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no config directory on this platform",
            ));
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap())?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut cfg = AppConfig::default();
        cfg.portals.push(PortalConfig {
            name: "Work".into(),
            portal: "vpn.example.com".into(),
            username: "alice".into(),
            auth: AuthMode::Saml,
            backend: Backend::Gpclient,
            reuse_credentials: true,
        });
        cfg.last_portal = Some(0);
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(back.settings.reconnect_on_drop);
    }

    #[test]
    fn legacy_config_gains_defaults() {
        // A config written before `backend`/`gpclient_bin` existed must still
        // load: portals default to the openconnect backend.
        let json = r#"{
            "portals": [
                { "name": "ETSP", "portal": "vpn.etsp.ru", "username": "u", "auth": "Password" }
            ],
            "last_portal": 0,
            "settings": { "reconnect_on_drop": false, "autostart": false,
                          "openconnect_bin": "/opt/wrapper" }
        }"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.portals[0].backend, Backend::Openconnect);
        // Reuse defaults to on: an existing portal should not start asking for
        // credentials again just because the field did not exist yet.
        assert!(cfg.portals[0].reuse_credentials);
        assert_eq!(cfg.settings.gpclient_bin, "");
        assert_eq!(cfg.settings.openconnect_bin, "/opt/wrapper");
        // Colour scheme defaults to following the desktop.
        assert_eq!(cfg.settings.theme, Theme::System);
    }

    #[test]
    fn bin_for_backend() {
        let mut s = Settings::default();
        assert_eq!(s.bin_for(&Backend::Openconnect), None);
        assert_eq!(s.bin_for(&Backend::Gpclient), None);
        s.openconnect_bin = "  ".into();
        s.gpclient_bin = "/usr/bin/gpclient".into();
        assert_eq!(s.bin_for(&Backend::Openconnect), None);
        assert_eq!(
            s.bin_for(&Backend::Gpclient),
            Some(PathBuf::from("/usr/bin/gpclient"))
        );
    }

    #[test]
    fn defaults_when_missing() {
        // Point XDG_CONFIG_HOME at an empty dir so the test is independent of
        // the machine's real config (and of /etc/palisade provisioned by the
        // NixOS module, which load() may legitimately pick up).
        let tmp = std::env::temp_dir().join("palisade-test-empty-config");
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: tests run single-threaded per process; we only read the var
        // inside AppConfig::load.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        unsafe { std::env::set_var("GPCONNECT_SYSTEM_CONFIG", tmp.join("nope.json")) };
        assert_eq!(AppConfig::load(), AppConfig::default());
    }
}
