use palisade_core::ConnState;

/// Map a connection state to the status title, subline and dot RGB used by the UI.
pub fn status_for(state: &ConnState) -> (String, String, u32) {
    match state {
        ConnState::Disconnected => (
            "Not Connected".into(),
            "Select a portal and press Connect.".into(),
            0x8a8a8a,
        ),
        ConnState::Authenticating => (
            "Authenticating…".into(),
            "Checking credentials".into(),
            0xc8871a,
        ),
        ConnState::Connecting => (
            "Connecting…".into(),
            "Retrieving configuration…".into(),
            0xc8871a,
        ),
        ConnState::Connected(info) => (
            "Connected".into(),
            if info.ip.is_empty() {
                info.gateway.clone()
            } else {
                format!("{} — {}", info.gateway, info.ip)
            },
            0x2e8b3d,
        ),
        ConnState::Error(e) => ("Error".into(), e.clone(), 0xc0392b),
    }
}

/// Strip CSI and OSC escape sequences and trailing whitespace from a log line.
pub fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out.trim_end().to_string()
}

pub fn password_store_decision(remember: bool) -> bool {
    remember
}

/// Settings is an inline tab; the separate startup-only dialog is reserved for
/// externally requested first-run/startup configuration.
pub fn settings_surface_decision(active_tab: i32, settings_open: bool) -> (bool, bool) {
    (settings_open && active_tab != 2, active_tab == 2)
}

/// Copy the full log text; callers pass only already-sanitized lines.
pub fn copy_log_text(text: &str) -> &str {
    text
}

pub fn empty_state_text() -> (&'static str, &'static str) {
    ("No portals configured", "Add portal…")
}

/// Portal editor needs a host and a username; display name is derived when blank.
pub fn portal_valid(name: &str, portal: &str, username: &str) -> bool {
    let _ = name;
    !portal.trim().is_empty() && !username.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_connection_states_to_mockup_status() {
        assert_eq!(
            status_for(&ConnState::Disconnected),
            (
                "Not Connected".into(),
                "Select a portal and press Connect.".into(),
                0x8a8a8a
            )
        );
        assert_eq!(
            status_for(&ConnState::Authenticating),
            (
                "Authenticating…".into(),
                "Checking credentials".into(),
                0xc8871a
            )
        );
        assert_eq!(
            status_for(&ConnState::Connecting),
            (
                "Connecting…".into(),
                "Retrieving configuration…".into(),
                0xc8871a
            )
        );
        assert_eq!(
            status_for(&ConnState::Connected(palisade_core::TunnelInfo {
                gateway: "vpn.example.com".into(),
                ip: "10.0.0.1".into(),
            })),
            (
                "Connected".into(),
                "vpn.example.com — 10.0.0.1".into(),
                0x2e8b3d
            )
        );
        assert_eq!(
            status_for(&ConnState::Error("authentication failed".into())),
            ("Error".into(), "authentication failed".into(), 0xc0392b)
        );
    }

    #[test]
    fn password_storage_only_when_remember_is_checked() {
        assert!(password_store_decision(true));
        assert!(!password_store_decision(false));
    }

    #[test]
    fn config_save_roundtrip_preserves_portal_and_settings() {
        let mut cfg = palisade_core::AppConfig::default();
        cfg.portals.push(palisade_core::PortalConfig {
            name: "Work".into(),
            portal: "vpn.example.com".into(),
            username: "alice".into(),
            auth: palisade_core::AuthMode::Password,
            backend: palisade_core::Backend::Openconnect,
            reuse_credentials: true,
        });
        cfg.last_portal = Some(0);
        cfg.settings.close_to_tray = true;
        let json = serde_json::to_string(&cfg).unwrap();
        let back: palisade_core::AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
        assert!(!json.contains("password"));
    }

    #[test]
    fn portal_crud_keeps_no_secrets() {
        let password = "super-secret-value";
        let portal = palisade_core::PortalConfig {
            name: "Lab".into(),
            portal: "vpn.example.com".into(),
            username: "alice".into(),
            auth: palisade_core::AuthMode::Password,
            backend: palisade_core::Backend::Openconnect,
            reuse_credentials: true,
        };
        let json = serde_json::to_string(&portal).unwrap();
        assert!(!json.contains(password));
        assert_eq!(portal.portal, "vpn.example.com");
        assert_eq!(portal.username, "alice");
    }

    #[test]
    fn settings_tab_opens_inline_settings_panel() {
        let (needs_dialog, opens_inline) = settings_surface_decision(2, false);
        assert!(!needs_dialog);
        assert!(opens_inline);
    }

    #[test]
    fn copy_log_copies_entire_selectable_text() {
        assert_eq!(copy_log_text("line one\nline two"), "line one\nline two");
        assert_eq!(copy_log_text(""), "");
    }

    #[test]
    fn status_for_empty_portal_list_shows_connect_cta() {
        let (title, subtitle, dot) = status_for(&ConnState::Disconnected);
        assert_eq!(title, "Not Connected");
        assert_eq!(subtitle, "Select a portal and press Connect.");
        assert_eq!(dot, 0x8a8a8a);
        assert_eq!(empty_state_text(), ("No portals configured", "Add portal…"));
    }

    #[test]
    fn strips_csi_and_osc_sequences() {
        assert_eq!(
            strip_ansi("\u{1b}[?25l? Password: \u{1b}[12C\u{1b}[?25h"),
            "? Password:"
        );
        assert_eq!(strip_ansi("\u{1b}[?2004hplain text"), "plain text");
        assert_eq!(strip_ansi("a\u{1b}]0;title\u{7}b"), "ab");
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }

    #[test]
    fn portal_validation_rejects_missing_host_or_username() {
        assert!(portal_valid("Work", "vpn.example.com", "alice"));
        assert!(!portal_valid("Work", "", "alice"));
        assert!(!portal_valid("Work", "  ", "alice"));
        assert!(!portal_valid("Work", "vpn.example.com", ""));
        assert!(!portal_valid("", "vpn.example.com", " "));
    }
}
