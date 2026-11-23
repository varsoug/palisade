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
