use std::collections::VecDeque;

use palisade_core::{
    delete_password, get_password, store_password, AppConfig, AuthMode, Backend, ConnEvent,
    ConnState, PortalConfig, Session, Theme,
};
use tokio::sync::mpsc::UnboundedReceiver;

const MAX_LOG_LINES: usize = 500;
const SHOW_LOG_LINES: usize = 60;

/// Remove ANSI escape sequences (CSI and OSC) from one line of child output.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI: ESC [ ... final byte in @..~
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... terminated by BEL or ESC \
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

pub struct GpConnect {
    config: AppConfig,
    session: Session,
    state: ConnState,
    events: UnboundedReceiver<ConnEvent>,
    logs: VecDeque<String>,
    /// Current connection stage ("Retrieving configuration…"), if any.
    stage: Option<String>,
    /// Password prompt state: (portal index, entered password, remember flag).
    password_prompt: Option<(usize, String, bool)>,
    /// OTP challenge prompt state: (message, entered code).
    challenge_prompt: Option<(String, String)>,
    /// Editor state for adding a new portal.
    editor: Option<PortalEditor>,
    selected: Option<usize>,
    status_message: Option<String>,
    /// System tray, when the desktop provides a StatusNotifier host.
    tray: Option<crate::tray::TrayHandle>,
    /// Set while the window is hidden in the tray.
    hidden_to_tray: bool,
    /// Set by the Quit tray action: close for real even with close-to-tray on.
    quitting: bool,
    /// Active bottom tab (replaces the collapsing sections). None means all
    /// tabs are closed and only the status box is visible.
    tab: Option<Tab>,
    /// "Connect at login" fires once per process.
    startup_connect_done: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Portals,
    Log,
    Settings,
}

struct PortalEditor {
    name: String,
    portal: String,
    username: String,
    auth: AuthMode,
    backend: Backend,
    reuse_credentials: bool,
}

impl PortalEditor {
    fn new() -> Self {
        Self {
            name: String::new(),
            portal: String::new(),
            username: String::new(),
            auth: AuthMode::Password,
            backend: Backend::Openconnect,
            reuse_credentials: true,
        }
    }

    fn from_config(p: &PortalConfig) -> Self {
        Self {
            name: p.name.clone(),
            portal: p.portal.clone(),
            username: p.username.clone(),
            auth: p.auth.clone(),
            backend: p.backend.clone(),
            reuse_credentials: p.reuse_credentials,
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
            backend: self.backend.clone(),
            reuse_credentials: self.reuse_credentials,
        }
    }
}

impl GpConnect {
    pub fn new(config: AppConfig, session: Session, events: UnboundedReceiver<ConnEvent>) -> Self {
        let selected = config.last_portal.filter(|&i| i < config.portals.len());
        Self {
            config,
            session,
            state: ConnState::Disconnected,
            events,
            logs: VecDeque::new(),
            stage: None,
            password_prompt: None,
            challenge_prompt: None,
            editor: None,
            selected,
            status_message: None,
            tray: crate::tray::TrayHandle::start(),
            hidden_to_tray: false,
            quitting: false,
            tab: None,
            startup_connect_done: false,
        }
    }

    /// Connect once, on the first frame, when "Connect at login" is on.
    /// Runs here rather than in `new` so the session channel is already being
    /// polled and the UI can show the progress it triggers.
    /// Hide the window instead of quitting when the tray can host it.
    ///
    /// Cancel is sent before the viewport processes Close, so the app keeps
    /// running (and the VPN stays up) while the dock icon disappears.
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if self.quitting {
            return;
        }
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if !self.config.settings.close_to_tray || self.tray.is_none() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        // Wayland has no set_visible and cannot unminimise, so minimising is
        // the only hide that works and focus is how we come back. On X11 the
        // Visible pair does the real work.
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        self.hidden_to_tray = true;
        tracing::info!("window hidden to tray; the session keeps running");
    }

    fn maybe_connect_at_startup(&mut self) {
        if self.startup_connect_done {
            return;
        }
        self.startup_connect_done = true;
        if !self.config.settings.connect_at_startup {
            return;
        }
        if let Some(idx) = self.selected {
            tracing::info!("connect at login: connecting to portal {idx}");
            self.start_connect(idx);
        }
    }

    /// Apply the configured colour scheme. `System` follows the desktop and
    /// re-applies when the desktop flips, because eframe reports it per frame.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        let chosen = match self.config.settings.theme {
            // Read the desktop preference on its own, not from the visuals we
            // applied earlier — otherwise `System` just echoes whatever theme
            // is currently active and stops following the desktop.
            Theme::System => ctx
                .input(|i| i.raw.system_theme)
                .unwrap_or(egui::Theme::Dark),
            Theme::Dark => egui::Theme::Dark,
            Theme::Light => egui::Theme::Light,
        };
        let want_dark = chosen == egui::Theme::Dark;
        if ctx.style().visuals.dark_mode != want_dark {
            ctx.set_theme(chosen);
        }
        // Match the reference palette: primary blue for the main action, soft
        // panel background, thin borders. Applied every frame is cheap and
        // keeps the visuals consistent after a theme flip.
        ctx.style_mut(|style| {
            style.visuals.widgets.active.bg_fill = primary_blue();
            style.visuals.widgets.active.fg_stroke.color = egui::Color32::WHITE;
            style.visuals.selection.bg_fill = primary_blue();
        });
    }

    /// Apply actions requested from the tray menu.
    fn handle_tray(&mut self, ctx: &egui::Context) {
        let Some(tray) = &self.tray else { return };
        let dark = ctx.style().visuals.dark_mode;
        tray.set_state(crate::tray::TrayState::from_conn(&self.state), dark);
        for action in tray.take_actions() {
            use crate::tray::TrayAction;
            match action {
                TrayAction::ShowWindow => {
                    // Wayland ignores Visible and refuses to unminimise, so the
                    // only reliable way to bring the window forward is to ask
                    // the compositor for focus (which raises it).
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                    self.hidden_to_tray = false;
                }
                TrayAction::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                TrayAction::Connect => {
                    if let Some(idx) = self.selected {
                        let busy = matches!(
                            self.state,
                            ConnState::Authenticating
                                | ConnState::Connecting
                                | ConnState::Connected(_)
                        );
                        if !busy {
                            self.start_connect(idx);
                        }
                    }
                }
                TrayAction::Disconnect => self.session.disconnect(),
            }
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                ConnEvent::State(s) => {
                    self.stage = None;
                    self.state = s;
                }
                ConnEvent::Stage(s) => self.stage = Some(s),
                ConnEvent::Challenge(msg) => {
                    if self.challenge_prompt.is_none() {
                        self.challenge_prompt = Some((msg, String::new()));
                    }
                }
                ConnEvent::Log(l) => {
                    // gpclient identifies entered characters by redrawing the
                    // prompt ("? Password: ***" + cursor moves), not by echoing
                    // them, so the old password never appears — but the control
                    // codes make the log unreadable.
                    let l = strip_ansi(&l);
                    if l.trim().is_empty() {
                        continue;
                    }
                    self.logs.push_back(l);
                    while self.logs.len() > MAX_LOG_LINES {
                        self.logs.pop_front();
                    }
                }
            }
        }
    }

    fn start_connect(&mut self, idx: usize) {
        let Some(portal) = self.config.portals.get(idx).cloned() else {
            return;
        };
        self.config.last_portal = Some(idx);
        let _ = self.config.save();

        if portal.auth == AuthMode::Password {
            if let Some(pw) = get_password(&portal.portal, &portal.username) {
                self.status_message = None;
                let bin = self.bin_for(&portal.backend);
                let _ = self.session.connect(portal, Some(pw), bin);
                return;
            }
            // Not in the keyring: prompt, with an option to store it.
            self.password_prompt = Some((idx, String::new(), true));
            return;
        }

        self.status_message = Some("Waiting for browser authentication…".into());
        let bin = self.bin_for(&portal.backend);
        let _ = self.session.connect(portal, None, bin);
    }

    /// Binary from settings for `backend` (None = find it on PATH).
    fn bin_for(&self, backend: &Backend) -> Option<std::path::PathBuf> {
        self.config.settings.bin_for(backend)
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.status_message = Some(format!("Failed to save config: {e}"));
        }
    }
}

impl eframe::App for GpConnect {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Let a tray click wake a sleeping window: without this the action
        // only lands on the next ordinary frame, which may never come.
        if let Some(tray) = &self.tray {
            tray.attach(ctx.clone());
        }
        self.drain_events();
        self.maybe_connect_at_startup();
        // Apply the theme first: the tray reads dark_mode from ctx.style().
        self.apply_theme(ctx);
        self.handle_tray(ctx);
        self.handle_close_request(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.config.portals.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label("No portals configured yet.");
                    if ui.button("➕ Add portal…").clicked() {
                        self.editor = Some(PortalEditor::new());
                    }
                });
                return;
            }

            // --- Status-first panel, modeled on the official client ---

            // "Connect to:" portal selector.
            let mut selected = self.selected.unwrap_or(0);
            let prev = selected;
            egui::ComboBox::from_label("Connect to")
                .selected_text(
                    self.config
                        .portals
                        .get(selected)
                        .map(|p| p.name.clone())
                        .unwrap_or_default(),
                )
                .show_ui(ui, |ui| {
                    for (idx, p) in self.config.portals.iter().enumerate() {
                        ui.selectable_value(&mut selected, idx, &p.name);
                    }
                });
            if selected != prev {
                self.selected = Some(selected);
            }

            // Big connect/disconnect action.
            let busy = matches!(
                self.state,
                ConnState::Authenticating | ConnState::Connecting | ConnState::Connected(_)
            );
            let text = if busy { "Disconnect" } else { "Connect" };
            let button = egui::Button::new(egui::RichText::new(text).size(15.0))
                .fill(primary_blue())
                .corner_radius(4.0)
                .min_size(egui::vec2(ui.available_width(), 36.0));
            if ui.add(button).clicked() {
                if busy {
                    self.session.disconnect();
                } else if let Some(idx) = self.selected {
                    self.start_connect(idx);
                }
            }

            // Status box: centered dot + title + subline, as in the reference.
            let (dot_color, title) = match &self.state {
                ConnState::Disconnected => {
                    (egui::Color32::from_rgb(0x8a, 0x8a, 0x8a), "Not Connected")
                }
                ConnState::Authenticating => {
                    (egui::Color32::from_rgb(0xc8, 0x87, 0x1a), "Authenticating…")
                }
                ConnState::Connecting => (egui::Color32::from_rgb(0xc8, 0x87, 0x1a), "Connecting…"),
                ConnState::Connected(_) => (egui::Color32::from_rgb(0x2e, 0x8b, 0x3d), "Connected"),
                ConnState::Error(_) => (egui::Color32::from_rgb(0xc0, 0x39, 0x2b), "Error"),
            };
            let subline = match &self.state {
                ConnState::Disconnected => "Select a portal and press Connect".to_string(),
                ConnState::Authenticating | ConnState::Connecting => self
                    .stage
                    .clone()
                    .unwrap_or_else(|| "Checking credentials".into()),
                ConnState::Connected(info) => {
                    if info.ip.is_empty() {
                        info.gateway.clone()
                    } else {
                        format!("{} · {}", info.gateway, info.ip)
                    }
                }
                ConnState::Error(e) => e.clone(),
            };
            let panel_bg = if ui.visuals().dark_mode {
                egui::Color32::from_rgb(0x2a, 0x2a, 0x2a)
            } else {
                egui::Color32::from_rgb(0xe9, 0xe9, 0xe9)
            };
            let panel_border = if ui.visuals().dark_mode {
                egui::Color32::from_rgb(0x45, 0x45, 0x45)
            } else {
                egui::Color32::from_rgb(0xd2, 0xd2, 0xd2)
            };
            egui::Frame::NONE
                .fill(panel_bg)
                .stroke(egui::Stroke::new(1.0_f32, panel_border))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(16, 16))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 6.0, dot_color);
                            let title_color = if ui.visuals().dark_mode {
                                egui::Color32::from_rgb(0xf0, 0xf0, 0xf0)
                            } else {
                                egui::Color32::from_rgb(0x1f, 0x1f, 0x1f)
                            };
                            ui.label(
                                egui::RichText::new(title)
                                    .size(16.0)
                                    .strong()
                                    .color(title_color),
                            );
                        });
                        ui.add_space(6.0);
                        let sub_color = if ui.visuals().dark_mode {
                            egui::Color32::from_rgb(0xa0, 0xa0, 0xa0)
                        } else {
                            egui::Color32::from_rgb(0x6a, 0x6a, 0x6a)
                        };
                        ui.colored_label(sub_color, subline);
                    });
                });
            ui.add_space(8.0);
            if let Some(stage) = &self.stage {
                ui.label(stage);
            }
            if let Some(msg) = &self.status_message {
                ui.label(msg.as_str());
            }

            ui.add_space(4.0);

            // --- Tab bar at the top, content below ---
            let spacing = ui.spacing().item_spacing.x;
            let tab_w = (ui.available_width() - spacing * 2.0) / 3.0;
            ui.horizontal(|ui| {
                for (tab, label) in [
                    (Tab::Portals, "Portals"),
                    (Tab::Log, "Log"),
                    (Tab::Settings, "Settings"),
                ] {
                    let selected = self.tab == Some(tab);
                    let btn = egui::Button::new(label)
                        .min_size(egui::vec2(tab_w, 26.0))
                        .fill(if selected {
                            primary_blue()
                        } else {
                            ui.visuals().widgets.inactive.bg_fill
                        });
                    if ui.add(btn).clicked() {
                        self.tab = if selected { None } else { Some(tab) };
                    }
                }
            });
            ui.separator();
            match self.tab {
                Some(Tab::Portals) => {
                    self.portal_list(ui);
                    if ui.button("➕ Add portal…").clicked() {
                        self.editor = Some(PortalEditor::new());
                    }
                }
                Some(Tab::Log) => self.log_view(ui),
                Some(Tab::Settings) | None => {}
            }
            if self.tab == Some(Tab::Settings) {
                let mut settings = self.config.settings.clone();
                // The autostart entry is the source of truth: if it was removed
                // outside the app (or never written), the checkbox must not
                // claim otherwise.
                settings.autostart = crate::autostart::is_enabled();
                ui.horizontal(|ui| {
                    ui.label("Theme:");
                    ui.selectable_value(&mut settings.theme, Theme::System, "System");
                    ui.selectable_value(&mut settings.theme, Theme::Light, "Light");
                    ui.selectable_value(&mut settings.theme, Theme::Dark, "Dark");
                });
                ui.separator();
                ui.checkbox(
                    &mut settings.reconnect_on_drop,
                    "Reconnect on unexpected drop",
                );
                ui.checkbox(&mut settings.autostart, "Launch at login");
                ui.checkbox(&mut settings.connect_at_startup, "Connect at login")
                    .on_hover_text("Connect to the last used portal as soon as the app starts.");
                if !settings.autostart && settings.connect_at_startup {
                    ui.weak("Needs “Launch at login” to run when you log in.");
                }
                ui.checkbox(&mut settings.close_to_tray, "Close to tray")
                    .on_hover_text(
                        "Closing or minimising hides the window and removes the \
                         dock entry; the VPN keeps running. Reopen from the tray \
                         icon, quit from its menu.",
                    );
                if settings.close_to_tray && self.tray.is_none() {
                    ui.weak("No system tray on this desktop — closing still quits.");
                }
                ui.horizontal(|ui| {
                    ui.label("openconnect binary:");
                    ui.add(
                        egui::TextEdit::singleline(&mut settings.openconnect_bin)
                            .desired_width(ui.available_width())
                            .hint_text("leave empty to find it on PATH"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("gpclient binary:");
                    ui.add(
                        egui::TextEdit::singleline(&mut settings.gpclient_bin)
                            .desired_width(ui.available_width())
                            .hint_text("leave empty to find it on PATH"),
                    );
                });
                if settings != self.config.settings {
                    let autostart_changed = settings.autostart != self.config.settings.autostart;
                    self.config.settings = settings;
                    self.save_config();
                    if autostart_changed {
                        // The checkbox must reflect reality, not just intent:
                        // report a failure instead of silently doing nothing.
                        if let Err(e) = crate::autostart::apply(self.config.settings.autostart) {
                            self.status_message = Some(e);
                        }
                    }
                }
            }
        });

        self.password_window(ctx);
        self.challenge_window(ctx);
        self.editor_window(ctx);
    }

    /// Closing the window ends the VPN too, so no root client is left owning
    /// the tun device after the app is gone.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        tracing::info!("exiting: tearing down the VPN session");
        self.session.disconnect();
    }
}

/// The reference design's primary blue (buttons and selection).
fn primary_blue() -> egui::Color32 {
    egui::Color32::from_rgb(0x34, 0x65, 0xa4)
}

impl GpConnect {
    fn portal_list(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .max_height(140.0)
            .show(ui, |ui| {
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
        {
            // A read-only multiline TextEdit (not per-line labels) so the
            // whole log can be selected with Ctrl+A and copied.
            let start = self.logs.len().saturating_sub(SHOW_LOG_LINES * 4);
            let text: String = self
                .logs
                .iter()
                .skip(start)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let mut text = text;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(180.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                });
        }
    }

    fn password_window(&mut self, ctx: &egui::Context) {
        let mut open = self.password_prompt.is_some();
        let mut done = false;

        egui::Window::new("VPN Password")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let Some((idx, pw, remember)) = &mut self.password_prompt else {
                    return;
                };
                let mut cancel = false;
                let portal = self
                    .config
                    .portals
                    .get(*idx)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                let mut close_x = false;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("VPN Password").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            close_x = true;
                        }
                    });
                });
                if close_x {
                    cancel = true;
                }
                ui.separator();
                ui.label(format!("Password for {portal}:"));
                let submit = ui
                    .add(
                        egui::TextEdit::singleline(pw)
                            .password(true)
                            .desired_width(280.0),
                    )
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.checkbox(remember, "Remember in system keyring");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        if (ui.button("Connect").clicked() || submit) && !pw.is_empty() {
                            done = true;
                        }
                    });
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
                    let bin = self.config.settings.bin_for(&portal.backend);
                    let _ = self.session.connect(portal, Some(pw), bin);
                }
            }
        }
        if !open {
            self.password_prompt = None;
        }
    }

    fn challenge_window(&mut self, ctx: &egui::Context) {
        let mut open = self.challenge_prompt.is_some();
        let mut submit = false;

        egui::Window::new("Verification code")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let Some((msg, code)) = &mut self.challenge_prompt else {
                    return;
                };
                ui.label(msg.as_str());
                let enter = ui
                    .add(egui::TextEdit::singleline(code).desired_width(200.0))
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter {
                    submit = true;
                }
                ui.add_space(8.0);
                if ui.button("OK").clicked() && !code.is_empty() {
                    submit = true;
                }
            });

        if submit {
            if let Some((_, code)) = self.challenge_prompt.take() {
                let _ = self.session.respond(&code);
            }
        }
        if !open {
            self.challenge_prompt = None;
        }
    }

    fn editor_window(&mut self, ctx: &egui::Context) {
        let mut open = self.editor.is_some();
        let mut save = false;
        let mut cancel = false;

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
                ui.horizontal(|ui| {
                    ui.label("Backend:");
                    ui.radio_value(&mut ed.backend, Backend::Openconnect, "openconnect");
                    ui.radio_value(&mut ed.backend, Backend::Gpclient, "gpclient");
                })
                .response
                .on_hover_text(
                    "gpclient handles portals whose MFA challenge forms make \
                     openconnect stall after the code is sent",
                );

                ui.add_space(8.0);
                ui.checkbox(
                    &mut ed.reuse_credentials,
                    "Reuse saved login and password for repeated prompts",
                )
                .on_hover_text(
                    "When the portal asks for credentials again, answer with the \
                     username above and the password from the system keyring \
                     instead of prompting. The password is never stored in the \
                     config file.",
                );
                // There is nothing to type here: the username is the field
                // above and the password lives in the keyring. Showing
                // editable-looking copies of them only invited confusion.
                let reuse = ed.reuse_credentials;
                ui.weak(if reuse {
                    "Repeated prompts are answered with the username above and the \
                     keyring password; only a verification code is asked."
                } else {
                    "Only the first prompt is answered automatically; later prompts \
                     wait for you."
                });
                ui.add_space(8.0);
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
                    if self.config.portals.get(idx).map(|p| p.name.clone())
                        == Some(entry.name.clone())
                    {
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

#[cfg(test)]
mod tests {
    use super::strip_ansi;

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
}
