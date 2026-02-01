use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use crate::config::schema::Config;
use crate::ipc::protocol::{TunnelInfo, TunnelStatus};

use super::tunnel::{read_stderr, Tunnel};

struct ManagedTunnel {
    tunnel: Tunnel,
    /// Handle to the task monitoring SSH process exit. Aborting it kills SSH
    /// via kill_on_drop on the Child held inside the task's future.
    monitor: Option<JoinHandle<()>>,
}

struct ExitEvent {
    id: String,
    code: Option<i32>,
    stderr: Option<String>,
}

struct Inner {
    tunnels: HashMap<String, ManagedTunnel>,
    exit_tx: mpsc::UnboundedSender<ExitEvent>,
}

/// Manages all tunnel lifecycles. Cheaply cloneable (Arc wrapper).
#[derive(Clone)]
pub struct TunnelManager {
    inner: Arc<Mutex<Inner>>,
}

impl TunnelManager {
    pub fn new() -> Self {
        let (exit_tx, exit_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Mutex::new(Inner {
            tunnels: HashMap::new(),
            exit_tx,
        }));

        // Background task: process tunnel exit events
        Self::spawn_exit_handler(Arc::clone(&inner), exit_rx);

        Self { inner }
    }

    /// Populate tunnels from config. Does not start any — that's the caller's job.
    pub async fn load_tunnels(&self, config: &Config) {
        let mut state = self.inner.lock().await;
        for (id, tunnel_config) in &config.tunnel {
            let tunnel = Tunnel::new(id.clone(), tunnel_config.clone(), &config.defaults);
            state.tunnels.insert(
                id.clone(),
                ManagedTunnel {
                    tunnel,
                    monitor: None,
                },
            );
        }
        tracing::info!(count = state.tunnels.len(), "loaded tunnels from config");
    }

    /// Start the SSH process for a tunnel.
    pub async fn connect(&self, id: &str) -> Result<(), String> {
        let mut state = self.inner.lock().await;

        // Clone sender before taking mutable ref to the tunnel entry
        let exit_tx = state.exit_tx.clone();

        let mt = state
            .tunnels
            .get_mut(id)
            .ok_or_else(|| format!("tunnel '{id}' not found"))?;

        if mt.tunnel.status == TunnelStatus::Connected
            || mt.tunnel.status == TunnelStatus::Connecting
        {
            return Err(format!("tunnel '{id}' is already connected"));
        }

        let mut child = mt
            .tunnel
            .start()
            .map_err(|e| format!("failed to start tunnel '{id}': {e}"))?;

        // Spawn monitor task: waits for SSH exit, sends event back
        let stderr_handle = child.stderr.take();
        let tunnel_id = id.to_string();

        let handle = tokio::spawn(async move {
            let wait_result = child.wait().await;
            let stderr = read_stderr(stderr_handle).await;
            let code = wait_result.ok().and_then(|s| s.code());
            let _ = exit_tx.send(ExitEvent {
                id: tunnel_id,
                code,
                stderr,
            });
        });

        mt.monitor = Some(handle);
        Ok(())
    }

    /// Kill the SSH process for a tunnel.
    pub async fn disconnect(&self, id: &str) -> Result<(), String> {
        let mut state = self.inner.lock().await;
        let mt = state
            .tunnels
            .get_mut(id)
            .ok_or_else(|| format!("tunnel '{id}' not found"))?;

        if let Some(handle) = mt.monitor.take() {
            // Aborting the monitor task drops the Child, which sends SIGKILL
            handle.abort();
        }

        mt.tunnel.record_disconnect();
        Ok(())
    }

    /// Snapshot of all tunnels for IPC responses.
    pub async fn list(&self) -> Vec<TunnelInfo> {
        let state = self.inner.lock().await;
        let mut infos: Vec<TunnelInfo> = state
            .tunnels
            .values()
            .map(|mt| mt.tunnel.to_info())
            .collect();
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        infos
    }

    /// Snapshot of a single tunnel.
    pub async fn get(&self, id: &str) -> Option<TunnelInfo> {
        let state = self.inner.lock().await;
        state.tunnels.get(id).map(|mt| mt.tunnel.to_info())
    }

    pub async fn tunnel_count(&self) -> usize {
        self.inner.lock().await.tunnels.len()
    }

    fn spawn_exit_handler(
        inner: Arc<Mutex<Inner>>,
        mut exit_rx: mpsc::UnboundedReceiver<ExitEvent>,
    ) {
        tokio::spawn(async move {
            while let Some(event) = exit_rx.recv().await {
                let mut state = inner.lock().await;
                if let Some(mt) = state.tunnels.get_mut(&event.id) {
                    mt.monitor = None;
                    mt.tunnel.record_exit(event.code, event.stderr);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{Defaults, TunnelConfig, TunnelMode, TunnelType};

    fn test_config() -> Config {
        let mut tunnel = HashMap::new();
        tunnel.insert(
            "dev-db".to_string(),
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
                identity: None,
                jump_host: None,
                jump_port: None,
                ssh_binary: None,
                keepalive: None,
            },
        );
        tunnel.insert(
            "proxy".to_string(),
            TunnelConfig {
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
            },
        );
        Config {
            defaults: Defaults::default(),
            tunnel,
        }
    }

    #[tokio::test]
    async fn load_populates_tunnels() {
        let mgr = TunnelManager::new();
        mgr.load_tunnels(&test_config()).await;

        assert_eq!(mgr.tunnel_count().await, 2);
        assert!(mgr.get("dev-db").await.is_some());
        assert!(mgr.get("proxy").await.is_some());
        assert!(mgr.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn list_returns_sorted() {
        let mgr = TunnelManager::new();
        mgr.load_tunnels(&test_config()).await;

        let list = mgr.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "dev-db");
        assert_eq!(list[1].id, "proxy");
    }

    #[tokio::test]
    async fn all_tunnels_start_disconnected() {
        let mgr = TunnelManager::new();
        mgr.load_tunnels(&test_config()).await;

        for info in mgr.list().await {
            assert_eq!(info.status, TunnelStatus::Disconnected);
        }
    }

    #[tokio::test]
    async fn connect_unknown_tunnel_fails() {
        let mgr = TunnelManager::new();
        let result = mgr.connect("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn disconnect_unknown_tunnel_fails() {
        let mgr = TunnelManager::new();
        let result = mgr.disconnect("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn connect_with_bad_binary_sets_error() {
        let mut config = test_config();
        config
            .tunnel
            .get_mut("dev-db")
            .unwrap()
            .ssh_binary = Some("/nonexistent/ssh".into());

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;

        let result = mgr.connect("dev-db").await;
        assert!(result.is_err());

        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Error);
    }

    #[tokio::test]
    async fn connect_then_disconnect() {
        let mut config = test_config();
        // Use `sleep 60` as a long-running "SSH" process
        config
            .tunnel
            .get_mut("dev-db")
            .unwrap()
            .ssh_binary = Some("sleep".into());
        config
            .tunnel
            .get_mut("dev-db")
            .unwrap()
            .host = "60".into(); // sleep argument (last arg = host)

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;

        mgr.connect("dev-db").await.unwrap();
        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Connected);

        mgr.disconnect("dev-db").await.unwrap();
        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Disconnected);
    }

    #[tokio::test]
    async fn exit_detection() {
        let mut config = test_config();
        // `false` exits immediately with code 1
        config
            .tunnel
            .get_mut("dev-db")
            .unwrap()
            .ssh_binary = Some("false".into());

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;

        mgr.connect("dev-db").await.unwrap();

        // Give the exit handler a moment to process
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Error);
        assert!(info.last_error.is_some());
    }
}
