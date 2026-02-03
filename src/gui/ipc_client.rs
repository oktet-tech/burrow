use std::collections::HashMap;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use crate::daemon;
use crate::ipc::protocol::{
    DaemonInfo, LogLine, RpcNotification, RpcRequest, RpcResponse, TunnelInfo, JSONRPC_VERSION,
};

// -- Types --

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("not connected to daemon")]
    Disconnected,
    #[error("daemon error ({code}): {message}")]
    Rpc { code: i32, message: String },
    #[error("unexpected response format")]
    BadResponse,
}

/// Events pushed from the background IPC connection to the GUI.
#[derive(Debug, Clone)]
pub enum DaemonEvent {
    Connected,
    Disconnected(String),
    LogLine(LogEvent),
    TunnelsChanged(Vec<TunnelInfo>),
}

#[derive(Debug, Clone)]
pub struct LogEvent {
    pub timestamp: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

enum ClientCmd {
    Request {
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Value, IpcError>>,
    },
    Shutdown,
}

// -- Client handle --

const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Async IPC client for the GUI. Maintains a persistent connection to the
/// daemon with automatic reconnection on failure.
///
/// Cloneable -- all clones share the same underlying connection.
#[derive(Clone)]
pub struct GuiIpcClient {
    cmd_tx: mpsc::Sender<ClientCmd>,
}

impl std::fmt::Debug for GuiIpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuiIpcClient").finish()
    }
}

impl GuiIpcClient {
    /// Spawn the background connection task.
    /// Returns the client handle and a receiver for daemon lifecycle events.
    pub fn spawn() -> (Self, mpsc::Receiver<DaemonEvent>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (event_tx, event_rx) = mpsc::channel(256);
        tokio::spawn(connection_task(cmd_rx, event_tx));
        (Self { cmd_tx }, event_rx)
    }

    /// Send a JSON-RPC request and await the daemon's response.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, IpcError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(ClientCmd::Request {
                method: method.to_string(),
                params,
                reply: tx,
            })
            .await
            .map_err(|_| IpcError::Disconnected)?;
        rx.await.map_err(|_| IpcError::Disconnected)?
    }

    /// Send a request and deserialize the response into a concrete type.
    pub async fn request_typed<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, IpcError> {
        let value = self.request(method, params).await?;
        serde_json::from_value(value).map_err(|_| IpcError::BadResponse)
    }

    /// Signal the background task to shut down.
    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(ClientCmd::Shutdown).await;
    }

    // -- Convenience wrappers for common operations --

    pub async fn tunnel_list(&self) -> Result<Vec<TunnelInfo>, IpcError> {
        #[derive(serde::Deserialize)]
        struct R {
            tunnels: Vec<TunnelInfo>,
        }
        Ok(self
            .request_typed::<R>("tunnel.list", json!({}))
            .await?
            .tunnels)
    }

    pub async fn tunnel_connect(&self, id: &str) -> Result<(), IpcError> {
        self.request("tunnel.connect", json!({ "id": id }))
            .await
            .map(|_| ())
    }

    pub async fn tunnel_disconnect(&self, id: &str) -> Result<(), IpcError> {
        self.request("tunnel.disconnect", json!({ "id": id }))
            .await
            .map(|_| ())
    }

    pub async fn daemon_status(&self) -> Result<DaemonInfo, IpcError> {
        self.request_typed("daemon.status", json!({})).await
    }

    pub async fn tunnel_add(&self, id: &str, config: Value) -> Result<Value, IpcError> {
        self.request("tunnel.add", json!({ "id": id, "config": config }))
            .await
    }

    pub async fn tunnel_update(&self, id: &str, config: Value) -> Result<Value, IpcError> {
        self.request("tunnel.update", json!({ "id": id, "config": config }))
            .await
    }

    pub async fn tunnel_remove(&self, id: &str, force: bool) -> Result<Value, IpcError> {
        self.request("tunnel.remove", json!({ "id": id, "force": force }))
            .await
    }

    pub async fn config_reload(&self) -> Result<Value, IpcError> {
        self.request("config.reload", json!({})).await
    }
}

// -- Background connection management --

/// Reconnect loop: connect -> run session -> on disconnect, backoff -> retry.
async fn connection_task(
    mut cmd_rx: mpsc::Receiver<ClientCmd>,
    event_tx: mpsc::Sender<DaemonEvent>,
) {
    let mut backoff = RECONNECT_MIN;

    loop {
        match UnixStream::connect(&daemon::socket_path()).await {
            Ok(stream) => {
                backoff = RECONNECT_MIN;
                let _ = event_tx.send(DaemonEvent::Connected).await;

                let shutdown = run_session(stream, &mut cmd_rx, &event_tx).await;

                let _ = event_tx
                    .send(DaemonEvent::Disconnected("connection lost".into()))
                    .await;

                if shutdown {
                    return;
                }
            }
            Err(e) => {
                let _ = event_tx
                    .send(DaemonEvent::Disconnected(e.to_string()))
                    .await;
            }
        }

        // Stay responsive during backoff: fail queued requests, honor shutdown.
        if wait_or_drain(&mut cmd_rx, backoff).await {
            return;
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// Sleep for `duration`, failing incoming requests with `Disconnected`.
/// Returns true if shutdown was requested.
async fn wait_or_drain(cmd_rx: &mut mpsc::Receiver<ClientCmd>, duration: Duration) -> bool {
    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return false,
            cmd = cmd_rx.recv() => match cmd {
                Some(ClientCmd::Request { reply, .. }) => {
                    let _ = reply.send(Err(IpcError::Disconnected));
                }
                Some(ClientCmd::Shutdown) | None => return true,
            },
        }
    }
}

/// Drive one connected session: multiplex outgoing requests with incoming
/// responses over a single Unix socket. Returns true if shutdown requested.
async fn run_session(
    stream: UnixStream,
    cmd_rx: &mut mpsc::Receiver<ClientCmd>,
    event_tx: &mpsc::Sender<DaemonEvent>,
) -> bool {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut pending: HashMap<u64, oneshot::Sender<Result<Value, IpcError>>> = HashMap::new();
    let mut next_id: u64 = 1;

    // Auto-subscribe to log stream and tunnel events after connecting
    if send_request(&mut writer, &mut next_id, "logs.subscribe", json!({ "last_n": 512 }))
        .await
        .is_err()
    {
        return false;
    }
    if send_request(&mut writer, &mut next_id, "events.subscribe", json!({}))
        .await
        .is_err()
    {
        return false;
    }

    let shutdown = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(ref text)) => dispatch_line(text, &mut pending, event_tx),
                _ => break false, // EOF or read error
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(ClientCmd::Request { method, params, reply }) => {
                    let id = next_id;
                    next_id += 1;
                    let req = RpcRequest {
                        jsonrpc: JSONRPC_VERSION.to_string(),
                        id,
                        method,
                        params,
                    };
                    let mut buf =
                        serde_json::to_string(&req).expect("request must serialize");
                    buf.push('\n');
                    if writer.write_all(buf.as_bytes()).await.is_err() {
                        let _ = reply.send(Err(IpcError::Disconnected));
                        break false;
                    }
                    pending.insert(id, reply);
                }
                Some(ClientCmd::Shutdown) | None => break true,
            },
        }
    };

    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(IpcError::Disconnected));
    }
    shutdown
}

/// Send a fire-and-forget request (no pending reply tracked).
async fn send_request(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<(), std::io::Error> {
    let id = *next_id;
    *next_id += 1;
    let req = RpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id,
        method: method.to_string(),
        params,
    };
    let mut buf = serde_json::to_string(&req).expect("request must serialize");
    buf.push('\n');
    writer.write_all(buf.as_bytes()).await
}

/// Route an incoming JSON line: either a response to a pending request,
/// or a server-pushed notification (e.g. log.line).
fn dispatch_line(
    line: &str,
    pending: &mut HashMap<u64, oneshot::Sender<Result<Value, IpcError>>>,
    event_tx: &mpsc::Sender<DaemonEvent>,
) {
    let Ok(raw) = serde_json::from_str::<Value>(line) else {
        return;
    };

    // Notifications have no "id" field
    if raw.get("id").is_none() {
        if let Ok(notif) = serde_json::from_value::<RpcNotification>(raw) {
            match notif.method.as_str() {
                "log.line" => {
                    if let Ok(log_line) = serde_json::from_value::<LogLine>(notif.params) {
                        let ev = LogEvent {
                            timestamp: log_line.timestamp,
                            level: log_line.level,
                            target: log_line.target,
                            message: log_line.message,
                        };
                        let _ = event_tx.try_send(DaemonEvent::LogLine(ev));
                    }
                }
                "tunnel.changed" => {
                    if let Ok(tunnels) = serde_json::from_value::<Vec<TunnelInfo>>(notif.params) {
                        let _ = event_tx.try_send(DaemonEvent::TunnelsChanged(tunnels));
                    }
                }
                _ => {}
            }
        }
        return;
    }

    // Regular response
    let Ok(resp) = serde_json::from_value::<RpcResponse>(raw) else {
        return;
    };
    let Some(tx) = pending.remove(&resp.id) else {
        return;
    };
    let result = match resp.error {
        Some(err) => Err(IpcError::Rpc {
            code: err.code,
            message: err.message,
        }),
        None => Ok(resp.result.unwrap_or(Value::Null)),
    };
    let _ = tx.send(result);
}

