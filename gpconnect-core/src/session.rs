use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::PortalConfig;

pub const DEFAULT_OPENCONNECT_BIN: &str = "openconnect";

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
}

enum Job {
    Connect { portal: PortalConfig, password: Option<String>, openconnect_bin: PathBuf },
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
                rt.block_on(worker_loop(cmd_rx, event_tx, state2));
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

async fn worker_loop(
    mut cmd_rx: mpsc::UnboundedReceiver<Job>,
    events: mpsc::UnboundedSender<ConnEvent>,
    state: Arc<Mutex<ConnState>>,
) {
    let mut pump: Option<JoinHandle<()>> = None;

    while let Some(job) = cmd_rx.recv().await {
        // Abort the previous connection task. The Child inside it has
        // kill_on_drop set, so aborting kills openconnect.
        if let Some(p) = pump.take() {
            p.abort();
        }

        match job {
            Job::Stop => {
                *state.lock().unwrap() = ConnState::Disconnected;
                let _ = events.send(ConnEvent::State(ConnState::Disconnected));
            }
            Job::Connect { portal, password, openconnect_bin } => {
                *state.lock().unwrap() = ConnState::Connecting;
                let _ = events.send(ConnEvent::State(ConnState::Connecting));
                let child = match spawn_openconnect(&portal, password.as_deref(), &openconnect_bin)
                {
                    Ok(c) => c,
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
                let state2 = Arc::clone(&state);
                let events2 = events.clone();
                pump = Some(tokio::spawn(async move {
                    let _ = pump_output(child, portal, events2, state2, password, openconnect_bin)
                        .await;
                }));
            }
        }
    }
}

fn spawn_openconnect(
    portal: &PortalConfig,
    password: Option<&str>,
    bin: &PathBuf,
) -> io::Result<Child> {
    let mut cmd = Command::new(bin);
    cmd.args(["--protocol=gp", "--non-inter"])
        // Portals that reject unknown client OSes (this corp portal answers
        // getconfig.esp with HTTP 512 for clientos=Linux) accept Windows;
        // this mirrors the gpclient invocation that worked against it.
        .arg("--os=win")
        .arg("--user")
        .arg(&portal.username)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Dropping the pump task (abort/replace) must kill openconnect.
        .kill_on_drop(true);

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
        crate::AuthMode::Password => {
            if password.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "password auth requires a password",
                ));
            }
            cmd.arg("--passwd-on-stdin");
        }
        crate::AuthMode::Saml => {
            // OpenConnect runs a localhost redirect listener and asks this
            // command to open the SSO URL in the user's browser.
            cmd.arg(format!("--external-browser={}", browser_opener()));
        }
    }

    cmd.arg(&portal.portal);
    cmd.spawn()
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

/// Feed the password, translate openconnect output into state events, wait
/// for exit. On unexpected exit with `reconnect_on_drop`, restart the child.
async fn pump_output(
    mut child: Child,
    portal: PortalConfig,
    events: mpsc::UnboundedSender<ConnEvent>,
    state: Arc<Mutex<ConnState>>,
    password: Option<String>,
    bin: PathBuf,
) {
    // Password first, then close stdin so openconnect doesn't block on it.
    if portal.auth == crate::AuthMode::Password {
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(pw) = &password {
                let _ = stdin.write_all(pw.as_bytes()).await;
                let _ = stdin.write_all(b"\n").await;
            }
            let _ = stdin.shutdown().await;
        }
    }

    let progress = Arc::new(Mutex::new(Progress {
        connected: false,
        info: TunnelInfo::default(),
        stage: String::new(),
    }));

    let stderr = child.stderr.take();
    let stdout = child.stdout.take();
    let mut handles = Vec::new();
    if let Some(s) = stderr {
        let (events, progress, portal) = (events.clone(), Arc::clone(&progress), portal.portal.clone());
        handles.push(tokio::spawn(async move {
            read_lines(s, events, progress, portal).await;
        }));
    }
    if let Some(s) = stdout {
        let (events, progress, portal) = (events.clone(), Arc::clone(&progress), portal.portal.clone());
        handles.push(tokio::spawn(async move {
            read_lines(s, events, progress, portal).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    let connected = progress.lock().unwrap().connected;

    let status = child.wait().await.ok();

    if connected {
        // Unexpected drop (not killed by us): maybe reconnect.
        let want_reconnect = crate::AppConfig::load().settings.reconnect_on_drop;
        if want_reconnect {
            let _ = events.send(ConnEvent::State(ConnState::Connecting));
            *state.lock().unwrap() = ConnState::Connecting;
            tokio::time::sleep(Duration::from_secs(3)).await;
            if let Ok(new_child) = spawn_openconnect(&portal, password.as_deref(), &bin) {
                Box::pin(pump_output(new_child, portal, events, state, password, bin)).await;
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

struct Progress {
    connected: bool,
    info: TunnelInfo,
    stage: String,
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

async fn read_lines(
    stream: impl tokio::io::AsyncRead + Unpin,
    events: mpsc::UnboundedSender<ConnEvent>,
    progress: Arc<Mutex<Progress>>,
    portal: String,
) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::info!("openconnect: {line}");
        let _ = events.send(ConnEvent::Log(line.clone()));

        let mut p = progress.lock().unwrap();
        if !p.connected {
            if line.contains("Connected as") {
                p.connected = true;
                p.info.ip = line
                    .split("Connected as")
                    .nth(1)
                    .and_then(|rest| rest.split(',').next())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                p.info.gateway = portal.clone();
                let _ = events.send(ConnEvent::State(ConnState::Connected(p.info.clone())));
            } else {
                if is_auth_failure(&line) {
                    let _ = events.send(ConnEvent::State(ConnState::Error(
                        "authentication failed".into(),
                    )));
                }
                if let Some(stage) = stage_for_line(&line, &p.stage) {
                    p.stage = stage.into();
                    let _ = events.send(ConnEvent::Stage(stage.into()));
                }
            }
        }
    }
}

fn is_auth_failure(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    ["authentication failure", "login failed", "bad credentials", "401", "authfailed"]
        .iter()
        .any(|p| l.contains(p))
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
