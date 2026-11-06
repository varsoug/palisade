//! System tray (freedesktop StatusNotifierItem) for the running app.
//!
//! The tray lives on its own thread because `ksni` drives a zbus connection
//! with its own event loop; the GUI pushes state in and pulls actions out
//! through a small shared cell, the same lock-free-of-async pattern the rest
//! of the app uses.

use std::sync::{Arc, Mutex};

use ksni::blocking::TrayMethods as _;
use ksni::{menu::StandardItem, Tray};
use palisade_core::ConnState;

/// What the tray should tell the user right now.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TrayState {
    pub label: String,
    pub connected: bool,
    pub busy: bool,
}

impl TrayState {
    pub fn from_conn(state: &ConnState) -> Self {
        match state {
            ConnState::Disconnected => Self {
                label: "Not connected".into(),
                ..Default::default()
            },
            ConnState::Authenticating => Self {
                label: "Authenticating…".into(),
                busy: true,
                ..Default::default()
            },
            ConnState::Connecting => Self {
                label: "Connecting…".into(),
                busy: true,
                ..Default::default()
            },
            ConnState::Connected(info) => Self {
                label: if info.ip.is_empty() {
                    format!("Connected to {}", info.gateway)
                } else {
                    format!("Connected to {} ({})", info.gateway, info.ip)
                },
                connected: true,
                busy: false,
            },
            ConnState::Error(e) => Self {
                label: format!("Error: {e}"),
                ..Default::default()
            },
        }
    }
}

/// An action requested from the tray menu, consumed by the GUI each frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ShowWindow,
    Connect,
    Disconnect,
    Quit,
}

#[derive(Clone, Default)]
struct Shared {
    state: TrayState,
    actions: Vec<TrayAction>,
    /// Wakes the GUI loop. Without it a click only lands when the next frame
    /// happens to run, which never happens while the window sleeps.
    wake: Option<egui::Context>,
    /// Whether the desktop is in dark mode (selects white icons).
    dark: bool,
}

struct GpConnectTray {
    shared: Arc<Mutex<Shared>>,
}

/// Load a PNG icon into ksni's ARGB format (alpha first, then RGB).
fn load_icon(png: &[u8]) -> Option<ksni::Icon> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let w = info.width as usize;
    let h = info.height as usize;
    let rgba = &buf[..info.buffer_size()];
    // Status bars expect small icons (32-48px); the source PNGs are 1199x1312.
    // Downscale with simple box sampling, preserving aspect ratio.
    let max = 48usize;
    let scale = (w as f32 / max as f32).max(h as f32 / max as f32);
    let nw = (w as f32 / scale).round().max(1.0) as usize;
    let nh = (h as f32 / scale).round().max(1.0) as usize;
    let mut data = Vec::with_capacity(nw * nh * 4);
    for y in 0..nh {
        for x in 0..nw {
            let sx = (x as f32 * scale) as usize;
            let sy = (y as f32 * scale) as usize;
            let si = (sy.min(h - 1) * w + sx.min(w - 1)) * 4;
            // Bounds guard: the source buffer may be smaller than expected
            // (e.g. RGB instead of RGBA), so never read past its end.
            if si + 3 >= rgba.len() {
                continue;
            }
            // ksni expects ARGB: alpha, red, green, blue.
            data.extend_from_slice(&[rgba[si + 3], rgba[si], rgba[si + 1], rgba[si + 2]]);
        }
    }
    Some(ksni::Icon {
        width: nw as i32,
        height: nh as i32,
        data,
    })
}

/// The tray icon: the P-with-rampart mark, with a status overlay.
/// Black on light desktops, white on dark ones.
fn icon(state: &TrayState, dark: bool) -> Option<ksni::Icon> {
    let name = if state.busy {
        "running"
    } else if state.connected {
        "connected"
    } else if matches!(state.label.as_str(), s if s.starts_with("Error")) {
        "error"
    } else {
        "running"
    };
    let path = match (name, dark) {
        ("running", false) => include_bytes!("../../assets/tray-running.png").as_slice(),
        ("running", true) => include_bytes!("../../assets/tray-running-dark.png").as_slice(),
        ("connected", false) => include_bytes!("../../assets/tray-connected.png").as_slice(),
        ("connected", true) => include_bytes!("../../assets/tray-connected-dark.png").as_slice(),
        ("error", false) => include_bytes!("../../assets/tray-error.png").as_slice(),
        ("error", true) => include_bytes!("../../assets/tray-error-dark.png").as_slice(),
        _ => return None,
    };
    load_icon(path)
}

impl Tray for GpConnectTray {
    fn id(&self) -> String {
        "palisade".into()
    }

    /// Left click on the icon: bring the window up. Without this the icon only
    /// opens a menu, which reads as "the icon does nothing".
    fn activate(&mut self, _x: i32, _y: i32) {
        self.push(TrayAction::ShowWindow);
    }

    /// Middle click: toggle the tunnel without opening the window.
    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        let (connected, busy) = {
            let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            (s.state.connected, s.state.busy)
        };
        self.push(if connected || busy {
            TrayAction::Disconnect
        } else {
            TrayAction::Connect
        });
    }

    fn title(&self) -> String {
        "Palisade".into()
    }

    fn icon_name(&self) -> String {
        // Empty name forces the host to use our pixmap instead of a themed
        // system icon (which would override the custom mark).
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        icon(&s.state, s.dark).into_iter().collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        ksni::ToolTip {
            title: "Palisade".into(),
            description: s.state.label.clone(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let s = self
            .shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .clone();
        Vec::new()
            .into_iter()
            .chain(std::iter::once(ksni::MenuItem::Standard(StandardItem {
                label: s.label.clone(),
                enabled: false,
                ..Default::default()
            })))
            .chain(std::iter::once(ksni::MenuItem::Separator))
            .chain(std::iter::once(ksni::MenuItem::Standard(StandardItem {
                label: "Show window".into(),
                activate: Box::new(|this: &mut Self| this.push(TrayAction::ShowWindow)),
                ..Default::default()
            })))
            .chain(std::iter::once(ksni::MenuItem::Standard(StandardItem {
                label: if s.busy || s.connected {
                    "Disconnect".into()
                } else {
                    "Connect".into()
                },
                enabled: true,
                activate: Box::new(|this: &mut Self| {
                    let action = if this
                        .shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .state
                        .connected
                        || this
                            .shared
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .state
                            .busy
                    {
                        TrayAction::Disconnect
                    } else {
                        TrayAction::Connect
                    };
                    this.push(action);
                }),
                ..Default::default()
            })))
            .chain(std::iter::once(ksni::MenuItem::Separator))
            .chain(std::iter::once(ksni::MenuItem::Standard(StandardItem {
                label: "Quit".into(),
                activate: Box::new(|this: &mut Self| this.push(TrayAction::Quit)),
                ..Default::default()
            })))
            .collect()
    }
}

impl GpConnectTray {
    fn push(&mut self, action: TrayAction) {
        let wake = {
            let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            s.actions.push(action);
            s.wake.clone()
        };
        if let Some(ctx) = wake {
            ctx.request_repaint();
        }
    }
}

/// Handle to a running tray.
pub struct TrayHandle {
    shared: Arc<Mutex<Shared>>,
    handle: Option<ksni::blocking::Handle<GpConnectTray>>,
}

impl TrayHandle {
    /// Start the tray. Returns None when the desktop has no StatusNotifier
    /// host (plain X11/GNOME without an extension, headless sessions), in
    /// which case the app simply runs without one.
    pub fn start() -> Option<Self> {
        let shared: Arc<Mutex<Shared>> = Arc::new(Mutex::new(Shared::default()));
        let tray = GpConnectTray {
            shared: Arc::clone(&shared),
        };
        let handle = match tray.spawn() {
            Ok(h) => h,
            Err(e) => {
                tracing::info!("system tray unavailable: {e}");
                return None;
            }
        };
        Some(Self {
            shared,
            handle: Some(handle),
        })
    }

    pub fn set_state(&self, state: TrayState, dark: bool) {
        {
            let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if s.state == state && s.dark == dark {
                return;
            }
            s.state = state.clone();
            s.dark = dark;
        }
        if let Some(h) = &self.handle {
            // Update the tray's own state so ksni re-reads icon and tooltip.
            // `dark` must be carried too, otherwise a theme flip leaves the
            // old (black or white) icon in place.
            let _ = h.update(|tray| {
                let mut s = tray.shared.lock().unwrap_or_else(|e| e.into_inner());
                s.state = state.clone();
                s.dark = dark;
            });
        }
    }

    /// Hand the tray a context it can use to wake the GUI.
    pub fn attach(&self, ctx: egui::Context) {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).wake = Some(ctx);
    }

    pub fn take_actions(&self) -> Vec<TrayAction> {
        std::mem::take(
            &mut self
                .shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .actions,
        )
    }
}
