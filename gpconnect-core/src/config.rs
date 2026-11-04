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
}

impl Default for Settings {
    fn default() -> Self {
        Self { reconnect_on_drop: true, autostart: false }
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
        let Some(path) = Self::config_path() else { return Self::default() };
        std::fs::read_to_string(path)
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
        assert_eq!(AppConfig::load(), AppConfig::default());
    }
}
