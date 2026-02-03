use serde_json::json;

use crate::cli::{TunnelAddArgs, TunnelModifyArgs};
use crate::config::{
    self, is_valid_tunnel_id, validate_config, Config, Defaults, TunnelConfig, TunnelMode,
    TunnelType,
};
use crate::ipc::protocol::TunnelStatus;

use super::commands::{send_rpc, RpcClientError};

// -- Public command handlers --

pub fn tunnel_add(args: TunnelAddArgs) {
    if !is_valid_tunnel_id(&args.id) {
        eprintln!("invalid tunnel ID '{}': must be lowercase alphanumeric with hyphens", args.id);
        std::process::exit(1);
    }

    let tunnel_type = parse_tunnel_type(&args.tunnel_type);
    let mode = args.mode.as_deref().map(parse_tunnel_mode).unwrap_or(TunnelMode::Auto);

    let tunnel_config = TunnelConfig {
        name: args.name,
        host: args.host,
        port: args.port,
        tunnel_type,
        mode,
        local_port: args.local_port,
        remote_host: args.remote_host,
        remote_port: args.remote_port,
        local_host: args.local_host,
        remote_bind: args.remote_bind,
        identity: args.identity,
        jump_host: args.jump_host,
        jump_port: args.jump_port,
        ssh_binary: args.ssh_binary,
        keepalive: args.keepalive,
    };

    let mut config = load_or_empty();

    if config.tunnel.contains_key(&args.id) {
        eprintln!("tunnel '{}' already exists", args.id);
        std::process::exit(1);
    }

    config.tunnel.insert(args.id.clone(), tunnel_config);

    let result = validate_config(&config);
    if !result.is_ok() {
        for err in &result.errors {
            eprintln!("  - {err}");
        }
        std::process::exit(1);
    }

    save_or_exit(&config);
    println!("tunnel '{}' added", args.id);
    try_daemon_reload();
}

pub fn tunnel_remove(id: &str, force: bool) {
    let mut config = load_or_exit();

    if !config.tunnel.contains_key(id) {
        eprintln!("tunnel '{id}' not found");
        std::process::exit(1);
    }

    if is_tunnel_active(id) && !force {
        eprintln!("tunnel '{id}' is currently connected; use --force to remove");
        std::process::exit(1);
    }

    if force && is_tunnel_active(id) {
        // Disconnect first, ignore errors
        let _ = send_rpc("tunnel.disconnect", json!({ "id": id }));
    }

    config.tunnel.remove(id);
    save_or_exit(&config);
    println!("tunnel '{id}' removed");
    try_daemon_reload();
}

pub fn tunnel_modify(args: TunnelModifyArgs) {
    let mut config = load_or_exit();

    let tunnel = match config.tunnel.get_mut(&args.id) {
        Some(t) => t,
        None => {
            eprintln!("tunnel '{}' not found", args.id);
            std::process::exit(1);
        }
    };

    // Apply each provided field
    if let Some(name) = args.name {
        tunnel.name = name;
    }
    if let Some(host) = args.host {
        tunnel.host = host;
    }
    if let Some(ref tt) = args.tunnel_type {
        tunnel.tunnel_type = parse_tunnel_type(tt);
    }
    if let Some(port) = args.port {
        tunnel.port = Some(port);
    }
    if let Some(ref m) = args.mode {
        tunnel.mode = parse_tunnel_mode(m);
    }
    if let Some(lp) = args.local_port {
        tunnel.local_port = lp;
    }
    if let Some(rh) = args.remote_host {
        tunnel.remote_host = Some(rh);
    }
    if let Some(rp) = args.remote_port {
        tunnel.remote_port = Some(rp);
    }
    if let Some(lh) = args.local_host {
        tunnel.local_host = Some(lh);
    }
    if let Some(rb) = args.remote_bind {
        tunnel.remote_bind = Some(rb);
    }
    if let Some(identity) = args.identity {
        tunnel.identity = Some(identity);
    }
    if let Some(jh) = args.jump_host {
        tunnel.jump_host = Some(jh);
    }
    if let Some(jp) = args.jump_port {
        tunnel.jump_port = Some(jp);
    }
    if let Some(sb) = args.ssh_binary {
        tunnel.ssh_binary = Some(sb);
    }
    if let Some(ka) = args.keepalive {
        tunnel.keepalive = Some(ka);
    }

    let result = validate_config(&config);
    if !result.is_ok() {
        for err in &result.errors {
            eprintln!("  - {err}");
        }
        std::process::exit(1);
    }

    save_or_exit(&config);
    println!("tunnel '{}' modified", args.id);
    try_daemon_reload();
}

pub fn tunnel_show(id: &str) {
    let config = load_or_exit();

    let tunnel = match config.tunnel.get(id) {
        Some(t) => t,
        None => {
            eprintln!("tunnel '{id}' not found");
            std::process::exit(1);
        }
    };

    println!("Tunnel: {id}");
    println!("  Name:        {}", tunnel.name);
    println!("  Host:        {}", tunnel.host);
    if let Some(port) = tunnel.port {
        println!("  Port:        {port}");
    }
    println!("  Type:        {}", tunnel.tunnel_type);
    println!("  Mode:        {}", tunnel.mode);
    println!("  Local port:  {}", tunnel.local_port);
    if let Some(ref v) = tunnel.remote_host {
        println!("  Remote host: {v}");
    }
    if let Some(v) = tunnel.remote_port {
        println!("  Remote port: {v}");
    }
    if let Some(ref v) = tunnel.local_host {
        println!("  Local host:  {v}");
    }
    if let Some(ref v) = tunnel.remote_bind {
        println!("  Remote bind: {v}");
    }
    if let Some(ref v) = tunnel.identity {
        println!("  Identity:    {v}");
    }
    if let Some(ref v) = tunnel.jump_host {
        println!("  Jump host:   {v}");
    }
    if let Some(v) = tunnel.jump_port {
        println!("  Jump port:   {v}");
    }
    if let Some(ref v) = tunnel.ssh_binary {
        println!("  SSH binary:  {v}");
    }
    if let Some(v) = tunnel.keepalive {
        println!("  Keepalive:   {v}");
    }
}

// -- Helpers --

fn parse_tunnel_type(s: &str) -> TunnelType {
    match s {
        "local" => TunnelType::Local,
        "reverse" => TunnelType::Reverse,
        "socks" => TunnelType::Socks,
        other => {
            eprintln!("unknown tunnel type '{other}': expected local, reverse, or socks");
            std::process::exit(1);
        }
    }
}

fn parse_tunnel_mode(s: &str) -> TunnelMode {
    match s {
        "auto" => TunnelMode::Auto,
        "manual" => TunnelMode::Manual,
        "on-demand" => TunnelMode::OnDemand,
        other => {
            eprintln!("unknown tunnel mode '{other}': expected auto, manual, or on-demand");
            std::process::exit(1);
        }
    }
}

/// Load config from disk, returning empty config if file doesn't exist.
fn load_or_empty() -> Config {
    match config::load_config() {
        Ok(cfg) => cfg,
        Err(config::ConfigError::NotFound(_)) => Config {
            defaults: Defaults::default(),
            tunnel: Default::default(),
        },
        Err(e) => {
            eprintln!("failed to load config: {e}");
            std::process::exit(1);
        }
    }
}

/// Load config, exit on any error including not-found.
fn load_or_exit() -> Config {
    match config::load_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("failed to load config: {e}");
            std::process::exit(1);
        }
    }
}

fn save_or_exit(config: &Config) {
    if let Err(e) = config::save_config(config) {
        eprintln!("failed to save config: {e}");
        std::process::exit(1);
    }
}

/// Best-effort: tell the daemon to reload config. Silently ignores
/// connection failures (daemon may not be running).
fn try_daemon_reload() {
    let _ = send_rpc("config.reload", json!({}));
}

/// Check if a tunnel is currently connected or connecting via daemon IPC.
fn is_tunnel_active(id: &str) -> bool {
    match send_rpc("tunnel.get", json!({ "id": id })) {
        Ok(resp) => {
            if let Some(result) = resp.result {
                if let Some(status) = result.get("status").and_then(|v| v.as_str()) {
                    let parsed: Result<TunnelStatus, _> =
                        serde_json::from_value(serde_json::Value::String(status.to_string()));
                    if let Ok(s) = parsed {
                        return s == TunnelStatus::Connected || s == TunnelStatus::Connecting;
                    }
                }
            }
            false
        }
        Err(RpcClientError::Io(_)) => false, // daemon not running
        Err(_) => false,
    }
}
