mod cli;
mod common;
mod config;
mod daemon;
mod ipc;

use clap::Parser;

use cli::{Cli, Commands, ConfigCommand, DaemonCommand, ServiceCommand, TunnelCommand};

fn main() {
    let cli = Cli::parse();

    // First-run: create sample config if it doesn't exist.
    // Skip for commands that handle missing config themselves.
    if !matches!(
        cli.command,
        Some(Commands::Config {
            command: ConfigCommand::Path
        }) | Some(Commands::Config {
            command: ConfigCommand::Edit
        }) | Some(Commands::DaemonForeground)
    ) {
        if let Some(path) = config::sample::ensure_config_exists() {
            println!("Created sample configuration at: {}", path.display());
            println!();
            println!("Edit the config file to add your tunnels, then run:");
            println!("  burrow config reload");
            println!();
            println!("Or add a tunnel with:");
            println!(
                "  burrow tunnel add my-tunnel --name \"My Tunnel\" --host example.com --type local \\"
            );
            println!("    --local-port 5432 --remote-host db.internal --remote-port 5432");
            return;
        }
    }

    match cli.command {
        Some(Commands::Status) => cli::commands::status(),
        Some(Commands::Connect { ref id }) => cli::commands::connect(id),
        Some(Commands::Disconnect { ref id }) => cli::commands::disconnect(id),
        Some(Commands::Enable { ref id }) => cli::commands::enable(id),
        Some(Commands::Disable { ref id }) => cli::commands::disable(id),
        Some(Commands::ConnectAll) => cli::commands::connect_all(),
        Some(Commands::DisconnectAll) => cli::commands::disconnect_all(),
        Some(Commands::RestartAll) => cli::commands::restart_all(),
        Some(Commands::Logs(ref args)) => cli::commands::logs(args),
        Some(Commands::Tunnel { command }) => match command {
            TunnelCommand::Add(args) => cli::tunnel_cmds::tunnel_add(args),
            TunnelCommand::Remove { ref id, force } => cli::tunnel_cmds::tunnel_remove(id, force),
            TunnelCommand::Modify(args) => cli::tunnel_cmds::tunnel_modify(args),
            TunnelCommand::Show { ref id } => cli::tunnel_cmds::tunnel_show(id),
        },
        Some(Commands::Config { command }) => match command {
            ConfigCommand::Path => cli::commands::config_path(),
            ConfigCommand::Edit => cli::commands::config_edit(),
            ConfigCommand::Reload => cli::commands::config_reload(),
        },
        Some(Commands::DaemonForeground) => run_daemon_foreground(),
        Some(Commands::Daemon { command }) => match command {
            DaemonCommand::Start => cli::commands::daemon_start(),
            DaemonCommand::Stop => cli::commands::daemon_stop(),
            DaemonCommand::Restart => cli::commands::daemon_restart(),
            DaemonCommand::Status => cli::commands::daemon_status(),
        },
        Some(Commands::Service { command }) => match command {
            ServiceCommand::Install => cli::service::service_install(),
            ServiceCommand::Uninstall => cli::service::service_uninstall(),
        },
        None => {
            println!("No command specified. Use 'burrow --help' for usage.");
        }
    }
}

/// Hidden entry point for the spawned daemon process.
fn run_daemon_foreground() {
    common::logging::init_logging();

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

    if let Err(e) = rt.block_on(daemon::run()) {
        tracing::error!(error = %e, "daemon exited with error");
        std::process::exit(1);
    }
}
