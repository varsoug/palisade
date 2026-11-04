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
    Connect {
        portal: PortalConfig,
        password: Option<String>,
        openconnect_bin: PathBuf,
    },
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
            .name("palisade-session".into())
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

        Ok(Session {
            cmd_tx: Some(cmd_tx),
            worker: Some(worker),
            state,
        })
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
            .send(Job::Connect {
                portal,
                password,
                openconnect_bin,
            })
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
        // Abort the tunnel and wait for the worker to finish tearing it down
        // before the process goes away: a killed GUI would otherwise leave a
        // root gpclient holding the tun device.
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(Job::Stop);
        }
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
    // Backend and binary path of the running session, needed to tear it down
    // the way that backend expects.
    let mut active: Option<PortalConfig> = None;
    let mut openconnect_bin = PathBuf::new();
    // Leftover sessions only need clearing once per process, before the first
    // connect; doing it on every reconnect would kill our own tunnel.
    let mut stale_cleared = false;

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
                // Aborting the pump kills the child, but a root gpclient
                // catches the signal and leaves tun0 and the routes behind.
                // Ask it to tear the session down properly first.
                if let Some(portal) = active.take() {
                    disconnect_cleanup(&portal, &openconnect_bin).await;
                }
                *state.lock().unwrap() = ConnState::Disconnected;
                let _ = events.send(ConnEvent::State(ConnState::Disconnected));
            }
            Job::Connect {
                portal,
                password,
                openconnect_bin: bin,
            } => {
                *state.lock().unwrap() = ConnState::Connecting;
                let _ = events.send(ConnEvent::State(ConnState::Connecting));
                if !stale_cleared && portal.backend == crate::Backend::Gpclient {
                    stale_cleared = true;
                    clear_stale_session(&bin).await;
                }
                openconnect_bin = bin.clone();
                active = Some(portal.clone());
                let spawned = spawn_openconnect(&portal, password.as_deref(), &bin);
                let (child, input) = match spawned {
                    Ok(v) => v,
                    Err(e) => {
                        let bin_name = match portal.backend {
                            crate::Backend::Openconnect => "openconnect",
                            crate::Backend::Gpclient => "gpclient",
                        };
                        let msg = if e.kind() == io::ErrorKind::NotFound {
                            format!(
                                "{bin_name} binary not found ({}) — install {bin_name} \
                                 or set its path in Settings",
                                bin.display()
                            )
                        } else {
                            format!("failed to start {bin_name}: {e}")
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
                    let _ = pump_output(child, portal, events2, state2, password, bin, input_slot2)
                        .await;
                }));
            }
        }
    }
}

/// Tear down what killing the child leaves behind.
///
/// gpclient keeps a root-owned `gpclient connect` process that owns the tun
/// device; SIGTERM is caught and the interface plus routes survive. Its own
/// `disconnect` subcommand signals that process and cleans up, so run it
/// through the same wrapper (which supplies the sudo).
async fn disconnect_cleanup(portal: &PortalConfig, bin: &PathBuf) {
    if portal.backend != crate::Backend::Gpclient {
        return;
    }
    let mut cmd = Command::new(bin);
    cmd.arg("disconnect").kill_on_drop(true);
    tracing::info!("running {} disconnect", bin.display());
    // A wrapper that asks for a sudo password would block forever, and this
    // runs on the worker thread that services every other command.
    let output = tokio::time::timeout(Duration::from_secs(15), cmd.output()).await;
    let output = match output {
        Ok(r) => r,
        Err(_) => {
            tracing::info!("disconnect timed out after 15s; continuing");
            return;
        }
    };
    match output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let err = String::from_utf8_lossy(&out.stderr);
            tracing::info!(
                "disconnect finished: {:?} {}{}",
                out.status,
                text.trim(),
                err.trim()
            );
        }
        Err(e) => tracing::info!("disconnect failed to run: {e}"),
    }
}

/// Tear down a VPN session left behind by a previous run.
///
/// A killed GUI leaves `gpclient` running as root with the tunnel up, which
/// makes every later connect fail with "another instance". Called at startup
/// before the first connect uses `bin`.
pub async fn clear_stale_session(bin: &std::path::Path) {
    let mut cmd = Command::new(bin);
    cmd.arg("disconnect").kill_on_drop(true);
    // Same reason as `disconnect_cleanup`: never block the worker on a
    // wrapper that may want a password.
    let output = match tokio::time::timeout(Duration::from_secs(15), cmd.output()).await {
        Ok(r) => r,
        Err(_) => {
            tracing::info!("startup cleanup timed out after 15s; continuing");
            return;
        }
    };
    match output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.contains("Found process") || text.contains("No VPN connection") {
                tracing::info!("startup cleanup: {}", text.trim());
            }
        }
        Err(e) => tracing::info!("startup cleanup skipped: {e}"),
    }
}

/// Build the openconnect/gpclient command line, portal argument included.
fn build_command(portal: &PortalConfig, bin: &PathBuf) -> io::Result<Command> {
    if portal.auth == crate::AuthMode::Password && portal.username.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "username required",
        ));
    }

    let mut cmd = Command::new(bin);

    match portal.backend {
        crate::Backend::Openconnect => {
            cmd.arg("--protocol=gp")
                // Portals that reject unknown client OSes (this corp portal
                // answers getconfig.esp with HTTP 512 for clientos=Linux)
                // accept Windows; mirrors the gpclient invocation that
                // worked against it.
                .arg("--os=win")
                .arg("--user")
                .arg(&portal.username);

            // HIP (host posture) report: if the package ships hipreport.sh
            // next to the binary, submit it — portals that demand HIP fail
            // auth without it.
            let hip = bin
                .parent()
                .and_then(|d| {
                    d.parent()
                        .map(|p| p.join("libexec/openconnect/hipreport.sh"))
                })
                .filter(|p| p.exists());
            if let Some(hip) = hip {
                cmd.arg(format!("--csd-wrapper={}", hip.display()));
            }
            if portal.auth == crate::AuthMode::Saml {
                // OpenConnect runs a localhost redirect listener and asks this
                // command to open the SSO URL in the user's browser.
                cmd.arg(format!("--external-browser={}", browser_opener()));
            }
            cmd.arg(&portal.portal);
        }
        crate::Backend::Gpclient => {
            // `gpclient connect <portal> [options]`. It performs the portal
            // login itself over HTTP and submits the HIP report itself. The
            // password is never an argv element — it is typed over the PTY
            // like the login prompts.
            //
            // `--os` takes the full name here (Linux|Windows|Mac), unlike
            // openconnect's `--os=win`.
            cmd.arg("connect")
                .arg(&portal.portal)
                .arg("--os=Windows")
                .arg("--user")
                .arg(&portal.username)
                .arg("--hip");
            if portal.auth == crate::AuthMode::Saml {
                // Without this gpclient opens its own embedded webview, which
                // a CLI-spawned process cannot show or drive. `--browser`
                // makes it hand the SSO URL to the desktop browser instead.
                cmd.arg("--browser");
            }
        }
    }

    // Dropping the pump task (abort/replace) must kill the child.
    cmd.kill_on_drop(true);
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

    // Disable terminal echo BEFORE the child starts: otherwise our password
    // write races openconnect's own echo-off and the secret is echoed into
    // the (logged) output stream.
    let mut term = nix::sys::termios::tcgetattr(&slave)?;
    term.local_flags.remove(nix::sys::termios::LocalFlags::ECHO);
    nix::sys::termios::tcsetattr(&slave, nix::sys::termios::SetArg::TCSANOW, &term)?;

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
    /// The attempt found a tunnel started by an earlier run instead of
    /// establishing its own. Not a connect: it must not trigger reconnect.
    stale_session: bool,
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
    if let Some(ip) = connected_ip(line) {
        p.connected = true;
        p.info.ip = ip;
        p.info.gateway = portal.to_string();
        let _ = events.send(ConnEvent::State(ConnState::Connected(p.info.clone())));
    } else {
        if is_auth_failure(line) {
            let _ = events.send(ConnEvent::State(ConnState::Error(
                "authentication failed".into(),
            )));
        }
        // A previous session outlived the app (crash, kill, logout). The
        // tunnel is already up, so telling the user "error" would be wrong and
        // would hide a working VPN.
        // A previous session outlived the app (crash, kill, logout) and holds
        // the lock. Report that the tunnel exists, but mark it NOT as a
        // successful connect: `finish_child` treats `connected` as "died
        // unexpectedly, reconnect", which would respawn in a tight loop.
        if is_already_running(line) {
            p.stale_session = true;
            let _ = events.send(ConnEvent::State(ConnState::Connected(TunnelInfo {
                gateway: portal.to_string(),
                ip: String::new(),
            })));
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

/// Does this line announce that the server wants the login again?
///
/// OpenConnect prints "Enter login credentials" whenever it starts a fresh
/// authentication dialog — including after a retry, when the password field is
/// asked for again rather than a one-time code.
fn is_login_prompt(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("enter login credentials") || l.contains("enter your username")
}

/// Recognise a successful tunnel from either backend's wording.
///
/// openconnect: `Connected as 10.1.2.3, using SSL`
/// gpclient:    `Connected to VPN, pipe_fd: 14` (no address on that line, so
///              the tunnel IP is left empty rather than making the state wait)
fn connected_ip(line: &str) -> Option<String> {
    if let Some(rest) = line.split("Connected as").nth(1) {
        let ip = rest.split(',').next().unwrap_or("").trim().to_string();
        return Some(ip);
    }
    if line.contains("Connected to VPN") {
        return Some(String::new());
    }
    None
}

/// Map an openconnect output line to a stage label in the official client's
/// wording. Returns None when nothing new to report.
fn stage_for_line(line: &str, current: &str) -> Option<&'static str> {
    let l = line.to_ascii_lowercase();
    let stage = if l.contains("prelogin") || l.contains("saml") || is_auth_failure(&l) {
        "Authenticating…"
    } else if l.contains("getconfig")
        || l.contains("retrieving configuration")
        || l.contains("config")
    {
        "Retrieving configuration…"
    } else if l.contains("gateway") || l.contains("ssl-tunnel-connect") {
        "Connecting…"
    } else {
        return None;
    };
    if stage == current {
        None
    } else {
        Some(stage)
    }
}

/// Does this openconnect line prompt for an OTP/challenge response?
fn is_challenge_prompt(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    [
        "challenge",
        "one-time",
        "verification code",
        "otp",
        "одноразов",
    ]
    .iter()
    .any(|p| l.contains(p))
}

/// The portal's human instruction usually precedes the "Challenge:" line.
fn server_message(last_line: &str) -> String {
    let l = last_line.to_ascii_lowercase();
    let looks_internal = [
        "post ",
        "get ",
        "connected",
        "ssl",
        "http",
        "prelogin",
        "getconfig",
        "challenge",
        "password",
        "user",
        "openconnect",
        "tunnel",
        "gateway",
    ]
    .iter()
    .any(|p| l.contains(p));
    if last_line.trim().is_empty() || looks_internal {
        "Enter the one-time code".into()
    } else {
        last_line.trim().to_string()
    }
}

/// gpclient refuses to start while another session owns its lock file. That
/// means a tunnel from a previous run is still up, not that this attempt
/// failed.
fn is_already_running(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("another instance of the client is already running")
}

fn is_auth_failure(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    [
        "authentication failure",
        "login failed",
        "bad credentials",
        "401",
        "authfailed",
    ]
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
    let (connected, stale) = {
        let p = progress.lock().unwrap();
        (p.connected, p.stale_session)
    };
    *input_slot.lock().unwrap() = None;
    tracing::info!("openconnect exited: {status:?}, connected={connected}, stale={stale}");

    if stale {
        // "Another instance is already running": our child exits immediately
        // and the tunnel belongs to someone else. Reconnecting would only
        // reproduce the same exit forever, so leave the reported state alone.
        return;
    }

    if connected {
        // Unexpected drop (not killed by us): maybe reconnect.
        let want_reconnect = crate::AppConfig::load().settings.reconnect_on_drop;
        if want_reconnect {
            let _ = events.send(ConnEvent::State(ConnState::Connecting));
            *state.lock().unwrap() = ConnState::Connecting;
            tokio::time::sleep(Duration::from_secs(3)).await;
            if let Ok((new_child, input)) = spawn_openconnect(&portal, password.as_deref(), &bin) {
                *input_slot.lock().unwrap() = input;
                Box::pin(pump_output(
                    new_child, portal, events, state, password, bin, input_slot,
                ))
                .await;
                return;
            }
        }
        *state.lock().unwrap() = ConnState::Disconnected;
        let _ = events.send(ConnEvent::State(ConnState::Disconnected));
    } else {
        let bin_name = match portal.backend {
            crate::Backend::Openconnect => "openconnect",
            crate::Backend::Gpclient => "gpclient",
        };
        let msg = format!(
            "{bin_name} exited{}",
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
        stale_session: false,
        challenge_sent: false,
    }));

    // Reader: blocking loop on the PTY master; unbounded mpsc::Sender is
    // usable from any thread, so parse lines right here.
    let reader_handle = {
        let events = events.clone();
        let progress = Arc::clone(&progress);
        let portal_name = portal.portal.clone();
        let username = portal.username.clone();
        let password_auth = portal.auth == crate::AuthMode::Password;
        let reuse = portal.reuse_credentials && password_auth;
        // gpclient gets the username from `--user`, so its prompts ask only for
        // the password; openconnect asks for user first, then password.
        let prompt_wants_user = portal.backend == crate::Backend::Openconnect;
        let needs_interactive_password = portal.backend == crate::Backend::Gpclient;
        let password = password.clone();
        let master = input_slot
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|f| f.try_clone().ok());
        tokio::task::spawn_blocking(move || {
            let Some(master) = master else { return };
            use std::io::{BufRead, BufReader, Write};
            let mut writer = match master.try_clone() {
                Ok(w) => w,
                Err(e) => {
                    tracing::info!("pty reader: cannot clone for writes: {e}");
                    return;
                }
            };
            // Whether the password still has to be typed at a login prompt.
            // openconnect takes `--passwd-on-stdin`, so its password is written
            // before the first prompt and that prompt needs no answer. gpclient
            // asks for it interactively instead, so its FIRST prompt is the one
            // that needs the password — that is the normal connect, not a
            // retry, and must work with or without `reuse_credentials`.
            let mut password_pending = needs_interactive_password;
            let mut reader = BufReader::new(master);
            loop {
                let mut raw = String::new();
                match reader.read_line(&mut raw) {
                    Ok(0) => {
                        tracing::info!("pty reader: EOF");
                        break;
                    }
                    Ok(_) => {
                        let line = raw.trim_end_matches(['\n', '\r']).to_string();
                        if line.is_empty() {
                            continue;
                        }
                        tracing::info!("openconnect: {line}");
                        let _ = events.send(ConnEvent::Log(line.clone()));
                        process_line(&line, &events, &progress, &portal_name);

                        if is_login_prompt(&line) && password_auth && (password_pending || reuse) {
                            let first = password_pending;
                            password_pending = false;
                            if let Some(pw) = password.as_deref() {
                                tracing::info!(
                                    "answering login prompt ({}{})",
                                    if first { "first" } else { "repeat" },
                                    if prompt_wants_user {
                                        ", user+password"
                                    } else {
                                        ", password"
                                    }
                                );
                                let write =
                                    |w: &mut std::fs::File, s: &str| -> std::io::Result<()> {
                                        w.write_all(s.as_bytes())?;
                                        w.write_all(b"\n")?;
                                        w.flush()
                                    };
                                let wrote = if prompt_wants_user {
                                    write(&mut writer, &username)
                                        .and_then(|_| write(&mut writer, pw))
                                } else {
                                    write(&mut writer, pw)
                                };
                                if let Err(e) = wrote {
                                    tracing::info!("reuse write failed: {e}");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::info!("pty reader: {e}");
                        break;
                    }
                }
            }
        })
    };

    let _ = reader_handle.await;
    finish_child(
        child, progress, portal, events, state, password, bin, input_slot,
    )
    .await;
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
        stale_session: false,
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
    finish_child(
        child, progress, portal, events, state, password, bin, input_slot,
    )
    .await;
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
        assert_eq!(
            server_message("Введите одноразовый пароль из приложения."),
            "Введите одноразовый пароль из приложения."
        );
        assert_eq!(
            server_message("POST https://x/prelogin.esp"),
            "Enter the one-time code"
        );
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn portal(backend: crate::Backend, auth: crate::AuthMode) -> PortalConfig {
        PortalConfig {
            name: "ETSP".into(),
            portal: "vpn.etsp.ru".into(),
            username: "alice".into(),
            auth,
            backend,
            reuse_credentials: true,
        }
    }

    #[test]
    fn gpclient_argv_is_a_connect_invocation() {
        let p = portal(crate::Backend::Gpclient, crate::AuthMode::Password);
        let cmd = build_command(&p, &PathBuf::from("/bin/gpclient")).unwrap();
        assert_eq!(
            args_of(&cmd),
            [
                "connect",
                "vpn.etsp.ru",
                "--os=Windows",
                "--user",
                "alice",
                "--hip"
            ]
        );
    }

    #[test]
    fn each_backend_uses_its_own_os_value() {
        // openconnect takes the short name (`win`); gpclient validates against
        // Linux|Windows|Mac and rejects `win` with a clap error.
        let oc = args_of(
            &build_command(
                &portal(crate::Backend::Openconnect, crate::AuthMode::Password),
                &PathBuf::from("/bin/openconnect"),
            )
            .unwrap(),
        );
        assert!(oc.contains(&"--os=win".to_string()));
        assert!(!oc.contains(&"--os=Windows".to_string()));

        let gp = args_of(
            &build_command(
                &portal(crate::Backend::Gpclient, crate::AuthMode::Password),
                &PathBuf::from("/bin/gpclient"),
            )
            .unwrap(),
        );
        assert!(gp.contains(&"--os=Windows".to_string()));
        assert!(!gp.contains(&"--os=win".to_string()));
    }

    #[test]
    fn gpclient_never_receives_the_password_on_argv() {
        // The password travels over the PTY; a secret in argv is readable by
        // any local user via ps or /proc/<pid>/cmdline.
        let p = portal(crate::Backend::Gpclient, crate::AuthMode::Password);
        let args = args_of(&build_command(&p, &PathBuf::from("/bin/gpclient")).unwrap());
        for forbidden in ["--passwd-on-stdin", "--password", "-p"] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "{forbidden} must not be passed"
            );
        }
    }

    #[test]
    fn already_running_is_not_an_error() {
        let line =
            "[2026-11-04T20:58:17Z ERROR] Error: Another instance of the client is already running";
        assert!(is_already_running(line));
        assert!(!is_already_running("Connected to VPN, pipe_fd: 14"));
    }

    #[test]
    fn connected_detection_covers_both_backends() {
        // openconnect wording, address present.
        assert_eq!(
            connected_ip("Connected as 10.1.2.3, using SSL"),
            Some("10.1.2.3".to_string())
        );
        // gpclient wording: success arrives without an address.
        assert_eq!(
            connected_ip(
                "[2026-11-04T19:02:40Z INFO  openconnect::vpn] Connected to VPN, pipe_fd: 14"
            ),
            Some(String::new())
        );
        assert_eq!(
            connected_ip("ESP tunnel connected; exiting HTTPS mainloop."),
            None
        );
        assert_eq!(connected_ip("POST https://x/getconfig.esp"), None);
    }

    #[test]
    fn login_prompt_detection() {
        assert!(is_login_prompt("Enter login credentials"));
        assert!(is_login_prompt(
            "Enter login credentials (Portal: vpn.etsp.ru)"
        ));
        assert!(is_login_prompt("Enter your username:"));
        // The gateway form of gpclient is a different protocol path; answering
        // it with a Linux password would be wrong.
        assert!(!is_login_prompt("? Password: "));
        assert!(!is_login_prompt("Challenge: "));
        assert!(!is_login_prompt("Connected as 10.0.0.1"));
    }

    #[test]
    fn gpclient_gets_no_openconnect_only_flags() {
        // The HIP/CSD wrapper and the external-browser hook are openconnect
        // features; gpclient handles both itself and would reject the flags.
        let p = portal(crate::Backend::Gpclient, crate::AuthMode::Saml);
        let args = args_of(&build_command(&p, &PathBuf::from("/bin/gpclient")).unwrap());
        assert!(!args.iter().any(|a| a.starts_with("--csd-wrapper")));
        assert!(!args.iter().any(|a| a.starts_with("--external-browser")));
        assert!(!args.iter().any(|a| a == "--protocol=gp"));
    }

    #[test]
    fn gpclient_saml_uses_external_browser() {
        // The embedded webview cannot be driven from a spawned child, so SAML
        // must be delegated to the desktop browser.
        let p = portal(crate::Backend::Gpclient, crate::AuthMode::Saml);
        let args = args_of(&build_command(&p, &PathBuf::from("/bin/gpclient")).unwrap());
        assert!(args.contains(&"--browser".to_string()));

        // Password auth must not carry it.
        let p = portal(crate::Backend::Gpclient, crate::AuthMode::Password);
        let args = args_of(&build_command(&p, &PathBuf::from("/bin/gpclient")).unwrap());
        assert!(!args.contains(&"--browser".to_string()));
    }

    #[test]
    fn openconnect_argv_keeps_gp_flags_and_portal_last() {
        let p = portal(crate::Backend::Openconnect, crate::AuthMode::Saml);
        let args = args_of(&build_command(&p, &PathBuf::from("/bin/openconnect")).unwrap());
        assert_eq!(args[0], "--protocol=gp");
        assert!(args.contains(&"--os=win".to_string()));
        assert!(args.iter().any(|a| a.starts_with("--external-browser")));
        assert_eq!(args.last().map(String::as_str), Some("vpn.etsp.ru"));
    }

    #[test]
    fn password_auth_requires_a_username() {
        let mut p = portal(crate::Backend::Gpclient, crate::AuthMode::Password);
        p.username.clear();
        assert!(build_command(&p, &PathBuf::from("/bin/gpclient")).is_err());
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
