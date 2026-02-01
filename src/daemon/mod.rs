pub mod manager;
pub mod network;
pub mod server;
pub mod state;
pub mod stub;
pub mod tunnel;

use std::path::{Path, PathBuf};

use crate::config;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("invalid configuration: {0}")]
    Config(String),
}

/// Platform-appropriate IPC socket path.
///
/// macOS: ~/Library/Application Support/Burrow/burrow.sock
/// Linux: $XDG_RUNTIME_DIR/burrow.sock (fallback: ~/.config/burrow/burrow.sock)
pub fn socket_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        directories::BaseDirs::new()
            .expect("cannot determine home directory")
            .home_dir()
            .join("Library/Application Support/Burrow/burrow.sock")
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("burrow.sock")
        } else {
            directories::ProjectDirs::from("", "", "burrow")
                .expect("cannot determine runtime directory")
                .config_dir()
                .join("burrow.sock")
        }
    }
}

/// Remove stale socket file. Errors if a daemon is already running.
fn cleanup_stale_socket(path: &Path) -> Result<(), DaemonError> {
    if !path.exists() {
        return Ok(());
    }

    // Try connecting — if it succeeds, another daemon owns this socket
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("daemon already running (socket: {})", path.display()),
        )
        .into()),
        Err(_) => {
            tracing::info!("removing stale socket: {}", path.display());
            std::fs::remove_file(path)?;
            Ok(())
        }
    }
}

/// Start the daemon. Blocks until shutdown is requested via IPC.
pub async fn run() -> Result<(), DaemonError> {
    let path = socket_path();

    cleanup_stale_socket(&path)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Load config -- missing config is fine, we just start with zero tunnels.
    // Validation or parse errors are fatal: fail fast so the user sees the problem.
    let mgr = manager::TunnelManager::new();
    match config::load_config() {
        Ok(cfg) => mgr.load_tunnels(&cfg).await,
        Err(config::ConfigError::NotFound(_)) => {
            tracing::info!("no config file found, starting with no tunnels");
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load config");
            return Err(DaemonError::Config(e.to_string()));
        }
    }

    // Restore persisted state (enabled flags, accumulated stats)
    let persisted = state::load_state();
    mgr.apply_state(&persisted).await;

    // Auto-save state on changes (debounced)
    let state_file = state::state_path();
    mgr.enable_state_persistence(state_file.clone());

    // Connect tunnels with mode=auto and enabled=true
    mgr.connect_auto_tunnels().await;

    // Bind stub listeners for on-demand tunnels
    mgr.start_on_demand_stubs().await;

    // Monitor network changes to recover tunnels quickly
    network::spawn_network_monitor(mgr.clone());

    tracing::info!("starting daemon, socket: {}", path.display());

    let result = server::run(&path, mgr.clone()).await;

    // Final state save before exit
    mgr.save_state_now(&state_file).await;

    // Always clean up socket on exit
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    tracing::info!("daemon stopped");

    result
}
