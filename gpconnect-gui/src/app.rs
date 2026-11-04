use std::collections::VecDeque;

use gpconnect_core::{
    delete_password, get_password, store_password, AppConfig, AuthMode, ConnEvent, ConnState,
    PortalConfig, Session,
};
use tokio::sync::mpsc::UnboundedReceiver;

const MAX_LOG_LINES: usize = 500;
const SHOW_LOG_LINES: usize = 60;

pub struct GpConnect {
    config: AppConfig,
    session: Session,
    state: ConnState,
    events: UnboundedReceiver<ConnEvent>,
    logs: VecDeque<String>,
    /// Password prompt state: (portal index, entered password, remember flag).
    password_prompt: Option<(usize, String, bool)>,
    /// Editor state for adding a new portal.
    editor: Option<PortalEditor>,
    selected: Option<usize>,
    status_message: Option<String>,
}

struct PortalEditor {
    name: String,
    portal: String,
    username: String,
    auth: AuthMode,
}

impl PortalEditor {
    fn new() -> Self {
        Self {
            name: String::new(),
            portal: String::new(),
            username: String::new(),
            auth: AuthMode::Password,
        }
    }

    fn from_config(p: &PortalConfig) -> Self {
        Self {
            name: p.name.clone(),
            portal: p.portal.clone(),
            username: p.username.clone(),
            auth: p.auth.clone(),
        }
    }

    fn valid(&self) -> bool {
        !self.portal.trim().is_empty() && !self.username.trim().is_empty()
    }

    fn to_config(&self) -> PortalConfig {
        PortalConfig {
            name: if self.name.trim().is_empty() {
                self.portal.trim().to_string()
            } else {
                self.name.trim().to_string()
            },
            portal: self.portal.trim().to_string(),
            username: self.username.trim().to_string(),
            auth: self.auth.clone(),
        }
    }
}

impl GpConnect {
    pub fn new(
        config: AppConfig,
        session: Session,
        events: UnboundedReceiver<ConnEvent>,
    ) -> Self {
        let selected = config.last_portal.filter(|&i| i < config.portals.len());
        Self {
            config,
            session,
            state: ConnState::Disconnected,
            events,
            logs: VecDeque::new(),
            password_prompt: None,
            editor: None,
            selected,
            status_message: None,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                ConnEvent::State(s) => self.state = s,
                ConnEvent::Log(l) => {
                    self.logs.push_back(l);
                    while self.logs.len() > MAX_LOG_LINES {
                        self.logs.pop_front();
                    }
                }
            }
        }
    }

    fn start_connect(&mut self, idx: usize) {
        let Some(portal) = self.config.portals.get(idx).cloned() else { return };
        self.config.last_portal = Some(idx);
        let _ = self.config.save();

        if portal.auth == AuthMode::Password {
            if let Some(pw) = get_password(&portal.portal, &portal.username) {
                self.status_message = None;
                let _ = self.session.connect(portal, Some(pw), None);
                return;
            }
            // Not in the keyring: prompt, with an option to store it.
            self.password_prompt = Some((idx, String::new(), true));
            return;
        }

        self.status_message = Some("Waiting for browser authentication…".into());
        let _ = self.session.connect(portal, None, None);
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.status_message = Some(format!("Failed to save config: {e}"));
        }
    }
}

impl eframe::App for GpConnect {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading("GPConnect");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.state_badge(ui);
                });
            });
            ui.label("Open-source GlobalProtect client (via OpenConnect)");
            ui.separator();

            if self.config.portals.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label("No portals configured yet.");
                    if ui.button("➕ Add portal…").clicked() {
                        self.editor = Some(PortalEditor::new());
                    }
                });
            } else {
                self.portal_list(ui);
            }

            ui.separator();

            // Connect / disconnect.
            let busy = matches!(
                self.state,
                ConnState::Authenticating | ConnState::Connecting | ConnState::Connected(_)
            );
            ui.horizontal(|ui| {
                if busy {
                    if ui.button("⏹ Disconnect").clicked() {
                        self.session.disconnect();
                    }
                } else if ui.button("▶ Connect").clicked() {
                    if let Some(idx) = self.selected {
                        self.start_connect(idx);
                    }
                }
            });

            if let Some(msg) = &self.status_message {
                ui.label(msg);
            }
            if let ConnState::Error(e) = &self.state {
                ui.colored_label(ui.visuals().error_fg_color, format!("Error: {e}"));
            }
            if let ConnState::Connected(info) = &self.state {
                ui.label(format!("Connected via {} — tunnel IP {}", info.gateway, info.ip));
            }

            ui.add_space(4.0);
            self.log_view(ui);

            ui.separator();
            ui.collapsing("Settings", |ui| {
                let mut settings = self.config.settings.clone();
                ui.checkbox(&mut settings.reconnect_on_drop, "Reconnect on unexpected drop");
                ui.checkbox(&mut settings.autostart, "Launch at login (best effort)");
                if settings != self.config.settings {
                    self.config.settings = settings;
                    self.save_config();
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::BOTTOM), |ui| {
                if ui.button("➕ Add portal…").clicked() {
                    self.editor = Some(PortalEditor::new());
                }
            });
        });

        self.password_window(ctx);
        self.editor_window(ctx);
    }
}

impl GpConnect {
    fn state_badge(&self, ui: &mut egui::Ui) {
        let (color, text) = match &self.state {
            ConnState::Disconnected => (ui.visuals().weak_text_color(), "Disconnected"),
            ConnState::Authenticating => (egui::Color32::GOLD, "Authenticating…"),
            ConnState::Connecting => (egui::Color32::GOLD, "Connecting…"),
            ConnState::Connected(_) => (egui::Color32::LIGHT_GREEN, "Connected"),
            ConnState::Error(_) => (ui.visuals().error_fg_color, "Error"),
        };
        ui.colored_label(color, text);
    }

    fn portal_list(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
            let mut to_remove = None;
            let mut to_edit = None;
            let selected = self.selected;

            for (idx, p) in self.config.portals.iter().enumerate() {
                ui.horizontal(|ui| {
                    let label = format!("{}  ({}, {})", p.name, p.portal, p.username);
                    if ui.selectable_label(selected == Some(idx), label).clicked() {
                        self.selected = Some(idx);
                    }
                    if ui.small_button("✏").on_hover_text("Edit").clicked() {
                        to_edit = Some(idx);
                    }
                    if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                        to_remove = Some(idx);
                    }
                });
            }

            if let Some(idx) = to_edit {
                self.editor = Some(PortalEditor::from_config(&self.config.portals[idx]));
            }
            if let Some(idx) = to_remove {
                let p = self.config.portals.remove(idx);
                let _ = delete_password(&p.portal, &p.username);
                if self.selected == Some(idx) {
                    self.selected = None;
                } else if let Some(s) = self.selected {
                    if s > idx {
                        self.selected = Some(s - 1);
                    }
                }
                self.save_config();
            }
        });
    }

    fn log_view(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Log", |ui| {
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .max_height(180.0)
                .show(ui, |ui| {
                    let start = self.logs.len().saturating_sub(SHOW_LOG_LINES * 4);
                    for line in self.logs.iter().skip(start) {
                        ui.monospace(line);
                    }
                });
        });
    }

    fn password_window(&mut self, ctx: &egui::Context) {
        let mut open = self.password_prompt.is_some();
        let mut done = false;

        egui::Window::new("VPN password")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let Some((idx, pw, remember)) = &mut self.password_prompt else { return };
                let portal = self
                    .config
                    .portals
                    .get(*idx)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                ui.label(format!("Password for {portal}:"));
                ui.add(egui::TextEdit::singleline(pw).password(true).desired_width(280.0));
                ui.checkbox(remember, "Remember in system keyring");
                ui.add_space(8.0);
                let mut cancel = false;
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() && !pw.is_empty() {
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                if cancel {
                    self.password_prompt = None;
                }
            });

        if done {
            if let Some((idx, pw, remember)) = self.password_prompt.take() {
                if let Some(portal) = self.config.portals.get(idx).cloned() {
                    if remember {
                        let _ = store_password(&portal.portal, &portal.username, &pw);
                    }
                    let _ = self.session.connect(portal, Some(pw), None);
                }
            }
        }
        if !open {
            self.password_prompt = None;
        }
    }

    fn editor_window(&mut self, ctx: &egui::Context) {
        let mut open = self.editor.is_some();
        let mut save = false;

        egui::Window::new("Portal")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let Some(ed) = &mut self.editor else { return };
                ui.label("Name:");
                ui.text_edit_singleline(&mut ed.name);
                ui.label("Portal hostname:");
                ui.text_edit_singleline(&mut ed.portal);
                ui.label("Username:");
                ui.text_edit_singleline(&mut ed.username);
                ui.horizontal(|ui| {
                    ui.label("Auth:");
                    ui.radio_value(&mut ed.auth, AuthMode::Password, "Password");
                    ui.radio_value(&mut ed.auth, AuthMode::Saml, "SAML (browser)");
                });
                ui.add_space(8.0);
                let mut cancel = false;
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() && ed.valid() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                if cancel {
                    self.editor = None;
                }
            });

        if save {
            if let Some(ed) = self.editor.take() {
                let entry = ed.to_config();
                if let Some(idx) = self.selected.filter(|&i| i < self.config.portals.len()) {
                    // Overwrite the selected entry only if the user opened the
                    // editor from an existing portal; otherwise append.
                    if self.config.portals.get(idx).map(|p| p.name.clone()) == Some(entry.name.clone()) {
                        self.config.portals[idx] = entry;
                        self.save_config();
                        return;
                    }
                }
                self.config.portals.push(entry);
                self.selected = Some(self.config.portals.len() - 1);
                self.save_config();
            }
        }
        if !open {
            self.editor = None;
        }
    }
}
