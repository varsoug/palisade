use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

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
}

/// Misc app settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Reconnect automatically when the tunnel drops unexpectedly.
    pub reconnect_on_drop: bool,
    /// Start the GUI at login (best-effort per platform).
    pub autostart: bool,
    /// OpenConnect binary to spawn. Empty string means "find `openconnect`
    /// on PATH". The NixOS module sets this to a sudo wrapper.
    pub openconnect_bin: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            reconnect_on_drop: true,
            autostart: false,
            openconnect_bin: String::new(),
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
        ProjectDirs::from("com", "GPConnect", "gpconnect")
            .map(|d| d.config_dir().join("config.json"))
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
            .unwrap_or_else(|_| "/etc/gpconnect/config.json".into());
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
        });
        cfg.last_portal = Some(0);
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(back.settings.reconnect_on_drop);
    }

    #[test]
    fn defaults_when_missing() {
        // Point XDG_CONFIG_HOME at an empty dir so the test is independent of
        // the machine's real config (and of /etc/gpconnect provisioned by the
        // NixOS module, which load() may legitimately pick up).
        let tmp = std::env::temp_dir().join("gpconnect-test-empty-config");
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: tests run single-threaded per process; we only read the var
        // inside AppConfig::load.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        unsafe { std::env::set_var("GPCONNECT_SYSTEM_CONFIG", tmp.join("nope.json")) };
        assert_eq!(AppConfig::load(), AppConfig::default());
    }
}
