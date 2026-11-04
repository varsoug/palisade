//! Palisade core: manages OpenConnect child processes that speak the
//! Palo Alto GlobalProtect protocol, plus config and credential storage.
//!
//! Secrets (VPN passwords) live only in the OS keyring, never on disk.

mod config;
mod session;

pub use config::{AppConfig, AuthMode, Backend, PortalConfig, Settings, Theme};
pub use session::{ConnEvent, ConnState, Session, TunnelInfo, DEFAULT_OPENCONNECT_BIN};

use keyring::Entry;

const KEYRING_SERVICE: &str = "palisade";

/// Store a portal password in the OS keyring.
pub fn store_password(portal: &str, username: &str, password: &str) -> Result<(), keyring::Error> {
    Entry::new(KEYRING_SERVICE, &format!("{portal}|{username}"))?.set_password(password)
}

/// Read a portal password from the OS keyring (None if not stored).
pub fn get_password(portal: &str, username: &str) -> Option<String> {
    Entry::new(KEYRING_SERVICE, &format!("{portal}|{username}"))
        .ok()?
        .get_password()
        .ok()
}

/// Delete a stored portal password.
pub fn delete_password(portal: &str, username: &str) -> Result<(), keyring::Error> {
    Entry::new(KEYRING_SERVICE, &format!("{portal}|{username}"))?.delete_credential()
}
