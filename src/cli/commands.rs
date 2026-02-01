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

// -- Tunnel commands --

pub fn connect(id: &str) {
    match send_rpc("tunnel.connect", json!({ "id": id })) {
        Ok(resp) => {
            if let Some(err) = resp.error {
                eprintln!("error: {}", err.message);
                std::process::exit(1);
            }
            println!("tunnel '{id}' connected");
        }
        Err(_) => {
            eprintln!("daemon is not running");
            std::process::exit(1);
        }
    }
}

pub fn disconnect(id: &str) {
    match send_rpc("tunnel.disconnect", json!({ "id": id })) {
        Ok(resp) => {
            if let Some(err) = resp.error {
                eprintln!("error: {}", err.message);
                std::process::exit(1);
            }
            println!("tunnel '{id}' disconnected");
        }
        Err(_) => {
            eprintln!("daemon is not running");
            std::process::exit(1);
        }
    }
}

pub fn enable(id: &str) {
    match send_rpc("tunnel.enable", json!({ "id": id })) {
        Ok(resp) => {
            if let Some(err) = resp.error {
                eprintln!("error: {}", err.message);
                std::process::exit(1);
            }
            println!("tunnel '{id}' enabled");
        }
        Err(_) => {
            eprintln!("daemon is not running");
            std::process::exit(1);
        }
    }
}

pub fn disable(id: &str) {
    match send_rpc("tunnel.disable", json!({ "id": id })) {
        Ok(resp) => {
            if let Some(err) = resp.error {
                eprintln!("error: {}", err.message);
                std::process::exit(1);
            }
            println!("tunnel '{id}' disabled");
        }
        Err(_) => {
            eprintln!("daemon is not running");
            std::process::exit(1);
        }
    }
}

// -- Bulk commands --

pub fn connect_all() {
    bulk_command("tunnel.connect_all", "connected")
}

pub fn disconnect_all() {
    bulk_command("tunnel.disconnect_all", "disconnected")
}

pub fn restart_all() {
    bulk_command("tunnel.restart_all", "connected")
}

fn bulk_command(method: &str, verb: &str) {
    match send_rpc(method, json!({})) {
        Ok(resp) => {
            if let Some(err) = resp.error {
                eprintln!("error: {}", err.message);
                std::process::exit(1);
            }
            if let Some(result) = resp.result {
                let succeeded = result["succeeded"].as_u64().unwrap_or(0);
                let failed = result["failed"].as_u64().unwrap_or(0);
                if failed == 0 {
                    println!("{verb} {succeeded} tunnels");
                } else {
                    println!("{verb} {succeeded} tunnels, {failed} failed");
                    if let Some(errors) = result["errors"].as_array() {
                        for e in errors {
                            if let Some(s) = e.as_str() {
                                eprintln!("  - {s}");
                            }
                        }
                    }
                }
            }
        }
        Err(_) => {
            eprintln!("daemon is not running");
            std::process::exit(1);
        }
    }
}

// -- Logs command --

pub fn logs(args: &crate::cli::LogsArgs) {
    let path = crate::common::logging::log_path();

    if !path.exists() {
        eprintln!("no log file found at: {}", path.display());
        std::process::exit(1);
    }

    if args.follow {
        logs_follow(&path, args.tunnel.as_deref());
    } else {
        logs_tail(&path, args.tunnel.as_deref());
    }
}

/// Print the last 100 lines (optionally filtered).
fn logs_tail(path: &std::path::Path, tunnel_filter: Option<&str>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to read log file: {e}");
            std::process::exit(1);
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    let tail = if lines.len() > 100 { &lines[lines.len() - 100..] } else { &lines };

    for line in tail {
        if matches_filter(line, tunnel_filter) {
            println!("{line}");
        }
    }
}

/// Continuously tail the log file, printing new lines as they appear.
fn logs_follow(path: &std::path::Path, tunnel_filter: Option<&str>) {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("failed to open log file: {e}");
            std::process::exit(1);
        }
    };

    // Start from end of file
    if let Err(e) = file.seek(SeekFrom::End(0)) {
        eprintln!("failed to seek log file: {e}");
        std::process::exit(1);
    }

    let mut buf = String::new();
    loop {
        buf.clear();
        match file.read_to_string(&mut buf) {
            Ok(0) => {
                // No new data -- sleep and retry
                std::thread::sleep(Duration::from_millis(200));
            }
            Ok(_) => {
                for line in buf.lines() {
                    if matches_filter(line, tunnel_filter) {
                        println!("{line}");
                    }
                }
            }
            Err(e) => {
                eprintln!("error reading log file: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn matches_filter(line: &str, tunnel_filter: Option<&str>) -> bool {
    match tunnel_filter {
        None => true,
        Some(id) => line.contains(id),
    }
}

// -- Config commands --

pub fn config_path() {
    println!("{}", crate::config::config_path().display());
}

pub fn config_edit() {
    let path = crate::config::config_path();

    // Create sample config if missing so the editor has something to show
    if !path.exists() {
        crate::config::sample::ensure_config_exists();
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    let status = Command::new(&editor)
        .arg(&path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("editor exited with {s}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("failed to launch editor '{editor}': {e}");
            std::process::exit(1);
        }
    }
}

pub fn config_reload() {
    match send_rpc("config.reload", json!({})) {
        Ok(resp) => {
            if let Some(err) = resp.error {
                eprintln!("error: {}", err.message);
                std::process::exit(1);
            }
            if let Some(result) = resp.result {
                let added = result["added"].as_array().map_or(0, |a| a.len());
                let removed = result["removed"].as_array().map_or(0, |a| a.len());
                let updated = result["updated"].as_array().map_or(0, |a| a.len());
                let errors = result["errors"].as_array().map_or(0, |a| a.len());

                if added == 0 && removed == 0 && updated == 0 {
                    println!("config reloaded, no changes");
                } else {
                    let mut parts = Vec::new();
                    if added > 0 {
                        parts.push(format!("{added} added"));
                    }
                    if removed > 0 {
                        parts.push(format!("{removed} removed"));
                    }
                    if updated > 0 {
                        parts.push(format!("{updated} updated"));
                    }
                    println!("config reloaded: {}", parts.join(", "));
                }

                if errors > 0 {
                    if let Some(errs) = result["errors"].as_array() {
                        for e in errs {
                            if let Some(s) = e.as_str() {
                                eprintln!("  - {s}");
                            }
                        }
                    }
                }
            }
        }
        Err(_) => {
            eprintln!("daemon is not running");
            std::process::exit(1);
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

fn direction_arrow(tunnel_type: &str) -> &'static str {
    match tunnel_type {
        "local" => "\u{2192}",   // →
        "reverse" => "\u{2190}", // ←
        "socks" => "\u{21c4}",   // ⇄
        _ => "\u{2192}",
    }
}

fn format_tunnel_table(tunnels: &[TunnelInfo]) -> String {
    // Each row: [name, status, local, remote, tunnel_type]
    let mut rows: Vec<[String; 5]> = Vec::with_capacity(tunnels.len());

    for t in tunnels {
        let local = format!("localhost:{}", t.local_port);
        let remote = t.remote.as_deref().unwrap_or("-").to_string();
        rows.push([
            t.name.clone(),
            status_indicator(t.status).to_string(),
            local,
            remote,
            t.tunnel_type.clone(),
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
        let arrow = direction_arrow(&row[4]);
        out.push_str(&format!(
            "{:<w0$}  {}{:<pad$}  {:<w2$}  {}  {}\n",
            row[0], row[1], "", row[2], arrow, row[3],
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
pub(crate) fn send_rpc(method: &str, params: serde_json::Value) -> Result<RpcResponse, RpcClientError> {
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
pub(crate) enum RpcClientError {
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

    fn make_tunnel_typed(
        name: &str,
        tunnel_type: &str,
        status: TunnelStatus,
        local_port: u16,
        remote: Option<&str>,
        last_error: Option<&str>,
    ) -> TunnelInfo {
        TunnelInfo {
            id: name.to_lowercase().replace(' ', "-"),
            name: name.to_string(),
            tunnel_type: tunnel_type.to_string(),
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

    fn make_tunnel(
        name: &str,
        status: TunnelStatus,
        local_port: u16,
        remote: Option<&str>,
        last_error: Option<&str>,
    ) -> TunnelInfo {
        make_tunnel_typed(name, "local", status, local_port, remote, last_error)
    }

    #[test]
    fn status_indicators() {
        assert_eq!(status_indicator(TunnelStatus::Connected), "\u{25cf} connected");
        assert_eq!(status_indicator(TunnelStatus::Disconnected), "\u{25cb} disconnected");
        assert_eq!(status_indicator(TunnelStatus::Connecting), "\u{25d0} connecting");
        assert_eq!(status_indicator(TunnelStatus::Error), "\u{26a0} error");
    }

    #[test]
    fn direction_arrows() {
        assert_eq!(direction_arrow("local"), "\u{2192}");
        assert_eq!(direction_arrow("reverse"), "\u{2190}");
        assert_eq!(direction_arrow("socks"), "\u{21c4}");
    }

    #[test]
    fn table_local_uses_right_arrow() {
        let tunnels = vec![make_tunnel(
            "Dev Database",
            TunnelStatus::Connected,
            5432,
            Some("db.internal:5432"),
            None,
        )];
        let out = format_tunnel_table(&tunnels);
        let lines: Vec<&str> = out.lines().collect();

        assert!(lines[0].contains("TUNNEL"));
        assert!(lines[0].contains("STATUS"));
        assert!(lines[0].contains("LOCAL"));
        assert!(lines[0].contains("REMOTE"));

        assert!(lines[1].contains("Dev Database"));
        assert!(lines[1].contains("\u{25cf} connected"));
        assert!(lines[1].contains("localhost:5432"));
        assert!(lines[1].contains("\u{2192}")); // →
        assert!(lines[1].contains("db.internal:5432"));
    }

    #[test]
    fn table_reverse_uses_left_arrow() {
        let tunnels = vec![make_tunnel_typed(
            "Expose API",
            "reverse",
            TunnelStatus::Connected,
            8080,
            Some("0.0.0.0:9000"),
            None,
        )];
        let out = format_tunnel_table(&tunnels);
        let line = out.lines().nth(1).unwrap();
        assert!(line.contains("\u{2190}")); // ←
        assert!(line.contains("0.0.0.0:9000"));
    }

    #[test]
    fn table_socks_uses_bidir_arrow() {
        let tunnels = vec![make_tunnel_typed(
            "Proxy",
            "socks",
            TunnelStatus::Connected,
            1080,
            Some("SOCKS5"),
            None,
        )];
        let out = format_tunnel_table(&tunnels);
        let line = out.lines().nth(1).unwrap();
        assert!(line.contains("\u{21c4}")); // ⇄
        assert!(line.contains("SOCKS5"));
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

        assert!(lines[1].starts_with("DB"));
        assert!(lines[2].starts_with("Long Tunnel Name"));
    }

    #[test]
    fn table_mixed_types_show_correct_arrows() {
        let tunnels = vec![
            make_tunnel_typed(
                "DB", "local", TunnelStatus::Connected, 5432, Some("db:5432"), None,
            ),
            make_tunnel_typed(
                "API", "reverse", TunnelStatus::Connected, 8080, Some("0.0.0.0:9000"), None,
            ),
            make_tunnel_typed(
                "Proxy", "socks", TunnelStatus::Connected, 1080, Some("SOCKS5"), None,
            ),
        ];
        let out = format_tunnel_table(&tunnels);
        let lines: Vec<&str> = out.lines().collect();

        assert!(lines[1].contains("\u{2192}")); // local →
        assert!(lines[2].contains("\u{2190}")); // reverse ←
        assert!(lines[3].contains("\u{21c4}")); // socks ⇄
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
