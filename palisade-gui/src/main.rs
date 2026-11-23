mod autostart;
mod bridge;
#[cfg(unix)]
mod privilege;
mod single_instance;
mod tray;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use copypasta::ClipboardProvider;
use palisade_core::{
    delete_password, get_password, store_password, AppConfig, AuthMode, Backend, ConnEvent,
    ConnState, PortalConfig, Session, Theme,
};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, VecModel};
use tokio::sync::mpsc::UnboundedReceiver;

slint::include_modules!();

use bridge::{credential_valid, portal_valid, prompt_hint, status_for, strip_ansi};

const MAX_LOG_LINES: usize = 500;

struct Controller {
    ui: AppWindow,
    config: AppConfig,
    session: Session,
    events: UnboundedReceiver<ConnEvent>,
    state: ConnState,
    stage: Option<String>,
    logs: VecDeque<String>,
    selected: Option<usize>,
    challenge: Option<String>,
    password_portal: Option<usize>,
    editing: Option<usize>,
    notice: Option<String>,
    tray: Option<tray::TrayHandle>,
    hidden_to_tray: bool,
    quitting: bool,
}

impl Controller {
    fn new(
        ui: AppWindow,
        config: AppConfig,
        session: Session,
        events: UnboundedReceiver<ConnEvent>,
    ) -> Self {
        let selected = config.last_portal.filter(|&i| i < config.portals.len());
        let mut c = Self {
            ui,
            config,
            session,
            events,
            state: ConnState::Disconnected,
            stage: None,
            logs: VecDeque::new(),
            selected,
            challenge: None,
            password_portal: None,
            editing: None,
            notice: None,
            tray: tray::TrayHandle::start(),
            hidden_to_tray: false,
            quitting: false,
        };
        c.refresh_portals();
        c.refresh_status();
        c.refresh_settings();
        c.apply_theme();
        if let Some(tray) = c.tray.as_ref() {
            tray.attach(&c.ui);
        }
        if c.config.settings.connect_at_startup {
            c.start_connect();
        }
        c
    }

    fn selected_portal(&self) -> Option<&PortalConfig> {
        self.selected.and_then(|i| self.config.portals.get(i))
    }

    fn refresh_portals(&mut self) {
        let names: Vec<SharedString> = self
            .config
            .portals
            .iter()
            .map(|p| SharedString::from(p.name.as_str()))
            .collect();
        let metas: Vec<SharedString> = self
            .config
            .portals
            .iter()
            .map(|p| SharedString::from(format!("{} — {}", p.name, p.portal)))
            .collect();
        self.ui
            .set_portal_names(ModelRc::new(VecModel::from(names)));
        self.ui
            .set_portal_metas(ModelRc::new(VecModel::from(metas)));
        self.ui
            .set_selected_portal(self.selected.unwrap_or(0) as i32);
        self.ui.set_empty(self.config.portals.is_empty());
        if self.config.portals.is_empty() {
            let (heading, cta) = bridge::empty_state_text();
            self.ui.set_status_title(SharedString::from(heading));
            self.ui.set_message(SharedString::from(cta));
        }
        let portal = self.selected_portal();
        self.ui.set_portal_label(
            portal
                .map(|p| SharedString::from(format!("{} ({})", p.name, p.portal)))
                .unwrap_or_else(|| SharedString::from("Select a portal")),
        );
        self.ui.set_portal_name(
            portal
                .map(|p| SharedString::from(p.name.as_str()))
                .unwrap_or_default(),
        );
        self.ui.set_portal_host(
            portal
                .map(|p| SharedString::from(p.portal.as_str()))
                .unwrap_or_default(),
        );
        self.ui.set_username(
            portal
                .map(|p| SharedString::from(p.username.as_str()))
                .unwrap_or_default(),
        );
    }

    fn refresh_status(&mut self) {
        let (title, subtitle, dot) = status_for(&self.state);
        let subtitle = if matches!(
            self.state,
            ConnState::Authenticating | ConnState::Connecting
        ) {
            self.stage.clone().unwrap_or(subtitle)
        } else {
            subtitle
        };
        let (portal_name, username, tunnel_ip) = match &self.state {
            ConnState::Connected(info) => (
                self.selected_portal()
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| info.gateway.clone()),
                self.selected_portal()
                    .map(|p| p.username.clone())
                    .unwrap_or_default(),
                info.ip.clone(),
            ),
            _ => (String::new(), String::new(), String::new()),
        };
        self.ui.set_status_title(SharedString::from(title.as_str()));
        self.ui
            .set_status_subtitle(SharedString::from(subtitle.as_str()));
        self.ui
            .set_error_message(SharedString::from(match &self.state {
                ConnState::Error(e) => e.as_str(),
                _ => "",
            }));
        self.ui.set_status_dot(slint::Color::from_argb_u8(
            0xff,
            (dot >> 16) as u8,
            (dot >> 8) as u8,
            dot as u8,
        ));
        self.ui
            .set_connected(matches!(self.state, ConnState::Connected(_)));
        self.ui.set_busy(matches!(
            self.state,
            ConnState::Authenticating | ConnState::Connecting
        ));
        self.ui
            .set_show_details(matches!(self.state, ConnState::Connected(_)));
        self.ui.set_username(SharedString::from(username.as_str()));
        self.ui
            .set_tunnel_ip(SharedString::from(tunnel_ip.as_str()));
        let log_status = if portal_name.is_empty() {
            format!("{} — Palisade", title)
        } else {
            format!("{} — {}", title, portal_name)
        };
        self.ui
            .set_log_status(SharedString::from(log_status.as_str()));
        self.ui.set_message(
            self.notice
                .as_deref()
                .map(SharedString::from)
                .unwrap_or_default(),
        );
        self.refresh_prompt_state();
    }

    fn refresh_prompt_state(&mut self) {
        let prompting = self.password_portal.is_some() || self.challenge.is_some();
        let saml_waiting = self.saml_waiting();
        self.ui.set_prompting(prompting);
        self.ui.set_saml_waiting(saml_waiting);
        self.ui
            .set_prompt_message(SharedString::from(prompt_hint(prompting, saml_waiting)));
    }

    fn saml_waiting(&self) -> bool {
        matches!(
            self.state,
            ConnState::Authenticating | ConnState::Connecting
        ) && self
            .selected_portal()
            .is_some_and(|p| p.auth == AuthMode::Saml)
    }

    fn refresh_log(&self) {
        let lines: Vec<SharedString> = self
            .logs
            .iter()
            .rev()
            .take(200)
            .map(|l| SharedString::from(l.as_str()))
            .collect();
        let text = lines
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.ui.set_log_lines(ModelRc::new(VecModel::from(lines)));
        self.ui.set_log_text(SharedString::from(text.as_str()));
    }

    fn refresh_settings(&self) {
        let s = &self.config.settings;
        self.ui.set_theme(match s.theme {
            Theme::System => 0,
            Theme::Light => 1,
            Theme::Dark => 2,
        });
        self.ui.set_reconnect(s.reconnect_on_drop);
        self.ui.set_launch_at_login(s.autostart);
        self.ui.set_connect_at_login(s.connect_at_startup);
        self.ui.set_close_to_tray(s.close_to_tray);
        self.ui
            .set_openconnect_bin(SharedString::from(s.openconnect_bin.as_str()));
        self.ui
            .set_gpclient_bin(SharedString::from(s.gpclient_bin.as_str()));
    }

    fn apply_theme(&self) {
        let palette = self.ui.global::<AppPalette>();
        palette.set_dark(matches!(self.config.settings.theme, Theme::Dark));
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.notice = Some(format!("Failed to save config: {e}"));
            self.refresh_status();
        }
    }

    fn start_connect(&mut self) {
        let Some(idx) = self.selected else {
            self.notice = Some("Add a portal first.".into());
            self.refresh_status();
            return;
        };
        let Some(portal) = self.config.portals.get(idx).cloned() else {
            return;
        };
        self.config.last_portal = Some(idx);
        self.save_config();
        if portal.auth == AuthMode::Password {
            if let Some(pw) = get_password(&portal.portal, &portal.username) {
                self.connect_with_password(idx, pw, false);
                return;
            }
            self.password_portal = Some(idx);
            self.ui
                .set_portal_name(SharedString::from(portal.name.as_str()));
            self.ui
                .set_portal_host(SharedString::from(portal.portal.as_str()));
            self.ui
                .set_username(SharedString::from(portal.username.as_str()));
            self.ui.set_password(SharedString::from(""));
            self.ui.set_remember(true);
            self.ui.set_password_open(true);
            self.refresh_prompt_state();
            return;
        }
        self.connect_with_password(idx, String::new(), false);
    }

    fn connect_with_password(&mut self, idx: usize, password: String, remember: bool) {
        let Some(portal) = self.config.portals.get(idx).cloned() else {
            return;
        };
        if remember {
            let _ = store_password(&portal.portal, &portal.username, &password);
        }
        self.password_portal = None;
        self.ui.set_password_open(false);
        self.ui.set_password(SharedString::from(""));
        self.selected = Some(idx);
        self.refresh_prompt_state();
        let bin = self.config.settings.bin_for(&portal.backend);
        let _ = self.session.connect(portal, Some(password), bin);
    }

    fn submit_password(&mut self) {
        let Some(idx) = self.password_portal else {
            self.ui.set_password_open(false);
            self.refresh_prompt_state();
            return;
        };
        let password = self.ui.get_password().to_string();
        if !credential_valid(&password) {
            self.notice = Some("Enter a password to connect.".into());
            self.refresh_status();
            return;
        }
        let remember = self.ui.get_remember();
        self.connect_with_password(idx, password, remember);
    }

    fn cancel_password(&mut self) {
        self.password_portal = None;
        self.ui.set_password_open(false);
        self.ui.set_password(SharedString::from(""));
        self.notice = None;
        self.refresh_status();
    }

    fn submit_challenge(&mut self) {
        let mut code = self.ui.get_verification_code().to_string();
        code.retain(|c| !c.is_whitespace());
        if !credential_valid(&code) {
            self.notice = Some("Enter the verification code.".into());
            self.refresh_status();
            return;
        }
        self.challenge = None;
        self.ui.set_verification_open(false);
        self.ui.set_verification_code(SharedString::from(""));
        self.notice = None;
        let _ = self.session.respond(&code);
        self.refresh_status();
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
                    if self.challenge.is_none() {
                        self.ui
                            .set_challenge_message(SharedString::from(msg.as_str()));
                        self.challenge = Some(msg);
                        self.ui.set_verification_code(SharedString::from(""));
                        self.ui.set_verification_open(true);
                    }
                }
                ConnEvent::Log(line) => {
                    let line = strip_ansi(&line);
                    if !line.trim().is_empty() {
                        self.logs.push_back(line);
                        while self.logs.len() > MAX_LOG_LINES {
                            self.logs.pop_front();
                        }
                    }
                }
            }
        }
        self.refresh_status();
        self.refresh_log();
    }

    fn handle_tray(&mut self) {
        let Some(tray) = &self.tray else { return };
        let dark = matches!(self.config.settings.theme, Theme::Dark);
        tray.set_state(tray::TrayState::from_conn(&self.state), dark);
        for action in tray.take_actions() {
            match action {
                tray::TrayAction::ShowWindow => {
                    self.ui.show().unwrap_or_default();
                    self.hidden_to_tray = false;
                }
                tray::TrayAction::Quit => {
                    self.quitting = true;
                    slint::quit_event_loop().unwrap_or_default();
                }
                tray::TrayAction::Connect => {
                    let busy = matches!(
                        self.state,
                        ConnState::Authenticating | ConnState::Connecting | ConnState::Connected(_)
                    );
                    if !busy {
                        self.start_connect();
                    }
                    self.ui.show().unwrap_or_default();
                    self.hidden_to_tray = false;
                }
                tray::TrayAction::Disconnect => {
                    self.session.disconnect();
                }
            }
        }
    }

    fn open_editor(&mut self, idx: Option<usize>) {
        let (name, host, username, saml, gpclient, reuse) =
            match idx.and_then(|i| self.config.portals.get(i)) {
                Some(p) => (
                    p.name.clone(),
                    p.portal.clone(),
                    p.username.clone(),
                    p.auth == AuthMode::Saml,
                    p.backend == Backend::Gpclient,
                    p.reuse_credentials,
                ),
                None => (
                    String::new(),
                    String::new(),
                    String::new(),
                    false,
                    false,
                    true,
                ),
            };
        self.editing = idx;
        self.ui.set_editor_name(SharedString::from(name.as_str()));
        self.ui.set_editor_host(SharedString::from(host.as_str()));
        self.ui
            .set_editor_username(SharedString::from(username.as_str()));
        self.ui.set_editor_saml(saml);
        self.ui.set_editor_gpclient(gpclient);
        self.ui.set_editor_reuse(reuse);
        self.ui.set_editor_error(SharedString::from(""));
        self.ui.set_portal_editor_open(true);
    }

    fn save_editor(&mut self) {
        let name = self.ui.get_editor_name().to_string();
        let host = self.ui.get_editor_host().to_string();
        let username = self.ui.get_editor_username().to_string();
        if !portal_valid(&name, &host, &username) {
            self.ui.set_editor_error(SharedString::from(
                "Portal hostname and username are required.",
            ));
            return;
        }
        let portal = PortalConfig {
            name: if name.trim().is_empty() {
                host.trim().to_string()
            } else {
                name.trim().to_string()
            },
            portal: host.trim().to_string(),
            username: username.trim().to_string(),
            auth: if self.ui.get_editor_saml() {
                AuthMode::Saml
            } else {
                AuthMode::Password
            },
            backend: if self.ui.get_editor_gpclient() {
                Backend::Gpclient
            } else {
                Backend::Openconnect
            },
            reuse_credentials: self.ui.get_editor_reuse(),
        };
        match self.editing.take() {
            Some(idx) if idx < self.config.portals.len() => {
                let old = &self.config.portals[idx];
                if old.portal != portal.portal || old.username != portal.username {
                    let _ = delete_password(&old.portal, &old.username);
                }
                self.config.portals[idx] = portal;
                self.selected = Some(idx);
            }
            _ => {
                self.config.portals.push(portal);
                self.selected = Some(self.config.portals.len() - 1);
            }
        }
        self.ui.set_portal_editor_open(false);
        self.save_config();
        self.refresh_portals();
        self.refresh_status();
    }

    fn remove_portal(&mut self, idx: usize) {
        if idx >= self.config.portals.len() {
            return;
        }
        let old = self.config.portals.remove(idx);
        let _ = delete_password(&old.portal, &old.username);
        self.selected = match self.selected {
            Some(s) if s == idx => None,
            Some(s) if s > idx => Some(s - 1),
            other => other,
        };
        self.save_config();
        self.refresh_portals();
        self.refresh_status();
    }

    fn save_settings(&mut self) {
        let s = &mut self.config.settings;
        s.theme = match self.ui.get_theme() {
            1 => Theme::Light,
            2 => Theme::Dark,
            _ => Theme::System,
        };
        s.reconnect_on_drop = self.ui.get_reconnect();
        s.autostart = self.ui.get_launch_at_login();
        s.connect_at_startup = self.ui.get_connect_at_login();
        s.close_to_tray = self.ui.get_close_to_tray();
        s.openconnect_bin = self.ui.get_openconnect_bin().to_string();
        s.gpclient_bin = self.ui.get_gpclient_bin().to_string();
        let autostart = s.autostart;
        self.ui.set_settings_open(false);
        self.save_config();
        self.apply_theme();
        if let Err(e) = autostart::apply(autostart) {
            self.notice = Some(e);
        }
        self.refresh_status();
    }
}

fn main() -> Result<(), slint::PlatformError> {
    std::env::set_var("SLINT_BACKEND", "winit-skia-software");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let _instance = single_instance::claim();
    #[cfg(unix)]
    let _helper = privilege::start_helper();

    let ui = AppWindow::new()?;

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<ConnEvent>();
    let session = match Session::spawn(event_tx) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to start VPN session worker: {e}");
            std::process::exit(1);
        }
    };
    let controller = Rc::new(RefCell::new(Controller::new(
        ui.clone_strong(),
        AppConfig::load(),
        session,
        event_rx,
    )));
    let pump = controller.clone();
    let timer = Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            let mut c = pump.borrow_mut();
            c.drain_events();
            c.handle_tray();
            if c.hidden_to_tray && !c.quitting {
                c.ui.hide().unwrap_or_default();
            }
        },
    );

    let c = controller.clone();
    ui.on_connect(move || {
        c.borrow_mut().start_connect();
    });
    let c = controller.clone();
    ui.on_disconnect(move || {
        c.borrow().session.disconnect();
    });
    let c = controller.clone();
    ui.on_select_portal(move |idx| {
        let mut c = c.borrow_mut();
        if idx >= 0 && (idx as usize) < c.config.portals.len() {
            c.selected = Some(idx as usize);
            c.refresh_portals();
            c.refresh_status();
        }
    });
    let c = controller.clone();
    ui.on_add_portal(move || c.borrow_mut().open_editor(None));
    let c = controller.clone();
    ui.on_edit_portal(move |idx| c.borrow_mut().open_editor(Some(idx as usize)));
    let c = controller.clone();
    ui.on_remove_portal(move |idx| c.borrow_mut().remove_portal(idx as usize));
    let c = controller.clone();
    ui.on_portal_save(move || c.borrow_mut().save_editor());
    let c = controller.clone();
    ui.on_portal_cancel(move || {
        let mut c = c.borrow_mut();
        c.editing = None;
        c.ui.set_portal_editor_open(false);
    });
    let c = controller.clone();
    ui.on_password_submit(move || c.borrow_mut().submit_password());
    let c = controller.clone();
    ui.on_password_cancel(move || c.borrow_mut().cancel_password());
    let c = controller.clone();
    ui.on_verification_submit(move || c.borrow_mut().submit_challenge());
    let c = controller.clone();
    ui.on_verification_cancel(move || {
        let mut c = c.borrow_mut();
        c.challenge = None;
        c.ui.set_verification_open(false);
        c.refresh_prompt_state();
    });
    let c = controller.clone();
    ui.on_copy_log(move || {
        let text = c.borrow().ui.get_log_text();
        let text = bridge::copy_log_text(&text).to_string();
        if let Ok(mut clipboard) = copypasta::ClipboardContext::new() {
            let _ = clipboard.set_contents(text);
        }
    });
    let c = controller.clone();
    ui.on_save_settings(move || c.borrow_mut().save_settings());
    let c = controller.clone();
    ui.on_close_settings(move || {
        let c = c.borrow();
        c.ui.set_settings_open(false);
        c.refresh_settings();
    });
    let c = controller.clone();
    ui.on_show_window(move || {
        let c = c.borrow();
        c.ui.show().unwrap_or_default();
    });
    let c = controller.clone();
    ui.on_quit(move || {
        let mut c = c.borrow_mut();
        c.quitting = true;
        slint::quit_event_loop().unwrap_or_default();
    });

    let result = ui.run();
    controller.borrow().session.disconnect();
    result
}
