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
}

struct GpConnectTray {
    shared: Arc<Mutex<Shared>>,
}

/// A 32×32 icon drawn in code: a shield-ish disc with a status colour, plus a
/// hole in the middle when connected. Avoids shipping a binary asset and gives
/// the tray an unambiguous connected/disconnected cue.
fn icon(state: &TrayState) -> ksni::Icon {
    const SIZE: i32 = 32;
    let mut data = vec![0u8; (SIZE * SIZE * 4) as usize];
    let (r, g, b) = if state.busy {
        (255u8, 190, 0)
    } else if state.connected {
        (60u8, 190, 90)
    } else {
        (150u8, 150u8, 150)
    };
    let c = (SIZE as f32 - 1.0) / 2.0;
    let outer = c - 1.0;
    let inner = c * 0.42;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let d = (dx * dx + dy * dy).sqrt();
            // Filled disc with a punched-out centre when connected, so the two
            // states differ in shape and not only in colour (colour-blind safe).
            let inside = d <= outer && !(state.connected && d <= inner);
            let alpha = if inside { 255 } else { 0 };
            let i = ((y * SIZE + x) * 4) as usize;
            data[i..i + 4].copy_from_slice(&[alpha, r, g, b]);
        }
    }
    ksni::Icon {
        width: SIZE,
        height: SIZE,
        data,
    }
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
            let s = self.shared.lock().unwrap();
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
        "network-vpn".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let s = self.shared.lock().unwrap();
        vec![icon(&s.state)]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let s = self.shared.lock().unwrap();
        ksni::ToolTip {
            title: "Palisade".into(),
            description: s.state.label.clone(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let s = self.shared.lock().unwrap().state.clone();
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
                    let action = if this.shared.lock().unwrap().state.connected
                        || this.shared.lock().unwrap().state.busy
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
            let mut s = self.shared.lock().unwrap();
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

    pub fn set_state(&self, state: TrayState) {
        {
            let mut s = self.shared.lock().unwrap();
            if s.state == state {
                return;
            }
            s.state = state;
        }
        if let Some(h) = &self.handle {
            let _ = h.update(|_| {});
        }
    }

    /// Hand the tray a context it can use to wake the GUI.
    pub fn attach(&self, ctx: egui::Context) {
        self.shared.lock().unwrap().wake = Some(ctx);
    }

    pub fn take_actions(&self) -> Vec<TrayAction> {
        std::mem::take(&mut self.shared.lock().unwrap().actions)
    }
}
