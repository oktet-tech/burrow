mod cli;
mod config;
mod daemon;
mod ipc;

use clap::Parser;

use cli::{Cli, Commands, DaemonCommand};

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Status) => cli::commands::status(),
        Some(Commands::Connect { ref id }) => cli::commands::connect(id),
        Some(Commands::Disconnect { ref id }) => cli::commands::disconnect(id),
        Some(Commands::DaemonForeground) => run_daemon_foreground(),
        Some(Commands::Daemon { command }) => match command {
            DaemonCommand::Start => cli::commands::daemon_start(),
            DaemonCommand::Stop => cli::commands::daemon_stop(),
            DaemonCommand::Restart => cli::commands::daemon_restart(),
            DaemonCommand::Status => cli::commands::daemon_status(),
        },
        None => {
            println!("No command specified. Use 'burrow --help' for usage.");
        }
    }
}

/// Hidden entry point for the spawned daemon process.
fn run_daemon_foreground() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BURROW_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

    if let Err(e) = rt.block_on(daemon::run()) {
        tracing::error!(error = %e, "daemon exited with error");
        std::process::exit(1);
    }
}
