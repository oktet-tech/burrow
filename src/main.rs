mod cli;
mod common;
mod config;
mod daemon;
mod gui;
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
        }) | Some(Commands::Config {
            command: ConfigCommand::Validate
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
            ConfigCommand::Validate => cli::commands::config_validate(),
            ConfigCommand::Reload => cli::commands::config_reload(),
        },
        Some(Commands::Gui) => {
            common::logging::init_logging(None);
            gui::launch()
        }
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
            // Default to GUI when run without arguments (e.g., from macOS bundle)
            common::logging::init_logging(None);
            gui::launch()
        }
    }
}

/// Hidden entry point for the spawned daemon process.
fn run_daemon_foreground() {
    let broadcast = common::log_broadcast::LogBroadcast::new(512);
    common::logging::init_logging(Some(&broadcast));

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

    let result = rt.block_on(daemon::run(broadcast));
    // process::exit skips destructors; drop the runtime first so any
    // remaining tasks drop their Child handles and kill SSH.
    drop(rt);
    if let Err(e) = result {
        tracing::error!(error = %e, "daemon exited with error");
        std::process::exit(1);
    }
}
