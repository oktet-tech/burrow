//! Launching the background daemon from the CLI or GUI.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Spawn `burrow daemon-foreground` in its own session and return its pid.
///
/// The new session drops the caller's controlling terminal, so closing the
/// terminal or pressing Ctrl-C doesn't reach the daemon or its SSH children.
pub fn spawn_daemon() -> std::io::Result<u32> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon-foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(cmd.spawn()?.id())
}

/// Poll until the daemon socket accepts connections.
pub fn wait_for_socket(socket: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}
