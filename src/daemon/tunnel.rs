use std::process::Stdio;
use std::time::Instant;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::config::schema::{Defaults, TunnelConfig, TunnelMode, TunnelType};
use crate::ipc::protocol::{TunnelInfo, TunnelStats, TunnelStatus};

pub struct Tunnel {
    pub id: String,
    config: TunnelConfig,
    ssh_binary: String,
    keepalive: bool,
    pub status: TunnelStatus,
    pub last_error: Option<String>,
    pub pid: Option<u32>,
    pub enabled: bool,
    pub last_connected: Option<String>,
    pub total_connections: u64,
    pub total_uptime_seconds: u64,
    pub reconnect_count: u64,
    /// Monotonic clock reference for calculating session uptime.
    session_start: Option<Instant>,
}

impl Tunnel {
    pub fn new(id: String, config: TunnelConfig, defaults: &Defaults) -> Self {
        let ssh_binary = config
            .ssh_binary
            .clone()
            .unwrap_or_else(|| defaults.ssh_binary.clone());
        let keepalive = config.keepalive.unwrap_or(defaults.keepalive);

        Self {
            id,
            config,
            ssh_binary,
            keepalive,
            status: TunnelStatus::Disconnected,
            last_error: None,
            pid: None,
            enabled: true,
            last_connected: None,
            total_connections: 0,
            total_uptime_seconds: 0,
            reconnect_count: 0,
            session_start: None,
        }
    }

    pub fn config(&self) -> &TunnelConfig {
        &self.config
    }

    /// Replace tunnel config and re-derive ssh_binary/keepalive.
    pub fn update_config(&mut self, config: TunnelConfig, defaults: &Defaults) {
        self.ssh_binary = config
            .ssh_binary
            .clone()
            .unwrap_or_else(|| defaults.ssh_binary.clone());
        self.keepalive = config.keepalive.unwrap_or(defaults.keepalive);
        self.config = config;
    }

    /// Build SSH command-line arguments per DESIGN.md.
    ///
    /// Always includes: -N, -o ExitOnForwardFailure=yes
    /// Keepalive adds: -o ServerAliveInterval=30, -o ServerAliveCountMax=3
    /// Forward flag depends on tunnel type: -L (local), -R (reverse), -D (socks)
    pub fn build_ssh_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        args.push("-N".into());

        args.push("-o".into());
        args.push("ExitOnForwardFailure=yes".into());

        if self.keepalive {
            args.push("-o".into());
            args.push("ServerAliveInterval=30".into());
            args.push("-o".into());
            args.push("ServerAliveCountMax=3".into());
        }

        // Forward specification
        match self.config.tunnel_type {
            TunnelType::Local => {
                let rhost = self.config.remote_host.as_deref().unwrap_or("localhost");
                let rport = self.config.remote_port.unwrap_or(0);
                args.push("-L".into());
                args.push(format!("127.0.0.1:{}:{}:{}", self.config.local_port, rhost, rport));
            }
            TunnelType::Reverse => {
                let rbind = self.config.remote_bind.as_deref().unwrap_or("localhost");
                let rport = self.config.remote_port.unwrap_or(0);
                let lhost = self.config.local_host.as_deref().unwrap_or("127.0.0.1");
                args.push("-R".into());
                args.push(format!("{}:{}:{}:{}", rbind, rport, lhost, self.config.local_port));
            }
            TunnelType::Socks => {
                args.push("-D".into());
                args.push(format!("127.0.0.1:{}", self.config.local_port));
            }
        }

        if let Some(ref identity) = self.config.identity {
            args.push("-i".into());
            args.push(identity.clone());
        }

        if let Some(ref jump_host) = self.config.jump_host {
            args.push("-J".into());
            match self.config.jump_port {
                Some(p) if p != 22 => args.push(format!("{jump_host}:{p}")),
                _ => args.push(jump_host.clone()),
            }
        }

        args.push("-p".into());
        args.push(self.config.port.to_string());

        args.push(self.config.host.clone());

        args
    }

    /// Start the SSH process and return the Child handle for external monitoring.
    /// Sets status to Connected. Caller is responsible for watching the child.
    pub fn start(&mut self) -> Result<tokio::process::Child, std::io::Error> {
        self.status = TunnelStatus::Connecting;
        self.last_error = None;

        let args = self.build_ssh_args();

        tracing::info!(
            tunnel_id = %self.id,
            "spawning: {} {}",
            self.ssh_binary,
            args.join(" ")
        );

        let child = match Command::new(&self.ssh_binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                self.status = TunnelStatus::Error;
                self.last_error = Some(format!("failed to spawn SSH: {e}"));
                return Err(e);
            }
        };

        self.pid = child.id();
        self.status = TunnelStatus::Connected;
        self.total_connections += 1;
        self.session_start = Some(Instant::now());
        self.last_connected = Some(chrono::Utc::now().to_rfc3339());
        tracing::info!(tunnel_id = %self.id, pid = ?self.pid, "SSH process started");

        Ok(child)
    }

    /// Record that the SSH process exited. Any exit is unexpected for -N tunnels.
    pub fn record_exit(&mut self, code: Option<i32>, stderr: Option<String>) {
        self.pid = None;
        self.status = TunnelStatus::Error;
        if let Some(start) = self.session_start.take() {
            self.total_uptime_seconds += start.elapsed().as_secs();
        }
        self.last_error = Some(match (code, &stderr) {
            (Some(c), Some(msg)) => format!("exited with code {c}: {msg}"),
            (Some(c), None) => format!("exited with code {c}"),
            (None, Some(msg)) => format!("killed by signal: {msg}"),
            (None, None) => "killed by signal".into(),
        });
        tracing::warn!(tunnel_id = %self.id, error = ?self.last_error, "SSH process exited");
    }

    /// Whether this tunnel should auto-reconnect after an unexpected exit.
    pub fn should_reconnect(&self) -> bool {
        self.enabled && self.config.mode == TunnelMode::Auto && self.status == TunnelStatus::Error
    }

    /// Mark as disconnected (user-initiated stop).
    pub fn record_disconnect(&mut self) {
        self.pid = None;
        self.status = TunnelStatus::Disconnected;
        self.last_error = None;
        if let Some(start) = self.session_start.take() {
            self.total_uptime_seconds += start.elapsed().as_secs();
        }
    }

    /// Seconds elapsed in the current session, or 0 if not connected.
    pub fn session_start_elapsed(&self) -> u64 {
        self.session_start.map_or(0, |s| s.elapsed().as_secs())
    }

    /// Build a TunnelInfo snapshot for IPC responses.
    pub fn to_info(&self) -> TunnelInfo {
        let remote = match self.config.tunnel_type {
            TunnelType::Local => {
                let rh = self.config.remote_host.as_deref().unwrap_or("localhost");
                let rp = self.config.remote_port.unwrap_or(0);
                Some(format!("{rh}:{rp}"))
            }
            TunnelType::Reverse => {
                let rb = self.config.remote_bind.as_deref().unwrap_or("localhost");
                let rp = self.config.remote_port.unwrap_or(0);
                Some(format!("{rb}:{rp}"))
            }
            TunnelType::Socks => Some("SOCKS5".into()),
        };

        TunnelInfo {
            id: self.id.clone(),
            name: self.config.name.clone(),
            tunnel_type: self.config.tunnel_type.to_string(),
            mode: self.config.mode.to_string(),
            status: self.status,
            local_port: self.config.local_port,
            remote,
            host: self.config.host.clone(),
            enabled: self.enabled,
            last_error: self.last_error.clone(),
            stats: Some(TunnelStats {
                total_connections: self.total_connections,
                current_session_start: if self.status == TunnelStatus::Connected {
                    self.last_connected.clone()
                } else {
                    None
                },
                total_uptime_seconds: self.total_uptime_seconds
                    + self.session_start_elapsed(),
                reconnect_count: self.reconnect_count,
            }),
        }
    }

    /// Spawn SSH process, wait for it to exit, update status.
    /// Convenience method for standalone use; the manager uses start() + monitor instead.
    pub async fn spawn(&mut self) {
        let mut child = match self.start() {
            Ok(c) => c,
            Err(e) => {
                self.status = TunnelStatus::Error;
                self.last_error = Some(format!("failed to spawn SSH: {e}"));
                tracing::error!(tunnel_id = %self.id, error = %e, "spawn failed");
                return;
            }
        };

        let stderr_handle = child.stderr.take();
        let wait_result = child.wait().await;
        let stderr_msg = read_stderr(stderr_handle).await;

        match wait_result {
            Ok(status) => self.record_exit(status.code(), stderr_msg),
            Err(e) => {
                self.status = TunnelStatus::Error;
                self.last_error = Some(format!("wait failed: {e}"));
            }
        }
    }
}

pub(super) async fn read_stderr(handle: Option<tokio::process::ChildStderr>) -> Option<String> {
    let mut stderr = handle?;
    let mut buf = String::new();
    // Process has already exited, so this reads buffered output then hits EOF
    let _ = stderr.read_to_string(&mut buf).await;
    let trimmed = buf.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{Defaults, TunnelConfig, TunnelMode, TunnelType};

    fn defaults() -> Defaults {
        Defaults::default()
    }

    fn local_config() -> TunnelConfig {
        TunnelConfig {
            name: "Dev Database".into(),
            host: "bastion.example.com".into(),
            port: 22,
            tunnel_type: TunnelType::Local,
            mode: TunnelMode::Auto,
            local_port: 5432,
            remote_host: Some("db.internal".into()),
            remote_port: Some(5432),
            local_host: None,
            remote_bind: None,
            identity: Some("/home/user/.ssh/work_key".into()),
            jump_host: Some("gateway.example.com".into()),
            jump_port: None,
            ssh_binary: None,
            keepalive: None,
        }
    }

    #[test]
    fn local_forward_args() {
        let t = Tunnel::new("dev-db".into(), local_config(), &defaults());
        let args = t.build_ssh_args();

        assert!(args.contains(&"-N".to_string()));
        assert!(args.contains(&"ExitOnForwardFailure=yes".to_string()));
        assert!(args.contains(&"ServerAliveInterval=30".to_string()));
        assert!(args.contains(&"-L".to_string()));
        assert!(args.contains(&"127.0.0.1:5432:db.internal:5432".to_string()));
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"/home/user/.ssh/work_key".to_string()));
        assert!(args.contains(&"-J".to_string()));
        assert!(args.contains(&"gateway.example.com".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"22".to_string()));
        // Host must be the last argument
        assert_eq!(args.last().unwrap(), "bastion.example.com");
    }

    #[test]
    fn reverse_forward_args() {
        let config = TunnelConfig {
            name: "Expose API".into(),
            host: "jumphost.example.com".into(),
            port: 22,
            tunnel_type: TunnelType::Reverse,
            mode: TunnelMode::Manual,
            local_port: 8080,
            remote_host: None,
            remote_port: Some(9000),
            local_host: Some("127.0.0.1".into()),
            remote_bind: Some("0.0.0.0".into()),
            identity: None,
            jump_host: None,
            jump_port: None,
            ssh_binary: None,
            keepalive: None,
        };
        let t = Tunnel::new("expose-api".into(), config, &defaults());
        let args = t.build_ssh_args();

        assert!(args.contains(&"-R".to_string()));
        assert!(args.contains(&"0.0.0.0:9000:127.0.0.1:8080".to_string()));
        assert!(!args.iter().any(|a| a == "-L" || a == "-D"));
        assert_eq!(args.last().unwrap(), "jumphost.example.com");
    }

    #[test]
    fn socks_forward_args() {
        let config = TunnelConfig {
            name: "Home SOCKS".into(),
            host: "home.example.com".into(),
            port: 22,
            tunnel_type: TunnelType::Socks,
            mode: TunnelMode::OnDemand,
            local_port: 1080,
            remote_host: None,
            remote_port: None,
            local_host: None,
            remote_bind: None,
            identity: None,
            jump_host: None,
            jump_port: None,
            ssh_binary: None,
            keepalive: None,
        };
        let t = Tunnel::new("proxy".into(), config, &defaults());
        let args = t.build_ssh_args();

        assert!(args.contains(&"-D".to_string()));
        assert!(args.contains(&"127.0.0.1:1080".to_string()));
        assert!(!args.iter().any(|a| a == "-L" || a == "-R"));
    }

    #[test]
    fn keepalive_disabled() {
        let mut config = local_config();
        config.keepalive = Some(false);
        let t = Tunnel::new("t".into(), config, &defaults());
        let args = t.build_ssh_args();

        assert!(!args.contains(&"ServerAliveInterval=30".to_string()));
        assert!(!args.contains(&"ServerAliveCountMax=3".to_string()));
    }

    #[test]
    fn custom_ssh_binary() {
        let mut config = local_config();
        config.ssh_binary = Some("/usr/local/bin/ssh".into());
        let t = Tunnel::new("t".into(), config, &defaults());
        assert_eq!(t.ssh_binary, "/usr/local/bin/ssh");
    }

    #[test]
    fn jump_host_with_custom_port() {
        let mut config = local_config();
        config.jump_host = Some("gateway.example.com".into());
        config.jump_port = Some(2222);
        let t = Tunnel::new("t".into(), config, &defaults());
        let args = t.build_ssh_args();

        assert!(args.contains(&"gateway.example.com:2222".to_string()));
    }

    #[test]
    fn reverse_uses_defaults_for_bind_addresses() {
        let config = TunnelConfig {
            name: "Rev".into(),
            host: "h.example.com".into(),
            port: 22,
            tunnel_type: TunnelType::Reverse,
            mode: TunnelMode::Auto,
            local_port: 3000,
            remote_host: None,
            remote_port: Some(4000),
            local_host: None,  // should default to 127.0.0.1
            remote_bind: None, // should default to localhost
            identity: None,
            jump_host: None,
            jump_port: None,
            ssh_binary: None,
            keepalive: None,
        };
        let t = Tunnel::new("rev".into(), config, &defaults());
        let args = t.build_ssh_args();

        assert!(args.contains(&"localhost:4000:127.0.0.1:3000".to_string()));
    }

    #[test]
    fn to_info_local_tunnel() {
        let t = Tunnel::new("dev-db".into(), local_config(), &defaults());
        let info = t.to_info();
        assert_eq!(info.tunnel_type, "local");
        assert_eq!(info.remote.as_deref(), Some("db.internal:5432"));
        assert_eq!(info.local_port, 5432);
    }

    #[test]
    fn to_info_reverse_tunnel() {
        let config = TunnelConfig {
            name: "Expose API".into(),
            host: "jump.example.com".into(),
            port: 22,
            tunnel_type: TunnelType::Reverse,
            mode: TunnelMode::Manual,
            local_port: 8080,
            remote_host: None,
            remote_port: Some(9000),
            local_host: None,
            remote_bind: Some("0.0.0.0".into()),
            identity: None,
            jump_host: None,
            jump_port: None,
            ssh_binary: None,
            keepalive: None,
        };
        let t = Tunnel::new("expose-api".into(), config, &defaults());
        let info = t.to_info();
        assert_eq!(info.tunnel_type, "reverse");
        assert_eq!(info.remote.as_deref(), Some("0.0.0.0:9000"));
    }

    #[test]
    fn to_info_socks_tunnel() {
        let config = TunnelConfig {
            name: "SOCKS Proxy".into(),
            host: "home.example.com".into(),
            port: 22,
            tunnel_type: TunnelType::Socks,
            mode: TunnelMode::OnDemand,
            local_port: 1080,
            remote_host: None,
            remote_port: None,
            local_host: None,
            remote_bind: None,
            identity: None,
            jump_host: None,
            jump_port: None,
            ssh_binary: None,
            keepalive: None,
        };
        let t = Tunnel::new("proxy".into(), config, &defaults());
        let info = t.to_info();
        assert_eq!(info.tunnel_type, "socks");
        assert_eq!(info.remote.as_deref(), Some("SOCKS5"));
    }

    #[test]
    fn initial_status_is_disconnected() {
        let t = Tunnel::new("t".into(), local_config(), &defaults());
        assert_eq!(t.status, TunnelStatus::Disconnected);
        assert!(t.last_error.is_none());
        assert!(t.pid.is_none());
    }

    #[tokio::test]
    async fn spawn_nonexistent_binary_sets_error() {
        let mut config = local_config();
        config.ssh_binary = Some("/nonexistent/ssh".into());
        let mut t = Tunnel::new("t".into(), config, &defaults());

        t.spawn().await;

        assert_eq!(t.status, TunnelStatus::Error);
        assert!(t.last_error.as_ref().unwrap().contains("failed to spawn SSH"));
    }

    #[tokio::test]
    async fn spawn_false_sets_error_with_exit_code() {
        // `false` exits immediately with code 1 -- simulates SSH failure
        let mut config = local_config();
        config.ssh_binary = Some("false".into());
        let mut t = Tunnel::new("t".into(), config, &defaults());

        t.spawn().await;

        assert_eq!(t.status, TunnelStatus::Error);
        assert!(t.last_error.as_ref().unwrap().contains("exited with code 1"));
        assert!(t.pid.is_none());
    }
}
