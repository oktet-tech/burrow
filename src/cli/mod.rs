pub mod commands;

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
    /// Manage the background daemon
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Run daemon in foreground (internal)
    #[command(hide = true, name = "daemon-foreground")]
    DaemonForeground,
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
