//! Ensures a single Palisade instance owns the desktop session.
//!
//! A stale GUI (crashed, detached, started twice) is a common way to end up
//! with two windows and two competing VPN sessions. Starting a new instance
//! therefore takes over: it terminates any older GUI, then lets the caller
//! clean up whatever VPN session it left behind.

use std::io::Write;

use fs2::FileExt;

/// Signals a process by id. Windows has no SIGTERM/kill(2); a new instance
/// there simply starts alongside (the VPN client itself refuses the second
/// session, so the app still cannot corrupt state).
#[cfg(unix)]
fn signal(pid: i32, sig: i32) -> bool {
    // SAFETY: kill(2) with a plain pid and signal number; no memory involved.
    unsafe { libc::kill(pid, sig) == 0 }
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for the pid's existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn signal(_pid: i32, _sig: i32) -> bool {
    false
}

#[cfg(not(unix))]
fn process_exists(_pid: i32) -> bool {
    false
}

/// Claims the instance lock, terminating older Palisade processes first.
///
/// Returns the locked file, which must stay alive for the duration of the
/// process — dropping it releases the lock.
pub fn claim() -> Option<std::fs::File> {
    let path = instance_path()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .ok()?;

    // Someone else holds it: ask them to leave, then take over.
    if file.try_lock_exclusive().is_err() {
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let old: i32 = old.trim().parse().unwrap_or(0);
        if old > 0 && old != std::process::id() as i32 {
            tracing::info!("replacing running Palisade instance (pid {old})");
            terminate(old);
        }
        // Wait briefly for the lock to be released.
        for _ in 0..40 {
            if file.try_lock_exclusive().is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    let mut f = &file;
    let _ = f.set_len(0);
    let _ = write!(f, "{}", std::process::id());
    Some(file)
}

#[cfg(unix)]
fn terminate(pid: i32) {
    // A stale pidfile can name a process that is not us: PID reuse is common
    // after a reboot or a long session. Never signal anything but our own
    // binary.
    if !is_own_process(pid) {
        tracing::info!("not terminating pid {pid}: not a Palisade process");
        return;
    }
    // SIGTERM first, SIGKILL if it does not go away.
    for sig in [libc::SIGTERM, libc::SIGKILL] {
        signal(pid, sig);
        for _ in 0..20 {
            if !process_exists(pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

#[cfg(not(unix))]
fn terminate(_pid: i32) {}

/// Is `pid` a Palisade binary? Guards against killing an unrelated process
/// that inherited a recycled PID from a stale pidfile.
#[cfg(unix)]
fn is_own_process(pid: i32) -> bool {
    let Some(own) = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_os_string()))
    else {
        return false;
    };
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(exe) => exe.file_name().map(|n| n.to_os_string()).as_deref() == Some(own.as_os_str()),
        Err(_) => false,
    }
}

fn instance_path() -> Option<std::path::PathBuf> {
    let dir = directories::ProjectDirs::from("com", "Palisade", "palisade")?
        .cache_dir()
        .to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("palisade.pid"))
}
