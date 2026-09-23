use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use indexmap::IndexMap;
use tokio::sync::{broadcast, mpsc, Mutex, Notify};
use tokio::task::JoinHandle;

use crate::config::schema::{Config, TunnelMode};
use crate::ipc::protocol::{BulkResult, ReloadResult, TunnelInfo, TunnelStatus};

use super::error::TunnelError;
use super::monitor::{Monitor, MonitorEvent};
use super::state::{self, PersistedTunnel, PersistedTunnelStats, State};
use super::stub;
use super::tunnel::Tunnel;

const MAX_BACKOFF_SECS: u64 = 300;
/// A session that stays up this long counts as healthy and resets backoff.
const STABLE_SESSION_SECS: u64 = 60;

struct ManagedTunnel {
    tunnel: Tunnel,
    /// Supervisor of the running SSH process. Dropping it kills SSH.
    monitor: Option<Monitor>,
    /// Handle to a pending reconnection timer. Aborting cancels reconnect.
    reconnect_task: Option<JoinHandle<()>>,
    /// Consecutive short-lived sessions. Drives backoff.
    consecutive_failures: u32,
    /// Bumped on every spawn and disconnect so exit events from an older
    /// SSH process can't clobber the current one.
    generation: u64,
    /// On-demand stub listener. Drop aborts and releases the port.
    stub_handle: Option<stub::StubHandle>,
    /// When the pending reconnect or stub re-bind fires, for display.
    next_retry_at: Option<SystemTime>,
}

impl ManagedTunnel {
    fn new(tunnel: Tunnel) -> Self {
        Self {
            tunnel,
            monitor: None,
            reconnect_task: None,
            consecutive_failures: 0,
            generation: 0,
            stub_handle: None,
            next_retry_at: None,
        }
    }

    /// Snapshot for clients. Adds what only the manager knows: whether an
    /// on-demand listener is waiting, and when the next retry is due.
    fn info(&self) -> TunnelInfo {
        let mut info = self.tunnel.to_info();
        if self.stub_handle.is_some() && self.monitor.is_none() {
            info.status = TunnelStatus::Standby;
        }
        if self.tunnel.enabled {
            info.next_retry_at = self
                .next_retry_at
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
        }
        info
    }
}

struct Inner {
    tunnels: IndexMap<String, ManagedTunnel>,
    exit_tx: mpsc::UnboundedSender<MonitorEvent>,
    state_dirty: Arc<Notify>,
    event_tx: broadcast::Sender<Vec<TunnelInfo>>,
    daemon_started: String,
}

impl Inner {
    /// Notify state persistence and broadcast a tunnel snapshot to GUI subscribers.
    fn notify_changed(&self) {
        self.state_dirty.notify_one();
        if self.event_tx.receiver_count() == 0 {
            return;
        }
        let snapshot: Vec<TunnelInfo> = self
            .tunnels
            .values()
            .map(ManagedTunnel::info)
            .collect();
        // Ignore send errors -- no subscribers is fine
        let _ = self.event_tx.send(snapshot);
    }
}

/// Manages all tunnel lifecycles. Cheaply cloneable (Arc wrapper).
#[derive(Clone)]
pub struct TunnelManager {
    inner: Arc<Mutex<Inner>>,
}

fn backoff_delay(consecutive_failures: u32) -> Duration {
    // 2s, 4s, 8s, 16s, 32s, 64s, 128s, 256s, 300s (capped)
    let secs = (1u64 << consecutive_failures.min(9)).min(MAX_BACKOFF_SECS);
    Duration::from_secs(secs)
}

/// Failure count after an SSH exit. A long healthy session starts backoff
/// over; a short one (auth/DNS/forward failure) escalates it.
fn next_failure_count(current: u32, session_secs: u64) -> u32 {
    if session_secs >= STABLE_SESSION_SECS {
        1
    } else {
        current.saturating_add(1)
    }
}

/// Start SSH and spawn a monitor task. Caller must hold the lock.
/// Backoff is not reset here: SSH spawning says nothing about whether the
/// session will survive authentication.
fn start_tunnel(
    mt: &mut ManagedTunnel,
    exit_tx: &mpsc::UnboundedSender<MonitorEvent>,
) -> Result<(), TunnelError> {
    let id = mt.tunnel.id.clone();

    if mt.tunnel.status == TunnelStatus::Connected
        || mt.tunnel.status == TunnelStatus::Connecting
    {
        return Err(TunnelError::AlreadyConnected(id));
    }

    let child = mt.tunnel.start().map_err(|source| {
        if source.kind() == std::io::ErrorKind::AddrInUse {
            let port = mt.tunnel.config().local_port;
            TunnelError::PortInUse { id: id.clone(), port }
        } else {
            TunnelError::Start { id: id.clone(), source }
        }
    })?;

    mt.generation += 1;
    mt.next_retry_at = None;
    mt.monitor = Some(Monitor::spawn(id, mt.generation, child, exit_tx.clone()));
    Ok(())
}

impl TunnelManager {
    pub fn new() -> Self {
        let (exit_tx, exit_rx) = mpsc::unbounded_channel();
        let state_dirty = Arc::new(Notify::new());
        let (event_tx, _) = broadcast::channel(64);
        let inner = Arc::new(Mutex::new(Inner {
            tunnels: IndexMap::new(),
            exit_tx,
            state_dirty,
            event_tx,
            daemon_started: chrono::Utc::now().to_rfc3339(),
        }));

        let mgr = Self { inner };
        Self::spawn_exit_handler(exit_rx, mgr.clone());
        mgr
    }

    /// Populate tunnels from config. Does not start any -- that's the caller's job.
    pub async fn load_tunnels(&self, config: &Config) {
        let mut state = self.inner.lock().await;
        for (id, tunnel_config) in &config.tunnel {
            let tunnel = Tunnel::new(id.clone(), tunnel_config.clone(), &config.defaults);
            state.tunnels.insert(id.clone(), ManagedTunnel::new(tunnel));
        }
        tracing::info!(count = state.tunnels.len(), "loaded tunnels from config");
    }

    /// Start the SSH process for a tunnel (user-initiated).
    pub async fn connect(&self, id: &str) -> Result<(), TunnelError> {
        let mut state = self.inner.lock().await;
        let exit_tx = state.exit_tx.clone();
        let mt = state
            .tunnels
            .get_mut(id)
            .ok_or_else(|| TunnelError::NotFound(id.to_string()))?;

        // Cancel any pending reconnect -- user is taking over
        if let Some(handle) = mt.reconnect_task.take() {
            handle.abort();
        }

        // Reset backoff so user gets a fresh start
        mt.consecutive_failures = 0;

        // Stop stub listener if active (frees port for SSH)
        mt.stub_handle = None;

        let result = start_tunnel(mt, &exit_tx);
        state.notify_changed();
        result
    }

    /// Kill the SSH process for a tunnel (user-initiated).
    pub async fn disconnect(&self, id: &str) -> Result<(), TunnelError> {
        let mut state = self.inner.lock().await;
        let mt = state
            .tunnels
            .get_mut(id)
            .ok_or_else(|| TunnelError::NotFound(id.to_string()))?;

        // Wait for SSH to be reaped so its port is free for a stub or reconnect.
        if let Some(monitor) = mt.monitor.take() {
            monitor.stop().await;
        }
        if let Some(handle) = mt.reconnect_task.take() {
            handle.abort();
        }

        mt.consecutive_failures = 0;
        mt.generation += 1;
        mt.next_retry_at = None;
        mt.tunnel.record_disconnect();
        state.notify_changed();
        Ok(())
    }

    /// Enable a tunnel. Auto-connect if mode==auto, start stub if on-demand.
    pub async fn enable(&self, id: &str) -> Result<(), TunnelError> {
        let mode = {
            let mut state = self.inner.lock().await;
            let mt = state
                .tunnels
                .get_mut(id)
                .ok_or_else(|| TunnelError::NotFound(id.to_string()))?;

            if mt.tunnel.enabled {
                return Ok(());
            }
            mt.tunnel.enabled = true;
            let mode = mt.tunnel.config().mode;
            state.notify_changed();
            mode
        };

        match mode {
            TunnelMode::Auto => {
                if let Err(e) = self.connect(id).await {
                    tracing::warn!(tunnel_id = %id, error = %e, "enabled but failed to auto-connect");
                }
            }
            TunnelMode::OnDemand => {
                self.restart_stub_if_needed(id).await;
            }
            TunnelMode::Manual => {}
        }
        Ok(())
    }

    /// Disable a tunnel. Disconnects if connected, stops stub if on-demand.
    pub async fn disable(&self, id: &str) -> Result<(), TunnelError> {
        let was_active = {
            let mut state = self.inner.lock().await;
            let mt = state
                .tunnels
                .get_mut(id)
                .ok_or_else(|| TunnelError::NotFound(id.to_string()))?;

            mt.tunnel.enabled = false;
            mt.stub_handle = None;
            let active = mt.tunnel.status == TunnelStatus::Connected
                || mt.tunnel.status == TunnelStatus::Connecting;
            state.notify_changed();
            active
        };

        if was_active {
            self.disconnect(id).await?;
        }
        Ok(())
    }

    /// Called by the stub listener when an on-demand connection arrives.
    /// The stub has already dropped its listener to free the port.
    /// `held_stream` is None for HTTP requests (browser will poll), Some for
    /// non-HTTP clients that need hold-and-proxy.
    pub async fn handle_on_demand(
        &self,
        id: &str,
        held_stream: Option<tokio::net::TcpStream>,
        local_port: u16,
    ) {
        let connect_result = {
            let mut state = self.inner.lock().await;
            let exit_tx = state.exit_tx.clone();

            let Some(mt) = state.tunnels.get_mut(id) else {
                tracing::error!(tunnel_id = %id, "on-demand trigger for unknown tunnel");
                return;
            };

            // The trigger runs outside the stub task, so disable, reload or a
            // user connect may have retired this stub in the meantime.
            if mt.stub_handle.take().is_none()
                || !mt.tunnel.enabled
                || mt.tunnel.config().mode != TunnelMode::OnDemand
            {
                tracing::debug!(tunnel_id = %id, "ignoring stale on-demand trigger");
                return;
            }

            let result = start_tunnel(mt, &exit_tx);
            state.notify_changed();
            result
        };

        match connect_result {
            Ok(()) => {
                if let Some(stream) = held_stream {
                    let tunnel_id = id.to_string();
                    tokio::spawn(async move {
                        stub::wait_and_proxy(stream, local_port, &tunnel_id).await;
                    });
                }
            }
            Err(e) => {
                tracing::error!(tunnel_id = %id, error = %e, "failed to start on-demand tunnel");
                self.restart_stub_if_needed(id).await;
            }
        }
    }

    /// Start stub listeners for all enabled on-demand tunnels that don't have one.
    pub async fn start_on_demand_stubs(&self) {
        let candidates: Vec<(String, String, u16)> = {
            let state = self.inner.lock().await;
            state
                .tunnels
                .iter()
                .filter(|(_, mt)| {
                    mt.tunnel.enabled
                        && mt.tunnel.config().mode == TunnelMode::OnDemand
                        && mt.tunnel.status == TunnelStatus::Disconnected
                        && mt.stub_handle.is_none()
                })
                .map(|(id, mt)| {
                    (
                        id.clone(),
                        mt.tunnel.config().name.clone(),
                        mt.tunnel.config().local_port,
                    )
                })
                .collect()
        };

        for (id, name, port) in candidates {
            self.start_stub_for(&id, &name, port).await;
        }
    }

    /// Re-bind stub listener for an on-demand tunnel if appropriate.
    pub async fn restart_stub_if_needed(&self, id: &str) {
        let info = {
            let state = self.inner.lock().await;
            state.tunnels.get(id).and_then(|mt| {
                // A live SSH owns the port; binding the stub would break it.
                if mt.tunnel.config().mode == TunnelMode::OnDemand
                    && mt.tunnel.enabled
                    && mt.stub_handle.is_none()
                    && mt.monitor.is_none()
                {
                    Some((mt.tunnel.config().name.clone(), mt.tunnel.config().local_port))
                } else {
                    None
                }
            })
        };
        if let Some((name, port)) = info {
            self.start_stub_for(id, &name, port).await;
        }
    }

    async fn start_stub_for(&self, id: &str, name: &str, port: u16) {
        match stub::spawn_stub(id.to_string(), name.to_string(), port, self.clone()) {
            Ok(handle) => {
                let mut state = self.inner.lock().await;
                if let Some(mt) = state.tunnels.get_mut(id) {
                    mt.stub_handle = Some(handle);
                    mt.next_retry_at = None;
                    // Status now reads as Standby
                    state.notify_changed();
                }
            }
            Err(e) => {
                tracing::error!(
                    tunnel_id = %id,
                    error = %e,
                    "failed to bind stub listener"
                );
            }
        }
    }

    /// Connect all enabled tunnels (auto and manual modes).
    pub async fn connect_all(&self) -> BulkResult {
        let ids: Vec<String> = {
            let inner = self.inner.lock().await;
            inner
                .tunnels
                .iter()
                .filter(|(_, mt)| {
                    mt.tunnel.enabled
                        && mt.tunnel.status != TunnelStatus::Connected
                        && mt.tunnel.status != TunnelStatus::Connecting
                })
                .map(|(id, _)| id.clone())
                .collect()
        };

        let mut succeeded = 0u32;
        let mut errors = Vec::new();
        for id in &ids {
            match self.connect(id).await {
                Ok(()) => succeeded += 1,
                Err(e) => errors.push(format!("{id}: {e}")),
            }
        }
        BulkResult {
            succeeded,
            failed: errors.len() as u32,
            errors,
        }
    }

    /// Disconnect all currently connected/connecting tunnels.
    pub async fn disconnect_all(&self) -> BulkResult {
        let ids: Vec<String> = {
            let inner = self.inner.lock().await;
            inner
                .tunnels
                .iter()
                .filter(|(_, mt)| {
                    mt.tunnel.status == TunnelStatus::Connected
                        || mt.tunnel.status == TunnelStatus::Connecting
                })
                .map(|(id, _)| id.clone())
                .collect()
        };

        let mut succeeded = 0u32;
        let mut errors = Vec::new();
        for id in &ids {
            match self.disconnect(id).await {
                Ok(()) => succeeded += 1,
                Err(e) => errors.push(format!("{id}: {e}")),
            }
        }
        BulkResult {
            succeeded,
            failed: errors.len() as u32,
            errors,
        }
    }

    /// Disconnect all, then connect all enabled tunnels.
    pub async fn restart_all(&self) -> BulkResult {
        // disconnect() waits for SSH to exit, so ports are free here.
        self.disconnect_all().await;
        self.connect_all().await
    }

    /// Reset backoff and reconnect auto-mode tunnels in error state.
    /// Called on network change to recover tunnels quickly instead of waiting
    /// for the exponential backoff timer.
    pub async fn reconnect_errored(&self) {
        let ids: Vec<String> = {
            let mut inner = self.inner.lock().await;
            let mut candidates = Vec::new();
            for (id, mt) in &mut inner.tunnels {
                // Disconnected means the user stopped it; leave it alone.
                if mt.tunnel.enabled
                    && mt.tunnel.config().mode == TunnelMode::Auto
                    && mt.tunnel.status == TunnelStatus::Error
                {
                    mt.consecutive_failures = 0;
                    if let Some(handle) = mt.reconnect_task.take() {
                        handle.abort();
                    }
                    candidates.push(id.clone());
                }
            }
            candidates
        };

        if ids.is_empty() {
            return;
        }

        tracing::info!(count = ids.len(), "reconnecting errored tunnels after network change");
        for id in &ids {
            if let Err(e) = self.connect(id).await {
                tracing::warn!(tunnel_id = %id, error = %e, "network-triggered reconnect failed");
            }
        }
    }

    /// Snapshot of all tunnels for IPC responses. Order matches config file.
    pub async fn list(&self) -> Vec<TunnelInfo> {
        let state = self.inner.lock().await;
        state
            .tunnels
            .values()
            .map(ManagedTunnel::info)
            .collect()
    }

    /// Snapshot of a single tunnel.
    pub async fn get(&self, id: &str) -> Option<TunnelInfo> {
        let state = self.inner.lock().await;
        state.tunnels.get(id).map(ManagedTunnel::info)
    }

    /// Subscribe to tunnel state change events for push-based GUI updates.
    pub async fn subscribe_events(&self) -> broadcast::Receiver<Vec<TunnelInfo>> {
        self.inner.lock().await.event_tx.subscribe()
    }

    pub async fn tunnel_count(&self) -> usize {
        self.inner.lock().await.tunnels.len()
    }

    /// Reload tunnels from a new config.
    /// - Add new tunnels (don't auto-connect)
    /// - Remove tunnels no longer in config (disconnect first if active)
    /// - Update existing tunnels whose config changed (reconnect if was connected)
    pub async fn reload_config(&self, config: &Config) -> ReloadResult {
        // Phase 1: collect what to add, remove, update while holding the lock
        let (to_add, to_remove, to_update, reconnect_ids) = {
            let state = self.inner.lock().await;

            let mut to_add = Vec::new();
            let mut to_update = Vec::new();
            let mut reconnect_ids = Vec::new();

            for (id, new_cfg) in &config.tunnel {
                match state.tunnels.get(id) {
                    None => to_add.push(id.clone()),
                    Some(mt) => {
                        if mt.tunnel.config() != new_cfg {
                            let was_connected = mt.tunnel.status == TunnelStatus::Connected
                                || mt.tunnel.status == TunnelStatus::Connecting;
                            to_update.push(id.clone());
                            if was_connected {
                                reconnect_ids.push(id.clone());
                            }
                        }
                    }
                }
            }

            let to_remove: Vec<String> = state
                .tunnels
                .keys()
                .filter(|id| !config.tunnel.contains_key(*id))
                .cloned()
                .collect();

            (to_add, to_remove, to_update, reconnect_ids)
        };

        let mut result = ReloadResult {
            added: Vec::new(),
            removed: Vec::new(),
            updated: Vec::new(),
            errors: Vec::new(),
        };

        // Phase 2: disconnect tunnels that will be removed or reconnected
        for id in &to_remove {
            if let Err(e) = self.disconnect(id).await {
                // May already be disconnected, that's fine
                tracing::debug!(tunnel_id = %id, error = %e, "disconnect before remove");
            }
        }
        for id in &reconnect_ids {
            if let Err(e) = self.disconnect(id).await {
                tracing::debug!(tunnel_id = %id, error = %e, "disconnect before update");
            }
        }

        // Phase 3: mutate under lock
        {
            let mut state = self.inner.lock().await;

            for id in &to_add {
                let tunnel_config = &config.tunnel[id];
                let tunnel =
                    Tunnel::new(id.clone(), tunnel_config.clone(), &config.defaults);
                state.tunnels.insert(id.clone(), ManagedTunnel::new(tunnel));
                result.added.push(id.clone());
                tracing::info!(tunnel_id = %id, "added tunnel");
            }

            for id in &to_remove {
                state.tunnels.shift_remove(id);
                result.removed.push(id.clone());
                tracing::info!(tunnel_id = %id, "removed tunnel");
            }

            for id in &to_update {
                if let Some(mt) = state.tunnels.get_mut(id) {
                    // Stop stub before config change (mode/port may differ)
                    mt.stub_handle = None;
                    let new_cfg = config.tunnel[id].clone();
                    mt.tunnel.update_config(new_cfg, &config.defaults);
                    mt.consecutive_failures = 0;
                    result.updated.push(id.clone());
                    tracing::info!(tunnel_id = %id, "updated tunnel config");
                }
            }

            // Reorder tunnels to match new config file order
            for (i, key) in config.tunnel.keys().enumerate() {
                if let Some(current) = state.tunnels.get_index_of(key) {
                    if current != i {
                        state.tunnels.move_index(current, i);
                    }
                }
            }

            state.notify_changed();
        }

        // Phase 4: reconnect tunnels that were connected before the update
        for id in &reconnect_ids {
            if let Err(e) = self.connect(id).await {
                let msg = format!("{id}: reconnect failed: {e}");
                tracing::warn!(tunnel_id = %id, error = %e, "reconnect after config update failed");
                result.errors.push(msg);
            }
        }

        // Phase 5: start stubs for new/updated on-demand tunnels
        self.start_on_demand_stubs().await;

        tracing::info!(
            added = result.added.len(),
            removed = result.removed.len(),
            updated = result.updated.len(),
            "config reload complete"
        );
        result
    }

    /// Merge persisted state into loaded tunnels. Config defines what tunnels
    /// exist; state restores enabled flag and accumulated stats.
    pub async fn apply_state(&self, persisted: &State) {
        let mut inner = self.inner.lock().await;
        for (id, pt) in &persisted.tunnels {
            if let Some(mt) = inner.tunnels.get_mut(id) {
                mt.tunnel.enabled = pt.enabled;
                mt.tunnel.last_connected = pt.last_connected.clone();
                mt.tunnel.last_error = pt.last_error.clone();
                mt.tunnel.total_connections = pt.stats.total_connections;
                mt.tunnel.total_uptime_seconds = pt.stats.total_uptime_seconds;
                mt.tunnel.reconnect_count = pt.stats.reconnect_count;
            }
        }
        tracing::info!("applied persisted state");
    }

    /// Connect all tunnels with mode=auto and enabled=true.
    pub async fn connect_auto_tunnels(&self) {
        let ids: Vec<String> = {
            let inner = self.inner.lock().await;
            inner
                .tunnels
                .iter()
                .filter(|(_, mt)| {
                    mt.tunnel.enabled && mt.tunnel.config().mode == TunnelMode::Auto
                })
                .map(|(id, _)| id.clone())
                .collect()
        };

        for id in &ids {
            if let Err(e) = self.connect(id).await {
                tracing::error!(tunnel_id = %id, error = %e, "failed to auto-connect");
            }
        }

        if !ids.is_empty() {
            tracing::info!(count = ids.len(), "auto-connected tunnels");
        }
    }

    /// Spawn a background task that saves state to disk with 1s debounce.
    pub fn enable_state_persistence(&self, path: PathBuf) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let state_dirty = inner.lock().await.state_dirty.clone();
            loop {
                state_dirty.notified().await;
                tokio::time::sleep(Duration::from_secs(1)).await;

                let snapshot = Self::snapshot_state(&inner).await;
                if let Err(e) = state::save_state_to(&path, &snapshot) {
                    tracing::error!(error = %e, "failed to save state");
                }
            }
        });
    }

    /// Stop every SSH process and pending timer so none outlive the daemon.
    pub async fn shutdown(&self) {
        let mut state = self.inner.lock().await;
        for mt in state.tunnels.values_mut() {
            if let Some(handle) = mt.reconnect_task.take() {
                handle.abort();
            }
            mt.stub_handle = None;
            mt.generation += 1;
            if let Some(monitor) = mt.monitor.take() {
                monitor.stop().await;
            }
        }
    }

    /// Save state synchronously (for daemon shutdown).
    pub async fn save_state_now(&self, path: &std::path::Path) {
        let snapshot = Self::snapshot_state(&self.inner).await;
        if let Err(e) = state::save_state_to(path, &snapshot) {
            tracing::error!(error = %e, "failed to save state on shutdown");
        }
    }

    async fn snapshot_state(inner: &Arc<Mutex<Inner>>) -> State {
        let guard = inner.lock().await;
        let mut tunnels = HashMap::new();
        for (id, mt) in &guard.tunnels {
            let t = &mt.tunnel;
            let uptime = t.total_uptime_seconds
                + t.session_start_elapsed();
            tunnels.insert(
                id.clone(),
                PersistedTunnel {
                    enabled: t.enabled,
                    last_connected: t.last_connected.clone(),
                    last_error: t.last_error.clone(),
                    stats: PersistedTunnelStats {
                        total_connections: t.total_connections,
                        total_uptime_seconds: uptime,
                        reconnect_count: t.reconnect_count,
                    },
                },
            );
        }
        State {
            tunnels,
            daemon_started: Some(guard.daemon_started.clone()),
        }
    }

    fn spawn_exit_handler(mut exit_rx: mpsc::UnboundedReceiver<MonitorEvent>, mgr: TunnelManager) {
        tokio::spawn(async move {
            while let Some(event) = exit_rx.recv().await {
                let mut state = mgr.inner.lock().await;
                let Some(mt) = state.tunnels.get_mut(event.id()) else {
                    continue;
                };
                if event.generation() != mt.generation {
                    tracing::debug!(tunnel_id = %event.id(), "ignoring event from stale SSH process");
                    continue;
                }

                match event {
                    MonitorEvent::Ready { .. } => mt.tunnel.mark_connected(),
                    MonitorEvent::Exited { id, code, stderr, .. } => {
                        Self::handle_exit(&mgr, mt, id, code, stderr);
                    }
                }
                state.notify_changed();
            }
        });
    }

    /// Record an SSH exit and schedule recovery: reconnect for auto mode,
    /// stub re-bind for on-demand. Both back off on repeated short sessions.
    fn handle_exit(
        mgr: &TunnelManager,
        mt: &mut ManagedTunnel,
        id: String,
        code: Option<i32>,
        stderr: Option<String>,
    ) {
        mt.monitor = None;
        let session_secs = mt.tunnel.session_start_elapsed();
        mt.tunnel.record_exit(code, stderr);
        mt.consecutive_failures = next_failure_count(mt.consecutive_failures, session_secs);
        let delay = backoff_delay(mt.consecutive_failures);
        mt.next_retry_at = None;

        if mt.tunnel.should_reconnect() {
            tracing::info!(
                tunnel_id = %id,
                delay_secs = delay.as_secs(),
                failures = mt.consecutive_failures,
                "scheduling reconnect"
            );
            let inner = Arc::clone(&mgr.inner);
            if let Some(old) = mt.reconnect_task.take() {
                old.abort();
            }
            mt.next_retry_at = Some(SystemTime::now() + delay);
            mt.reconnect_task = Some(tokio::spawn(Self::reconnect_loop(inner, id, delay)));
        } else if mt.tunnel.config().mode == TunnelMode::OnDemand
            && mt.tunnel.enabled
            && mt.stub_handle.is_none()
        {
            tracing::info!(
                tunnel_id = %id,
                delay_secs = delay.as_secs(),
                "scheduling on-demand listener re-bind"
            );
            let mgr = mgr.clone();
            if let Some(old) = mt.reconnect_task.take() {
                old.abort();
            }
            mt.next_retry_at = Some(SystemTime::now() + delay);
            mt.reconnect_task = Some(tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                mgr.restart_stub_if_needed(&id).await;
            }));
        }
    }

    /// Sleep then attempt reconnection. Retries with increasing backoff
    /// if start fails synchronously (e.g. bad binary). Exits when the tunnel
    /// connects or is no longer eligible for reconnection.
    async fn reconnect_loop(
        inner: Arc<Mutex<Inner>>,
        id: String,
        initial_delay: Duration,
    ) {
        let mut delay = initial_delay;

        loop {
            tokio::time::sleep(delay).await;

            let should_retry = {
                let mut state = inner.lock().await;
                let exit_tx = state.exit_tx.clone();

                let Some(mt) = state.tunnels.get_mut(&id) else {
                    break;
                };

                if !mt.tunnel.should_reconnect() {
                    break;
                }

                mt.tunnel.reconnect_count += 1;

                tracing::info!(
                    tunnel_id = %id,
                    attempt = mt.tunnel.reconnect_count,
                    "attempting reconnect"
                );

                let retry = match start_tunnel(mt, &exit_tx) {
                    Ok(()) => false,
                    // start failed synchronously (no child spawned), so the
                    // exit handler won't fire. We must retry ourselves, unless
                    // the failure (e.g. port conflict) rules reconnecting out.
                    Err(e) if mt.tunnel.should_reconnect() => {
                        mt.consecutive_failures += 1;
                        delay = backoff_delay(mt.consecutive_failures);
                        mt.next_retry_at = Some(SystemTime::now() + delay);
                        tracing::warn!(
                            tunnel_id = %id,
                            error = %e,
                            delay_secs = delay.as_secs(),
                            "reconnect failed, will retry"
                        );
                        true
                    }
                    Err(e) => {
                        tracing::warn!(tunnel_id = %id, error = %e, "reconnect failed, giving up");
                        false
                    }
                };
                // Clients see Connecting, or the new error and retry time.
                state.notify_changed();
                retry
            }; // lock released

            if !should_retry {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{Defaults, TunnelConfig, TunnelMode, TunnelType};

    fn test_config() -> Config {
        let mut tunnel = IndexMap::new();
        tunnel.insert(
            "dev-db".to_string(),
            TunnelConfig {
                name: "Dev Database".into(),
                host: "bastion.example.com".into(),
                port: None,
                tunnel_type: TunnelType::Local,
                mode: TunnelMode::Auto,
                local_port: 59432,
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
                port: None,
                tunnel_type: TunnelType::Socks,
                mode: TunnelMode::OnDemand,
                local_port: 59080,
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

    fn manual_config(ssh_binary: &str) -> Config {
        let mut tunnel = IndexMap::new();
        tunnel.insert(
            "manual-tun".to_string(),
            TunnelConfig {
                name: "Manual".into(),
                host: "example.com".into(),
                port: None,
                tunnel_type: TunnelType::Local,
                mode: TunnelMode::Manual,
                local_port: 59999,
                remote_host: Some("localhost".into()),
                remote_port: Some(22),
                local_host: None,
                remote_bind: None,
                identity: None,
                jump_host: None,
                jump_port: None,
                ssh_binary: Some(ssh_binary.into()),
                keepalive: None,
            },
        );
        Config {
            defaults: Defaults::default(),
            tunnel,
        }
    }

    /// Script that ignores SSH arguments and stays alive, unlike `sleep`,
    /// which rejects the `-N` flag and exits immediately.
    fn long_running_fake_ssh() -> (tempfile::TempDir, String) {
        fake_ssh("exec sleep 60")
    }

    fn fake_ssh(body: &str) -> (tempfile::TempDir, String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-ssh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path.to_string_lossy().into_owned())
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
    async fn list_preserves_config_order() {
        let mgr = TunnelManager::new();
        // test_config inserts dev-db then proxy
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
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.ssh_binary = Some("/nonexistent/ssh".into());
        tc.local_port = 59001;

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
        let (_dir, fake_ssh) = long_running_fake_ssh();
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.ssh_binary = Some(fake_ssh);
        tc.local_port = 59002;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;

        mgr.connect("dev-db").await.unwrap();
        // No "Authenticated to" line yet, so not Connected.
        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Connecting);

        mgr.disconnect("dev-db").await.unwrap();
        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Disconnected);
    }

    #[tokio::test]
    async fn exit_detection() {
        let mut config = test_config();
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.mode = TunnelMode::Manual;
        tc.ssh_binary = Some("false".into());
        tc.local_port = 59003;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;

        mgr.connect("dev-db").await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Error);
        assert!(info.last_error.is_some());
    }

    #[test]
    fn backoff_grows_exponentially() {
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
        assert_eq!(backoff_delay(4), Duration::from_secs(16));
        assert_eq!(backoff_delay(5), Duration::from_secs(32));
        assert_eq!(backoff_delay(8), Duration::from_secs(256));
    }

    #[test]
    fn short_sessions_escalate_backoff() {
        assert_eq!(next_failure_count(0, 0), 1);
        assert_eq!(next_failure_count(1, 5), 2);
        assert_eq!(next_failure_count(4, STABLE_SESSION_SECS - 1), 5);
    }

    #[test]
    fn stable_session_resets_backoff() {
        assert_eq!(next_failure_count(7, STABLE_SESSION_SECS), 1);
    }

    #[tokio::test]
    async fn repeated_quick_exits_grow_backoff() {
        let mut config = test_config();
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.ssh_binary = Some("false".into());
        tc.local_port = 59008;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;
        mgr.connect("dev-db").await.unwrap();

        // First exit -> 2s backoff; first reconnect exits too -> 4s backoff
        tokio::time::sleep(Duration::from_millis(2400)).await;
        let failures = mgr.inner.lock().await.tunnels["dev-db"].consecutive_failures;
        mgr.disconnect("dev-db").await.unwrap();
        assert_eq!(failures, 2);
    }

    #[tokio::test]
    async fn stale_exit_event_is_ignored() {
        let mut config = test_config();
        let (_dir, fake_ssh) = long_running_fake_ssh();
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.ssh_binary = Some(fake_ssh);
        tc.local_port = 59009;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;
        mgr.connect("dev-db").await.unwrap();

        let exit_tx = mgr.inner.lock().await.exit_tx.clone();
        exit_tx
            .send(MonitorEvent::Exited {
                id: "dev-db".into(),
                generation: 0,
                code: Some(255),
                stderr: None,
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let info = mgr.get("dev-db").await.unwrap();
        mgr.disconnect("dev-db").await.unwrap();
        assert_ne!(info.status, TunnelStatus::Error);
    }

    #[tokio::test]
    async fn becomes_connected_after_authentication() {
        let mut config = test_config();
        let (_dir, fake_ssh) = fake_ssh("echo 'Authenticated to bastion' >&2; exec sleep 60");
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.ssh_binary = Some(fake_ssh);
        tc.local_port = 59010;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;
        mgr.connect("dev-db").await.unwrap();

        // Poll: process spawn can be slow when the suite runs in parallel
        let mut info = mgr.get("dev-db").await.unwrap();
        for _ in 0..50 {
            if info.status == TunnelStatus::Connected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            info = mgr.get("dev-db").await.unwrap();
        }
        mgr.disconnect("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Connected);
        assert_eq!(info.stats.unwrap().total_connections, 1);
    }

    #[tokio::test]
    async fn on_demand_listener_reports_standby() {
        let mut config = test_config();
        config.tunnel.get_mut("proxy").unwrap().local_port = 59011;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;
        mgr.start_on_demand_stubs().await;

        let info = mgr.get("proxy").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Standby);
        mgr.disable("proxy").await.unwrap();
        assert_eq!(mgr.get("proxy").await.unwrap().status, TunnelStatus::Disconnected);
    }

    #[tokio::test]
    async fn failing_tunnel_reports_next_retry() {
        let mut config = test_config();
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.ssh_binary = Some("false".into());
        tc.local_port = 59012;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;
        mgr.connect("dev-db").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let retry_at = mgr.get("dev-db").await.unwrap().next_retry_at.unwrap();
        assert!(retry_at >= now && retry_at <= now + 3);

        mgr.disconnect("dev-db").await.unwrap();
        assert!(mgr.get("dev-db").await.unwrap().next_retry_at.is_none());
    }

    #[test]
    fn backoff_caps_at_max() {
        assert_eq!(backoff_delay(9), Duration::from_secs(300));
        assert_eq!(backoff_delay(10), Duration::from_secs(300));
        assert_eq!(backoff_delay(100), Duration::from_secs(300));
    }

    #[tokio::test]
    async fn manual_tunnel_does_not_reconnect() {
        let mut config = manual_config("false");
        config.tunnel.get_mut("manual-tun").unwrap().local_port = 59004;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;

        mgr.connect("manual-tun").await.unwrap();

        // Wait for exit + enough time that a reconnect would have been scheduled
        tokio::time::sleep(Duration::from_millis(200)).await;

        let info = mgr.get("manual-tun").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Error);
        assert_eq!(info.stats.unwrap().reconnect_count, 0);
    }

    #[tokio::test]
    async fn auto_tunnel_schedules_reconnect() {
        let mut config = test_config();
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.ssh_binary = Some("false".into());
        tc.local_port = 59005;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;

        mgr.connect("dev-db").await.unwrap();

        // Wait for exit handler to run
        tokio::time::sleep(Duration::from_millis(100)).await;

        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Error);

        // Backoff is 2s, wait for first reconnect attempt
        tokio::time::sleep(Duration::from_millis(2200)).await;

        let info = mgr.get("dev-db").await.unwrap();
        assert!(info.stats.unwrap().reconnect_count >= 1);
    }

    #[tokio::test]
    async fn disconnect_cancels_pending_reconnect() {
        let mut config = test_config();
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.ssh_binary = Some("false".into());
        tc.local_port = 59006;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;

        mgr.connect("dev-db").await.unwrap();

        // Wait for exit handler to schedule reconnect
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Disconnect before the 2s reconnect timer fires
        mgr.disconnect("dev-db").await.unwrap();

        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Disconnected);

        // Wait past the reconnect timer
        tokio::time::sleep(Duration::from_millis(2500)).await;

        // Should still be disconnected, not reconnected
        let info = mgr.get("dev-db").await.unwrap();
        assert_eq!(info.status, TunnelStatus::Disconnected);
        assert_eq!(info.stats.unwrap().reconnect_count, 0);
    }

    #[tokio::test]
    async fn connect_resets_backoff() {
        let mut config = test_config();
        let (_dir, fake_ssh) = long_running_fake_ssh();
        let tc = config.tunnel.get_mut("dev-db").unwrap();
        tc.ssh_binary = Some(fake_ssh);
        tc.local_port = 59007;

        let mgr = TunnelManager::new();
        mgr.load_tunnels(&config).await;

        mgr.connect("dev-db").await.unwrap();

        // Verify consecutive_failures is 0 after successful connect
        let state = mgr.inner.lock().await;
        assert_eq!(state.tunnels["dev-db"].consecutive_failures, 0);
        drop(state);

        mgr.disconnect("dev-db").await.unwrap();
    }
}
