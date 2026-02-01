use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::daemon;
use crate::ipc::protocol::{DaemonInfo, RpcRequest, RpcResponse, JSONRPC_VERSION};

// -- Daemon commands --

pub fn daemon_start() {
    let socket = daemon::socket_path();

    if UnixStream::connect(&socket).is_ok() {
        eprintln!("daemon is already running");
        std::process::exit(1);
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot determine executable path: {e}");
            std::process::exit(1);
        }
    };

    let child = match Command::new(&exe)
        .arg("daemon-foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to spawn daemon: {e}");
            std::process::exit(1);
        }
    };

    let pid = child.id();

    if wait_for_socket(&socket, Duration::from_secs(3)) {
        println!("daemon started (pid {pid})");
    } else {
        eprintln!("daemon failed to start (socket not available after 3s)");
        std::process::exit(1);
    }
}

pub fn daemon_stop() {
    match send_rpc("daemon.shutdown", json!({})) {
        Ok(resp) => {
            if resp.error.is_some() {
                let err = resp.error.unwrap();
                eprintln!("shutdown failed: {}", err.message);
                std::process::exit(1);
            }
            println!("daemon stopped");
        }
        Err(e) => {
            eprintln!("cannot connect to daemon: {e}");
            std::process::exit(1);
        }
    }
}

pub fn daemon_restart() {
    // Stop if running, then start
    let socket = daemon::socket_path();
    if UnixStream::connect(&socket).is_ok() {
        if let Ok(resp) = send_rpc("daemon.shutdown", json!({})) {
            if resp.error.is_none() {
                // Wait for socket to disappear
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(3) {
                    if !socket.exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    daemon_start();
}

pub fn daemon_status() {
    match send_rpc("daemon.status", json!({})) {
        Ok(resp) => {
            if let Some(result) = resp.result {
                match serde_json::from_value::<DaemonInfo>(result) {
                    Ok(info) => {
                        println!("Daemon: running");
                        println!("  Version:  {}", info.version);
                        println!("  Uptime:   {}", format_uptime(info.uptime_seconds));
                        println!("  Tunnels:  {}", info.tunnel_count);
                    }
                    Err(e) => {
                        eprintln!("failed to parse daemon status: {e}");
                        std::process::exit(1);
                    }
                }
            } else if let Some(err) = resp.error {
                eprintln!("daemon error: {}", err.message);
                std::process::exit(1);
            }
        }
        Err(_) => {
            println!("Daemon: not running");
        }
    }
}

// -- Helpers --

/// Send a JSON-RPC request to the daemon and read one response.
fn send_rpc(method: &str, params: serde_json::Value) -> Result<RpcResponse, RpcClientError> {
    let socket = daemon::socket_path();
    let mut stream = UnixStream::connect(&socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

    let request = RpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: 1,
        method: method.to_string(),
        params,
    };

    let mut buf = serde_json::to_string(&request)?;
    buf.push('\n');
    stream.write_all(buf.as_bytes())?;

    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;

    let response: RpcResponse = serde_json::from_str(&line)?;
    Ok(response)
}

/// Poll until the daemon socket accepts connections.
fn wait_for_socket(socket: &std::path::Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86400;
    let hours = (seconds % 86400) / 3600;
    let mins = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

#[derive(Debug, thiserror::Error)]
enum RpcClientError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_seconds_only() {
        assert_eq!(format_uptime(45), "45s");
    }

    #[test]
    fn uptime_minutes() {
        assert_eq!(format_uptime(125), "2m 5s");
    }

    #[test]
    fn uptime_hours() {
        assert_eq!(format_uptime(3661), "1h 1m");
    }

    #[test]
    fn uptime_days() {
        assert_eq!(format_uptime(90061), "1d 1h 1m");
    }
}
