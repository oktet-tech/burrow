pub mod commands;
pub mod service;
pub mod tunnel_cmds;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "burrow", version, about = "SSH tunnel manager")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Show tunnel status
    Status,
    /// Connect a tunnel
    Connect {
        /// Tunnel ID
        id: String,
    },
    /// Disconnect a tunnel
    Disconnect {
        /// Tunnel ID
        id: String,
    },
    /// Enable a tunnel (auto-connects if mode is auto)
    Enable {
        /// Tunnel ID
        id: String,
    },
    /// Disable a tunnel (disconnects if connected)
    Disable {
        /// Tunnel ID
        id: String,
    },
    /// Connect all enabled tunnels
    ConnectAll,
    /// Disconnect all connected tunnels
    DisconnectAll,
    /// Restart all enabled tunnels
    RestartAll,
    /// View daemon logs
    Logs(LogsArgs),
    /// Manage tunnel configuration
    Tunnel {
        #[command(subcommand)]
        command: TunnelCommand,
    },
    /// Manage config file
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Manage the background daemon
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Install or uninstall as a system service
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Run daemon in foreground (internal)
    #[command(hide = true, name = "daemon-foreground")]
    DaemonForeground,
}

#[derive(Debug, Subcommand)]
pub enum TunnelCommand {
    /// Add a new tunnel to the config
    Add(TunnelAddArgs),
    /// Remove a tunnel from the config
    Remove {
        /// Tunnel ID
        id: String,
        /// Remove even if tunnel is connected
        #[arg(long)]
        force: bool,
    },
    /// Modify an existing tunnel's configuration
    Modify(TunnelModifyArgs),
    /// Show details of a tunnel
    Show {
        /// Tunnel ID
        id: String,
    },
}

#[derive(Debug, clap::Args)]
pub struct TunnelAddArgs {
    /// Tunnel ID (lowercase alphanumeric with hyphens)
    pub id: String,
    /// Human-readable name
    #[arg(long)]
    pub name: String,
    /// SSH host
    #[arg(long)]
    pub host: String,
    /// Tunnel type: local, reverse, or socks
    #[arg(long = "type")]
    pub tunnel_type: String,
    /// Local bind port
    #[arg(long)]
    pub local_port: u16,
    /// SSH port (default: 22)
    #[arg(long)]
    pub port: Option<u16>,
    /// Tunnel mode: auto, manual, or on-demand
    #[arg(long)]
    pub mode: Option<String>,
    /// Remote target host (required for local)
    #[arg(long)]
    pub remote_host: Option<String>,
    /// Remote target port (required for local and reverse)
    #[arg(long)]
    pub remote_port: Option<u16>,
    /// Local bind address
    #[arg(long)]
    pub local_host: Option<String>,
    /// Remote bind address (for reverse)
    #[arg(long)]
    pub remote_bind: Option<String>,
    /// SSH identity file
    #[arg(long)]
    pub identity: Option<String>,
    /// ProxyJump host
    #[arg(long)]
    pub jump_host: Option<String>,
    /// ProxyJump port
    #[arg(long)]
    pub jump_port: Option<u16>,
    /// Override SSH binary
    #[arg(long)]
    pub ssh_binary: Option<String>,
    /// Override keepalive setting
    #[arg(long)]
    pub keepalive: Option<bool>,
}

#[derive(Debug, clap::Args)]
pub struct TunnelModifyArgs {
    /// Tunnel ID
    pub id: String,
    /// Human-readable name
    #[arg(long)]
    pub name: Option<String>,
    /// SSH host
    #[arg(long)]
    pub host: Option<String>,
    /// Tunnel type: local, reverse, or socks
    #[arg(long = "type")]
    pub tunnel_type: Option<String>,
    /// Local bind port
    #[arg(long)]
    pub local_port: Option<u16>,
    /// SSH port
    #[arg(long)]
    pub port: Option<u16>,
    /// Tunnel mode: auto, manual, or on-demand
    #[arg(long)]
    pub mode: Option<String>,
    /// Remote target host
    #[arg(long)]
    pub remote_host: Option<String>,
    /// Remote target port
    #[arg(long)]
    pub remote_port: Option<u16>,
    /// Local bind address
    #[arg(long)]
    pub local_host: Option<String>,
    /// Remote bind address
    #[arg(long)]
    pub remote_bind: Option<String>,
    /// SSH identity file
    #[arg(long)]
    pub identity: Option<String>,
    /// ProxyJump host
    #[arg(long)]
    pub jump_host: Option<String>,
    /// ProxyJump port
    #[arg(long)]
    pub jump_port: Option<u16>,
    /// Override SSH binary
    #[arg(long)]
    pub ssh_binary: Option<String>,
    /// Override keepalive setting
    #[arg(long)]
    pub keepalive: Option<bool>,
}

#[derive(Debug, clap::Args)]
pub struct LogsArgs {
    /// Continuously follow new log output
    #[arg(long, short)]
    pub follow: bool,
    /// Filter logs by tunnel ID
    #[arg(long)]
    pub tunnel: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print config file path
    Path,
    /// Open config file in $EDITOR
    Edit,
    /// Reload config file in the running daemon
    Reload,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon in background
    Start,
    /// Stop the running daemon
    Stop,
    /// Restart the daemon
    Restart,
    /// Show daemon status
    Status,
}

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Install as a login service (auto-start on login)
    Install,
    /// Uninstall the login service
    Uninstall,
}
