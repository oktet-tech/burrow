use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;

/// Top-level configuration file structure.
///
/// Maps the TOML layout:
/// ```toml
/// [defaults]
/// ssh_binary = "ssh"
/// keepalive = true
///
/// [tunnel.my-tunnel]
/// name = "..."
/// ...
/// ```
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub tunnel: HashMap<String, TunnelConfig>,
}

#[derive(Debug, Deserialize)]
pub struct Defaults {
    #[serde(default = "default_ssh_binary")]
    pub ssh_binary: String,
    #[serde(default = "default_true")]
    pub keepalive: bool,
    pub log_level: Option<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            ssh_binary: default_ssh_binary(),
            keepalive: true,
            log_level: None,
        }
    }
}

fn default_ssh_binary() -> String {
    "ssh".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelType {
    Local,
    Reverse,
    Socks,
}

impl fmt::Display for TunnelType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Reverse => write!(f, "reverse"),
            Self::Socks => write!(f, "socks"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelMode {
    Auto,
    Manual,
    #[serde(rename = "on-demand")]
    OnDemand,
}

impl Default for TunnelMode {
    fn default() -> Self {
        Self::Auto
    }
}

impl fmt::Display for TunnelMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Manual => write!(f, "manual"),
            Self::OnDemand => write!(f, "on-demand"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TunnelConfig {
    /// Human-readable name (required).
    pub name: String,
    /// SSH host (required).
    pub host: String,
    /// SSH port (default: 22).
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// Tunnel type: local, reverse, or socks (required).
    #[serde(rename = "type")]
    pub tunnel_type: TunnelType,
    /// Tunnel mode (default: auto).
    #[serde(default)]
    pub mode: TunnelMode,

    // -- Port bindings --
    /// Local bind port (required for all types).
    pub local_port: u16,
    /// Target host (required for local forwards).
    pub remote_host: Option<String>,
    /// Target port (required for local and reverse forwards).
    pub remote_port: Option<u16>,
    /// Local bind address for reverse forwards (default: 127.0.0.1).
    pub local_host: Option<String>,
    /// Remote bind address for reverse forwards (default: localhost).
    pub remote_bind: Option<String>,

    // -- SSH options (per-tunnel overrides) --
    /// SSH identity file path (~ is expanded).
    pub identity: Option<String>,
    /// ProxyJump host.
    pub jump_host: Option<String>,
    /// ProxyJump port (default: 22).
    pub jump_port: Option<u16>,
    /// Override SSH binary for this tunnel.
    pub ssh_binary: Option<String>,
    /// Override keepalive setting for this tunnel.
    pub keepalive: Option<bool>,
}

fn default_ssh_port() -> u16 {
    22
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_local_tunnel() {
        let toml_str = r#"
[tunnel.dev-db]
name = "Dev Database"
host = "bastion.example.com"
type = "local"
local_port = 5432
remote_host = "db.internal"
remote_port = 5432
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.tunnel.len(), 1);

        let t = &config.tunnel["dev-db"];
        assert_eq!(t.name, "Dev Database");
        assert_eq!(t.tunnel_type, TunnelType::Local);
        assert_eq!(t.mode, TunnelMode::Auto);
        assert_eq!(t.port, 22);
    }

    #[test]
    fn parse_socks_tunnel() {
        let toml_str = r#"
[tunnel.proxy]
name = "SOCKS Proxy"
host = "home.example.com"
type = "socks"
mode = "on-demand"
local_port = 1080
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let t = &config.tunnel["proxy"];
        assert_eq!(t.tunnel_type, TunnelType::Socks);
        assert_eq!(t.mode, TunnelMode::OnDemand);
        assert!(t.remote_host.is_none());
    }

    #[test]
    fn parse_reverse_tunnel() {
        let toml_str = r#"
[tunnel.expose-api]
name = "Expose Local API"
host = "jumphost.example.com"
type = "reverse"
mode = "manual"
local_port = 8080
local_host = "127.0.0.1"
remote_port = 9000
remote_bind = "0.0.0.0"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let t = &config.tunnel["expose-api"];
        assert_eq!(t.tunnel_type, TunnelType::Reverse);
        assert_eq!(t.remote_bind.as_deref(), Some("0.0.0.0"));
    }

    #[test]
    fn parse_defaults() {
        let toml_str = r#"
[defaults]
ssh_binary = "/usr/local/bin/ssh"
keepalive = false
log_level = "debug"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.defaults.ssh_binary, "/usr/local/bin/ssh");
        assert!(!config.defaults.keepalive);
        assert_eq!(config.defaults.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn empty_config_uses_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.defaults.ssh_binary, "ssh");
        assert!(config.defaults.keepalive);
        assert!(config.tunnel.is_empty());
    }
}
