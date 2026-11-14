use std::io::{IoSlice, IoSliceMut};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use nix::sys::signal::kill;
use nix::sys::socket::{recvmsg, sendmsg, ControlMessage, MsgFlags};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct Request<'a> {
    backend: &'a str,
    args: Vec<String>,
}

#[derive(Deserialize)]
struct Response {
    pid: u32,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let backend = args.next().unwrap_or_else(|| {
        eprintln!("palisade-helper-client: missing backend");
        std::process::exit(2);
    });
    let rest: Vec<String> = args.collect();
    let socket_path = std::env::var("PALISADE_HELPER_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let base = std::env::var("XDG_RUNTIME_DIR")
                .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
            PathBuf::from(base).join("palisade-helper.sock")
        });
    if let Err(e) = run(&backend, &rest, &socket_path) {
        eprintln!("palisade-helper-client: {e}");
        std::process::exit(1);
    }
}

fn run(backend: &str, args: &[String], socket_path: &PathBuf) -> Result<(), String> {
    let stream = UnixStream::connect(socket_path).map_err(|e| format!("connect to helper: {e}"))?;
    let req = serde_json::to_vec(&Request {
        backend,
        args: args.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    let fds = [0, 1, 2];
    sendmsg::<()>(
        stream.as_raw_fd(),
        &[IoSlice::new(&req)],
        &[ControlMessage::ScmRights(&fds)],
        MsgFlags::empty(),
        None,
    )
    .map_err(|e| format!("send request: {e}"))?;

    let mut reply = [0u8; 128];
    let bytes = {
        let mut iov = [IoSliceMut::new(&mut reply)];
        recvmsg::<()>(stream.as_raw_fd(), &mut iov, None, MsgFlags::empty())
            .map_err(|e| format!("read reply: {e}"))?
            .bytes
    };
    let resp: Response =
        serde_json::from_slice(&reply[..bytes]).map_err(|e| format!("invalid reply: {e}"))?;
    let pid = Pid::from_raw(resp.pid as i32);

    // The real child belongs to the root helper. Poll until it disappears;
    // tokio's kill_on_drop terminates this proxy on disconnect, and the signal
    // handler below forwards that to the privileged child.
    install_signal_forwarder(pid);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if kill(pid, None).is_err() {
            break;
        }
    }
    Ok(())
}

fn install_signal_forwarder(pid: Pid) {
    unsafe {
        libc::signal(
            libc::SIGTERM,
            forward_sigterm as *const () as libc::sighandler_t,
        );
    }
    // SAFETY: the handler only calls kill(2), which is async-signal-safe.
    static mut CHILD_PID: i32 = 0;
    unsafe {
        CHILD_PID = pid.as_raw();
    }
    extern "C" fn forward_sigterm(_: libc::c_int) {
        unsafe {
            libc::kill(CHILD_PID, libc::SIGTERM);
            libc::_exit(0);
        }
    }
}
