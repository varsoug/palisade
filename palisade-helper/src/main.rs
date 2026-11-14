use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use nix::sys::socket::sockopt::PeerCredentials;
use nix::sys::socket::{getsockopt, recvmsg, sendmsg, ControlMessageOwned, MsgFlags};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Request {
    backend: String,
    args: Vec<String>,
}

#[derive(Serialize)]
struct Response {
    pid: u32,
}

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = std::env::var("XDG_RUNTIME_DIR")
                .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
            PathBuf::from(base).join("palisade-helper.sock")
        });
    let gui_pid = std::env::args_os()
        .nth(2)
        .and_then(|v| v.to_str().and_then(|s| s.parse::<i32>().ok()));
    if let Err(e) = run(&path, gui_pid) {
        eprintln!("palisade-helper: {e}");
        std::process::exit(1);
    }
}

fn run(path: &PathBuf, gui_pid: Option<i32>) -> Result<(), String> {
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| format!("remove stale socket: {e}"))?;
    }
    let listener = UnixListener::bind(path).map_err(|e| format!("bind socket: {e}"))?;
    let invoking_uid = std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| "PKEXEC_UID is missing".to_string())?;
    let path_c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|e| format!("socket path: {e}"))?;
    if unsafe { libc::chown(path_c.as_ptr(), invoking_uid, -1i32 as u32) } != 0 {
        return Err(format!("chown socket: {}", std::io::Error::last_os_error()));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("protect socket: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("socket nonblocking mode: {e}"))?;
    let mut children: Vec<(u32, &'static str)> = Vec::new();
    let mut gui_gone_at: Option<std::time::Instant> = None;
    loop {
        if let Some(pid) = gui_pid {
            if unsafe { libc::kill(pid, 0) } != 0 {
                let deadline = *gui_gone_at.get_or_insert_with(std::time::Instant::now);
                if deadline.elapsed() > std::time::Duration::from_secs(20) {
                    cleanup_children(&children);
                    let _ = std::fs::remove_file(path);
                    return Ok(());
                }
            } else {
                gui_gone_at = None;
            }
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(e) = handle(stream, &mut children) {
                    eprintln!("palisade-helper: request failed: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!("palisade-helper: accept failed: {e}");
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
}

fn handle(stream: UnixStream, children: &mut Vec<(u32, &'static str)>) -> Result<(), String> {
    let expected_uid = std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| "PKEXEC_UID is missing".to_string())?;
    let peer =
        getsockopt(&stream, PeerCredentials).map_err(|e| format!("peer credentials: {e}"))?;
    if peer.uid() != expected_uid {
        return Err(format!(
            "peer uid {} is not the invoking user {}",
            peer.uid(),
            expected_uid
        ));
    }
    let fd = stream.as_raw_fd();
    let mut buf = [0u8; 8192];
    let (bytes, fds) = {
        let mut iov = [IoSliceMut::new(&mut buf)];
        let mut cmsg = Vec::<u8>::with_capacity(128);
        let msg = recvmsg::<()>(fd, &mut iov, Some(&mut cmsg), MsgFlags::empty())
            .map_err(|e| format!("recvmsg: {e}"))?;
        let mut fds = Vec::new();
        for c in msg.cmsgs().map_err(|e| format!("cmsgs: {e}"))? {
            if let ControlMessageOwned::ScmRights(v) = c {
                fds.extend(v);
            }
        }
        (msg.bytes, fds)
    };
    let req: Request =
        serde_json::from_slice(&buf[..bytes]).map_err(|e| format!("invalid request: {e}"))?;
    if fds.len() != 3 {
        return Err(format!("expected 3 stdio fds, got {}", fds.len()));
    }
    let backend = match req.backend.as_str() {
        "gpclient" => "gpclient",
        "openconnect" => "openconnect",
        _ => return Err(format!("unsupported backend: {}", req.backend)),
    };
    let program = resolve_backend(backend)?;
    let stdin = unsafe { Stdio::from(OwnedFd::from_raw_fd(fds[0])) };
    let stdout = unsafe { Stdio::from(OwnedFd::from_raw_fd(fds[1])) };
    let stderr = unsafe { Stdio::from(OwnedFd::from_raw_fd(fds[2])) };
    let child = Command::new(program)
        .args(req.args)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", req.backend))?;
    let reply = serde_json::to_vec(&Response { pid: child.id() }).map_err(|e| e.to_string())?;
    sendmsg::<()>(fd, &[IoSlice::new(&reply)], &[], MsgFlags::empty(), None)
        .map_err(|e| format!("sendmsg: {e}"))?;
    children.push((child.id(), backend));
    std::mem::forget(child);
    Ok(())
}

fn cleanup_children(children: &[(u32, &'static str)]) {
    if children.iter().any(|(_, backend)| *backend == "gpclient") {
        if let Ok(gpclient) = resolve_backend("gpclient") {
            let _ = Command::new(gpclient)
                .arg("disconnect")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    for (pid, _) in children {
        unsafe {
            libc::kill(*pid as i32, libc::SIGTERM);
        }
    }
}

fn resolve_backend(backend: &str) -> Result<String, String> {
    let config = std::fs::read_to_string("/usr/local/etc/palisade-helper.json")
        .map_err(|e| format!("read helper config: {e}"))?;
    let cfg: serde_json::Value =
        serde_json::from_str(&config).map_err(|e| format!("parse helper config: {e}"))?;
    let key = match backend {
        "gpclient" => "gpclient_bin",
        "openconnect" => "openconnect_bin",
        _ => return Err(format!("unsupported backend: {backend}")),
    };
    cfg[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{key} missing from helper config"))
}
