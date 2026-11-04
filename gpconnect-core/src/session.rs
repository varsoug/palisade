use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::PortalConfig;

pub const DEFAULT_OPENCONNECT_BIN: &str = "openconnect";

/// Handle used to answer prompts (OTP challenge) by writing to openconnect's
/// input. On Unix this is the PTY master; on Windows the piped stdin.
#[cfg(unix)]
type InputHandle = std::fs::File;
#[cfg(windows)]
type InputHandle = tokio::process::ChildStdin;

type InputSlot = Arc<Mutex<Option<InputHandle>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Authenticating,
    Connecting,
    Connected(TunnelInfo),
    Error(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TunnelInfo {
    pub gateway: String,
    pub ip: String,
}

#[derive(Debug)]
pub enum ConnEvent {
    State(ConnState),
    /// One line of OpenConnect output (already trimmed), newest last.
    Log(String),
    /// Connection stage in the official client's wording ("Retrieving
    /// configuration…", "Authenticating…"), emitted while not connected.
    Stage(String),
    /// The portal asks for a one-time code; the GUI should prompt and call
    /// `Session::respond`. Carries the portal's instruction text.
    Challenge(String),
}

enum Job {
    Connect { portal: PortalConfig, password: Option<String>, openconnect_bin: PathBuf },
    /// Write one response line (OTP challenge) to openconnect's input.
    Respond(String),
    /// Abort the running connection, if any.
    Stop,
}

/// A VPN session driving an OpenConnect child process.
///
/// `spawn` creates a background thread with its own tokio runtime so the GUI
/// does not need to be async itself.
pub struct Session {
    cmd_tx: Option<mpsc::UnboundedSender<Job>>,
    worker: Option<std::thread::JoinHandle<()>>,
    state: Arc<Mutex<ConnState>>,
}

impl Session {
    pub fn spawn(event_tx: mpsc::UnboundedSender<ConnEvent>) -> io::Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Job>();
        let state = Arc::new(Mutex::new(ConnState::Disconnected));
        let state2 = Arc::clone(&state);
        let input_slot: InputSlot = Arc::new(Mutex::new(None));
        let input_slot2 = Arc::clone(&input_slot);

        let worker = std::thread::Builder::new()
            .name("gpconnect-session".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = event_tx.send(ConnEvent::State(ConnState::Error(format!(
                            "tokio runtime: {e}"
                        ))));
                        return;
                    }
                };
                rt.block_on(worker_loop(cmd_rx, event_tx, state2, input_slot2));
            })?;

        Ok(Session { cmd_tx: Some(cmd_tx), worker: Some(worker), state })
    }

    pub fn state(&self) -> ConnState {
        self.state.lock().unwrap().clone()
    }

    pub fn connect(
        &self,
        portal: PortalConfig,
        password: Option<String>,
        openconnect_bin: Option<PathBuf>,
    ) -> Result<(), String> {
        let openconnect_bin = openconnect_bin.unwrap_or_else(|| DEFAULT_OPENCONNECT_BIN.into());
        *self.state.lock().unwrap() = ConnState::Authenticating;
        self.cmd_tx
            .as_ref()
            .ok_or_else(|| "session worker is gone".to_string())?
            .send(Job::Connect { portal, password, openconnect_bin })
            .map_err(|e| format!("session worker is gone: {e}"))
    }

    /// Write one response line to openconnect's input (OTP challenge reply).
    pub fn respond(&self, text: &str) -> Result<(), String> {
        self.cmd_tx
            .as_ref()
            .ok_or_else(|| "session worker is gone".to_string())?
            .send(Job::Respond(text.to_string()))
            .map_err(|e| format!("session worker is gone: {e}"))
    }

    pub fn disconnect(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Job::Stop);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Drop the command channel FIRST: the worker only exits when all
        // senders are gone, and joining while `cmd_tx` is still alive would
        // deadlock (the worker would park on recv forever).
        drop(self.cmd_tx.take());
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(unix)]
fn write_input(slot: &InputSlot, text: &str) {
    use std::io::Write;
    let mut guard = slot.lock().unwrap();
    if let Some(input) = guard.as_mut() {
        let r = input.write_all(text.as_bytes());
        let r2 = input.write_all(b"\n");
        let r3 = input.flush();
        tracing::info!("respond write: {r:?} {r2:?} {r3:?}");
    } else {
        tracing::info!("respond: no input handle!");
    }
}

#[cfg(windows)]
async fn write_input_async(slot: &InputSlot, text: &str) {
    use tokio::io::AsyncWriteExt;
    let mut taken = slot.lock().unwrap().take();
    if let Some(input) = taken.as_mut() {
        let r = input.write_all(text.as_bytes()).await;
        let r2 = input.write_all(b"\n").await;
        let r3 = input.flush().await;
        tracing::info!("respond write: {r:?} {r2:?} {r3:?}");
    } else {
        tracing::info!("respond: no input handle!");
    }
    *slot.lock().unwrap() = taken;
}

async fn worker_loop(
    mut cmd_rx: mpsc::UnboundedReceiver<Job>,
    events: mpsc::UnboundedSender<ConnEvent>,
    state: Arc<Mutex<ConnState>>,
    input_slot: InputSlot,
) {
    let mut pump: Option<JoinHandle<()>> = None;

    while let Some(job) = cmd_rx.recv().await {
        // Abort the previous connection task. The Child inside it has
        // kill_on_drop set, so aborting kills openconnect.
        if let Some(p) = pump.take() {
            p.abort();
        }

        match job {
            Job::Respond(text) => {
                #[cfg(unix)]
                write_input(&input_slot, &text);
                #[cfg(windows)]
                write_input_async(&input_slot, &text).await;
            }
            Job::Stop => {
                *state.lock().unwrap() = ConnState::Disconnected;
                let _ = events.send(ConnEvent::State(ConnState::Disconnected));
            }
            Job::Connect { portal, password, openconnect_bin } => {
                *state.lock().unwrap() = ConnState::Connecting;
                let _ = events.send(ConnEvent::State(ConnState::Connecting));
                let spawned = spawn_openconnect(&portal, password.as_deref(), &openconnect_bin);
                let (child, input) = match spawned {
                    Ok(v) => v,
                    Err(e) => {
                        let msg = if e.kind() == io::ErrorKind::NotFound {
                            format!(
                                "openconnect binary not found ({}) — install openconnect \
                                 or set its path in Settings",
                                openconnect_bin.display()
                            )
                        } else {
                            format!("failed to start openconnect: {e}")
                        };
                        *state.lock().unwrap() = ConnState::Error(msg.clone());
                        let _ = events.send(ConnEvent::State(ConnState::Error(msg)));
                        continue;
                    }
                };
                *input_slot.lock().unwrap() = input;
                let state2 = Arc::clone(&state);
                let events2 = events.clone();
                let input_slot2 = Arc::clone(&input_slot);
                pump = Some(tokio::spawn(async move {
                    let _ = pump_output(
                        child,
                        portal,
                        events2,
                        state2,
                        password,
                        openconnect_bin,
                        input_slot2,
                    )
                    .await;
                }));
            }
        }
    }
}

/// Build the openconnect command line (shared between platforms).
fn build_command(portal: &PortalConfig, bin: &PathBuf) -> io::Result<Command> {
    let mut cmd = Command::new(bin);
    cmd.arg("--protocol=gp")
        // Portals that reject unknown client OSes (this corp portal answers
        // getconfig.esp with HTTP 512 for clientos=Linux) accept Windows;
        // this mirrors the gpclient invocation that worked against it.
        .arg("--os=win")
        .arg("--user")
        .arg(&portal.username)
        // Dropping the pump task (abort/replace) must kill openconnect.
        .kill_on_drop(true);

    if portal.auth == crate::AuthMode::Password && portal.username.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "username required"));
    }

    // HIP (host posture) report: if the package ships hipreport.sh next to
    // the binary, submit it — portals that demand HIP fail auth without it.
    let hip = bin
        .parent()
        .and_then(|d| d.parent().map(|p| p.join("libexec/openconnect/hipreport.sh")))
        .filter(|p| p.exists());
    if let Some(hip) = hip {
        cmd.arg(format!("--csd-wrapper={}", hip.display()));
    }

    match portal.auth {
        crate::AuthMode::Saml => {
            // OpenConnect runs a localhost redirect listener and asks this
            // command to open the SSO URL in the user's browser.
            cmd.arg(format!("--external-browser={}", browser_opener()));
        }
        crate::AuthMode::Password => {}
    }

    cmd.arg(&portal.portal);
    Ok(cmd)
}

#[cfg(unix)]
fn spawn_openconnect(
    portal: &PortalConfig,
    _password: Option<&str>,
    bin: &PathBuf,
) -> io::Result<(Child, Option<InputHandle>)> {
    use nix::pty::openpty;
    use std::os::unix::io::OwnedFd;

    let mut cmd = build_command(portal, bin)?;

    // OpenConnect answers follow-up prompts (OTP challenge) only when its
    // stdin is a TTY, so run it on a PTY and service prompts ourselves via
    // the PTY master. Both the password and challenge replies are written
    // there.
    let winsize = nix::pty::Winsize {
        ws_row: 24,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ends = openpty(Some(&winsize), None)?;
    let master: OwnedFd = ends.master;
    let slave: OwnedFd = ends.slave;

    let slave_file = std::fs::File::from(slave);
    use std::process::Stdio;
    cmd.stdin(Stdio::from(slave_file.try_clone()?))
        .stdout(Stdio::from(slave_file.try_clone()?))
        .stderr(Stdio::from(slave_file));
    // No --passwd-on-stdin / --non-inter here: prompts are answered over the
    // PTY like a human would type them.
    let child = cmd.spawn()?;
    Ok((child, Some(std::fs::File::from(master))))
}

#[cfg(windows)]
fn spawn_openconnect(
    portal: &PortalConfig,
    password: Option<&str>,
    bin: &PathBuf,
) -> io::Result<(Child, Option<InputHandle>)> {
    let mut cmd = build_command(portal, bin)?;
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if portal.auth == crate::AuthMode::Password {
        if password.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "password auth requires a password",
            ));
        }
        cmd.arg("--passwd-on-stdin").arg("--non-inter");
    } else {
        cmd.arg("--non-inter");
    }
    let mut child = cmd.spawn()?;
    Ok((child, child.stdin.take()))
}

#[cfg(target_os = "windows")]
fn browser_opener() -> &'static str {
    "cmd /c start"
}

#[cfg(target_os = "macos")]
fn browser_opener() -> &'static str {
    "open"
}

#[cfg(all(unix, not(target_os = "macos")))]
fn browser_opener() -> &'static str {
    "xdg-open"
}

struct Progress {
    connected: bool,
    info: TunnelInfo,
    stage: String,
    last_line: String,
    challenge_sent: bool,
}

/// Translate one openconnect output line into state/stage/challenge events.
fn process_line(
    line: &str,
    events: &mpsc::UnboundedSender<ConnEvent>,
    progress: &Arc<Mutex<Progress>>,
    portal: &str,
) {
    let mut p = progress.lock().unwrap();
    if p.connected {
        p.last_line = line.to_string();
        return;
    }
    if line.contains("Connected as") {
        p.connected = true;
        p.info.ip = line
            .split("Connected as")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        p.info.gateway = portal.to_string();
        let _ = events.send(ConnEvent::State(ConnState::Connected(p.info.clone())));
    } else {
        if is_auth_failure(line) {
            let _ = events.send(ConnEvent::State(ConnState::Error(
                "authentication failed".into(),
            )));
        }
        if let Some(stage) = stage_for_line(line, &p.stage) {
            p.stage = stage.into();
            let _ = events.send(ConnEvent::Stage(stage.into()));
        }
        if !p.challenge_sent && is_challenge_prompt(line) {
            p.challenge_sent = true;
            let msg = server_message(&p.last_line);
            let _ = events.send(ConnEvent::Challenge(msg));
        }
    }
    p.last_line = line.to_string();
}

/// Map an openconnect output line to a stage label in the official client's
/// wording. Returns None when nothing new to report.
fn stage_for_line(line: &str, current: &str) -> Option<&'static str> {
    let l = line.to_ascii_lowercase();
    let stage = if l.contains("prelogin") || l.contains("saml") || is_auth_failure(&l) {
        "Authenticating…"
    } else if l.contains("getconfig") || l.contains("retrieving configuration") || l.contains("config") {
        "Retrieving configuration…"
    } else if l.contains("gateway") || l.contains("ssl-tunnel-connect") {
        "Connecting…"
    } else {
        return None;
    };
    if stage == current { None } else { Some(stage) }
}

/// Does this openconnect line prompt for an OTP/challenge response?
fn is_challenge_prompt(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    ["challenge", "one-time", "verification code", "otp", "одноразов"]
        .iter()
        .any(|p| l.contains(p))
}

/// The portal's human instruction usually precedes the "Challenge:" line.
fn server_message(last_line: &str) -> String {
    let l = last_line.to_ascii_lowercase();
    let looks_internal = [
        "post ", "get ", "connected", "ssl", "http", "prelogin", "getconfig",
        "challenge", "password", "user", "openconnect", "tunnel", "gateway",
    ]
    .iter()
    .any(|p| l.contains(p));
    if last_line.trim().is_empty() || looks_internal {
        "Enter the one-time code".into()
    } else {
        last_line.trim().to_string()
    }
}

fn is_auth_failure(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    ["authentication failure", "login failed", "bad credentials", "401", "authfailed"]
        .iter()
        .any(|p| l.contains(p))
}

/// Shared tail: wait for the child, then either reconnect or finalize state.
#[allow(clippy::too_many_arguments)]
async fn finish_child(
    mut child: Child,
    progress: Arc<Mutex<Progress>>,
    portal: PortalConfig,
    events: mpsc::UnboundedSender<ConnEvent>,
    state: Arc<Mutex<ConnState>>,
    password: Option<String>,
    bin: PathBuf,
    input_slot: InputSlot,
) {
    let status = child.wait().await.ok();
    let connected = progress.lock().unwrap().connected;
    *input_slot.lock().unwrap() = None;
    tracing::info!("openconnect exited: {status:?}, connected={connected}");

    if connected {
        // Unexpected drop (not killed by us): maybe reconnect.
        let want_reconnect = crate::AppConfig::load().settings.reconnect_on_drop;
        if want_reconnect {
            let _ = events.send(ConnEvent::State(ConnState::Connecting));
            *state.lock().unwrap() = ConnState::Connecting;
            tokio::time::sleep(Duration::from_secs(3)).await;
            if let Ok((new_child, input)) = spawn_openconnect(&portal, password.as_deref(), &bin)
            {
                *input_slot.lock().unwrap() = input;
                Box::pin(pump_output(new_child, portal, events, state, password, bin, input_slot))
                    .await;
                return;
            }
        }
        *state.lock().unwrap() = ConnState::Disconnected;
        let _ = events.send(ConnEvent::State(ConnState::Disconnected));
    } else {
        let msg = format!(
            "openconnect exited{}",
            status.map(|s| format!(" with {s}")).unwrap_or_default()
        );
        *state.lock().unwrap() = ConnState::Error(msg.clone());
        let _ = events.send(ConnEvent::State(ConnState::Error(msg)));
    }
}

#[cfg(unix)]
async fn pump_output(
    child: Child,
    portal: PortalConfig,
    events: mpsc::UnboundedSender<ConnEvent>,
    state: Arc<Mutex<ConnState>>,
    password: Option<String>,
    bin: PathBuf,
    input_slot: InputSlot,
) {
    // The input handle (PTY master) sits in input_slot; take a writer clone.
    let writer = {
        let guard = input_slot.lock().unwrap();
        guard.as_ref().and_then(|f| f.try_clone().ok())
    };
    let Some(writer) = writer else {
        let _ = events.send(ConnEvent::State(ConnState::Error(
            "no PTY master handle".into(),
        )));
        return;
    };

    // Feed the password through the PTY like a typed line.
    if portal.auth == crate::AuthMode::Password {
        if let Some(pw) = &password {
            let mut w = writer;
            let pw = pw.clone();
            let _ = tokio::task::spawn_blocking(move || {
                use std::io::Write;
                let r = w.write_all(pw.as_bytes());
                let r2 = w.write_all(b"\n");
                let r3 = w.flush();
                tracing::info!("password write: {r:?} {r2:?} {r3:?}");
            })
            .await;
        }
    }

    let progress = Arc::new(Mutex::new(Progress {
        connected: false,
        info: TunnelInfo::default(),
        stage: String::new(),
        last_line: String::new(),
        challenge_sent: false,
    }));

    // Reader: blocking loop on the PTY master; unbounded mpsc::Sender is
    // usable from any thread, so parse lines right here.
    let reader_handle = {
        let events = events.clone();
        let progress = Arc::clone(&progress);
        let portal_name = portal.portal.clone();
        let master = input_slot.lock().unwrap().as_ref().and_then(|f| f.try_clone().ok());
        tokio::task::spawn_blocking(move || {
            let Some(master) = master else { return };
            use std::io::{BufRead, BufReader};
            let mut reader = BufReader::new(master);
            loop {
                let mut raw = String::new();
                match reader.read_line(&mut raw) {
                    Ok(0) => break,          // EOF
                    Ok(_) => {
                        let line = raw.trim_end_matches(['\n', '\r']).to_string();
                        if line.is_empty() {
                            continue;
                        }
                        tracing::info!("openconnect: {line}");
                        let _ = events.send(ConnEvent::Log(line.clone()));
                        process_line(&line, &events, &progress, &portal_name);
                    }
                    Err(_) => break,         // EIO once the child is gone
                }
            }
        })
    };

    let _ = reader_handle.await;
    finish_child(child, progress, portal, events, state, password, bin, input_slot).await;
}

#[cfg(windows)]
async fn pump_output(
    mut child: Child,
    portal: PortalConfig,
    events: mpsc::UnboundedSender<ConnEvent>,
    state: Arc<Mutex<ConnState>>,
    password: Option<String>,
    bin: PathBuf,
    input_slot: InputSlot,
) {
    use tokio::io::AsyncWriteExt;

    // Password via --passwd-on-stdin, then keep stdin open.
    if portal.auth == crate::AuthMode::Password {
        if let Some(mut input) = child.stdin.take() {
            if let Some(pw) = &password {
                let _ = input.write_all(pw.as_bytes()).await;
                let _ = input.write_all(b"\n").await;
                let _ = input.flush().await;
            }
            *input_slot.lock().unwrap() = Some(input);
        }
    }

    let progress = Arc::new(Mutex::new(Progress {
        connected: false,
        info: TunnelInfo::default(),
        stage: String::new(),
        last_line: String::new(),
        challenge_sent: false,
    }));

    use tokio::io::AsyncBufReadExt;
    let stderr = child.stderr.take();
    let stdout = child.stdout.take();
    let mut handles = Vec::new();
    for s in [stderr, stdout].into_iter().flatten() {
        let (events, progress) = (events.clone(), Arc::clone(&progress));
        let portal_name = portal.portal.clone();
        handles.push(tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!("openconnect: {line}");
                let _ = events.send(ConnEvent::Log(line.clone()));
                process_line(&line, &events, &progress, &portal_name);
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    finish_child(child, progress, portal, events, state, password, bin, input_slot).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_failure_detection() {
        assert!(is_auth_failure("Authentication failure; please try again"));
        assert!(is_auth_failure("Login failed"));
        assert!(is_auth_failure("HTTP 401"));
        assert!(!is_auth_failure("Connected as 10.0.0.1, using SSL"));
    }

    #[test]
    fn challenge_detection() {
        assert!(is_challenge_prompt("Challenge: "));
        assert!(is_challenge_prompt("Введите одноразовый пароль"));
        assert!(!is_challenge_prompt("Connected as 10.0.0.1"));
        assert_eq!(server_message("Введите одноразовый пароль из приложения."), "Введите одноразовый пароль из приложения.");
        assert_eq!(server_message("POST https://x/prelogin.esp"), "Enter the one-time code");
    }

    #[test]
    fn session_drop_does_not_deadlock() {
        // Dropping a Session closes the command channel and joins the worker;
        // it must terminate promptly. A regression here hangs this test.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            drop(Session::spawn(event_tx).unwrap());
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("Session::drop deadlocked");
    }
}
