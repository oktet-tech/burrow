use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::daemon;
use crate::ipc::protocol::{
    DaemonInfo, RpcRequest, RpcResponse, TunnelInfo, TunnelStatus, JSONRPC_VERSION,
};

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

// -- Status command --

pub fn status() {
    match send_rpc("tunnel.list", json!({})) {
        Ok(resp) => {
            if let Some(err) = resp.error {
                eprintln!("error: {}", err.message);
                std::process::exit(1);
            }
            let result = resp.result.unwrap_or_default();
            let tunnels: Vec<TunnelInfo> = match result.get("tunnels") {
                Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
                None => Vec::new(),
            };
            if tunnels.is_empty() {
                println!("No tunnels configured.");
                return;
            }
            print!("{}", format_tunnel_table(&tunnels));
        }
        Err(_) => {
            eprintln!("daemon is not running");
            std::process::exit(1);
        }
    }
}

fn status_indicator(status: TunnelStatus) -> &'static str {
    match status {
        TunnelStatus::Connected => "\u{25cf} connected",
        TunnelStatus::Disconnected => "\u{25cb} disconnected",
        TunnelStatus::Connecting => "\u{25d0} connecting",
        TunnelStatus::Error => "\u{26a0} error",
    }
}

fn format_tunnel_table(tunnels: &[TunnelInfo]) -> String {
    let mut rows: Vec<[String; 4]> = Vec::with_capacity(tunnels.len());

    for t in tunnels {
        let local = format!("localhost:{}", t.local_port);
        let remote = t.remote.as_deref().unwrap_or("-").to_string();
        rows.push([
            t.name.clone(),
            status_indicator(t.status).to_string(),
            local,
            remote,
        ]);
    }

    // Column widths (minimum = header length)
    let mut w = [6usize, 14, 5, 6]; // TUNNEL, STATUS (unicode widths), LOCAL, REMOTE
    for row in &rows {
        w[0] = w[0].max(row[0].len());
        // Status column: display width differs from byte length due to unicode
        w[1] = w[1].max(display_width(&row[1]));
        w[2] = w[2].max(row[2].len());
        w[3] = w[3].max(row[3].len());
    }

    let mut out = String::new();

    // Header
    out.push_str(&format!(
        "{:<w0$}  {:<w1$}  {:<w2$}     {:<w3$}\n",
        "TUNNEL", "STATUS", "LOCAL", "REMOTE",
        w0 = w[0], w1 = w[1], w2 = w[2], w3 = w[3],
    ));

    // Rows
    for row in &rows {
        let pad = w[1].saturating_sub(display_width(&row[1]));
        out.push_str(&format!(
            "{:<w0$}  {}{:<pad$}  {:<w2$}  \u{2192}  {}\n",
            row[0], row[1], "", row[2], row[3],
            w0 = w[0], pad = pad, w2 = w[2],
        ));
    }

    // Error details below the table
    for t in tunnels {
        if let Some(ref err) = t.last_error {
            out.push_str(&format!("  {} error: {}\n", t.name, err));
        }
    }

    out
}

/// Approximate display width accounting for multi-byte unicode symbols.
/// The status indicators (●○◐⚠) each occupy ~2 display columns in most terminals.
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if c.is_ascii() { 1 } else { 2 })
        .sum()
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

    fn make_tunnel(
        name: &str,
        status: TunnelStatus,
        local_port: u16,
        remote: Option<&str>,
        last_error: Option<&str>,
    ) -> TunnelInfo {
        TunnelInfo {
            id: name.to_lowercase().replace(' ', "-"),
            name: name.to_string(),
            tunnel_type: "local".to_string(),
            mode: "auto".to_string(),
            status,
            local_port,
            remote: remote.map(String::from),
            host: "example.com".to_string(),
            enabled: true,
            last_error: last_error.map(String::from),
            stats: None,
        }
    }

    #[test]
    fn status_indicators() {
        assert_eq!(status_indicator(TunnelStatus::Connected), "\u{25cf} connected");
        assert_eq!(status_indicator(TunnelStatus::Disconnected), "\u{25cb} disconnected");
        assert_eq!(status_indicator(TunnelStatus::Connecting), "\u{25d0} connecting");
        assert_eq!(status_indicator(TunnelStatus::Error), "\u{26a0} error");
    }

    #[test]
    fn table_single_connected() {
        let tunnels = vec![make_tunnel(
            "Dev Database",
            TunnelStatus::Connected,
            5432,
            Some("db.internal:5432"),
            None,
        )];
        let out = format_tunnel_table(&tunnels);
        let lines: Vec<&str> = out.lines().collect();

        // Header
        assert!(lines[0].contains("TUNNEL"));
        assert!(lines[0].contains("STATUS"));
        assert!(lines[0].contains("LOCAL"));
        assert!(lines[0].contains("REMOTE"));

        // Data row
        assert!(lines[1].contains("Dev Database"));
        assert!(lines[1].contains("\u{25cf} connected"));
        assert!(lines[1].contains("localhost:5432"));
        assert!(lines[1].contains("\u{2192}"));
        assert!(lines[1].contains("db.internal:5432"));
    }

    #[test]
    fn table_error_shows_detail() {
        let tunnels = vec![make_tunnel(
            "Broken",
            TunnelStatus::Error,
            3000,
            Some("host:3000"),
            Some("exited with code 255"),
        )];
        let out = format_tunnel_table(&tunnels);

        assert!(out.contains("\u{26a0} error"));
        assert!(out.contains("Broken error: exited with code 255"));
    }

    #[test]
    fn table_multiple_tunnels_aligned() {
        let tunnels = vec![
            make_tunnel("DB", TunnelStatus::Connected, 5432, Some("db:5432"), None),
            make_tunnel(
                "Long Tunnel Name",
                TunnelStatus::Disconnected,
                8080,
                Some("api:8080"),
                None,
            ),
        ];
        let out = format_tunnel_table(&tunnels);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 rows

        // Both name columns should be padded to same width
        assert!(lines[1].starts_with("DB"));
        assert!(lines[2].starts_with("Long Tunnel Name"));
    }

    #[test]
    fn table_socks_shows_socks5_remote() {
        let tunnels = vec![make_tunnel(
            "Proxy",
            TunnelStatus::Connected,
            1080,
            Some("SOCKS5"),
            None,
        )];
        let out = format_tunnel_table(&tunnels);
        assert!(out.contains("SOCKS5"));
    }

    #[test]
    fn table_no_remote_shows_dash() {
        let tunnels = vec![make_tunnel(
            "Mystery",
            TunnelStatus::Disconnected,
            9999,
            None,
            None,
        )];
        let out = format_tunnel_table(&tunnels);
        assert!(out.contains("\u{2192}  -"));
    }
}
