use std::path::Path;
use std::time::{Instant, SystemTime};

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::watch;

use crate::ipc::protocol::{self, DaemonInfo, Request, RpcRequest, RpcResponse};

use super::manager::TunnelManager;
use super::DaemonError;

/// Bind the IPC socket and serve requests until daemon.shutdown.
pub async fn run(socket_path: &Path, mgr: TunnelManager) -> Result<(), DaemonError> {
    let listener = UnixListener::bind(socket_path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    }

    let started = Instant::now();
    let started_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    tracing::info!("listening for IPC connections");

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let shutdown_tx = shutdown_tx.clone();
                        let mgr = mgr.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(
                                stream, started, started_epoch, shutdown_tx, mgr,
                            ).await {
                                tracing::error!(error = %e, "connection handler failed");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "accept failed");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }

    tracing::info!("shutting down");
    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    started: Instant,
    started_epoch: u64,
    shutdown_tx: watch::Sender<bool>,
    mgr: TunnelManager,
) -> Result<(), std::io::Error> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(rpc_req) => match Request::from_rpc(&rpc_req) {
                Ok(request) => {
                    let is_shutdown = matches!(request, Request::DaemonShutdown);
                    let resp =
                        handle_request(rpc_req.id, request, started, started_epoch, &mgr).await;

                    if is_shutdown {
                        write_response(&mut writer, &resp).await?;
                        let _ = shutdown_tx.send(true);
                        return Ok(());
                    }
                    resp
                }
                Err(e) => {
                    let (code, msg) = match &e {
                        protocol::ProtocolError::UnknownMethod(_) => {
                            (protocol::METHOD_NOT_FOUND, e.to_string())
                        }
                        protocol::ProtocolError::InvalidParams(_) => {
                            (protocol::INVALID_PARAMS, e.to_string())
                        }
                    };
                    RpcResponse::error(rpc_req.id, code, msg)
                }
            },
            Err(e) => RpcResponse::error(0, protocol::PARSE_ERROR, format!("parse error: {e}")),
        };

        write_response(&mut writer, &response).await?;
    }

    Ok(())
}

async fn handle_request(
    id: u64,
    request: Request,
    started: Instant,
    started_epoch: u64,
    mgr: &TunnelManager,
) -> RpcResponse {
    match request {
        Request::TunnelList => {
            let tunnels = mgr.list().await;
            RpcResponse::success(id, json!({ "tunnels": tunnels }))
        }
        Request::TunnelGet { id: tid } => match mgr.get(&tid).await {
            Some(info) => RpcResponse::success(id, info),
            None => RpcResponse::error(
                id,
                protocol::TUNNEL_NOT_FOUND,
                format!("tunnel '{tid}' not found"),
            ),
        },
        Request::TunnelConnect { id: tid } => match mgr.connect(&tid).await {
            Ok(()) => RpcResponse::success(id, json!({ "status": "connected" })),
            Err(msg) => RpcResponse::error(id, protocol::TUNNEL_NOT_FOUND, msg),
        },
        Request::TunnelDisconnect { id: tid } => match mgr.disconnect(&tid).await {
            Ok(()) => {
                // Stub re-bind needs time for the SSH process to die and release the port.
                // Spawn a delayed task so the IPC response is not blocked.
                let mgr = mgr.clone();
                let tid2 = tid.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    mgr.restart_stub_if_needed(&tid2).await;
                });
                RpcResponse::success(id, json!({ "status": "disconnected" }))
            }
            Err(msg) => RpcResponse::error(id, protocol::TUNNEL_NOT_FOUND, msg),
        },
        Request::TunnelEnable { id: tid } => match mgr.enable(&tid).await {
            Ok(()) => RpcResponse::success(id, json!({ "status": "enabled" })),
            Err(msg) => RpcResponse::error(id, protocol::TUNNEL_NOT_FOUND, msg),
        },
        Request::TunnelDisable { id: tid } => match mgr.disable(&tid).await {
            Ok(()) => RpcResponse::success(id, json!({ "status": "disabled" })),
            Err(msg) => RpcResponse::error(id, protocol::TUNNEL_NOT_FOUND, msg),
        },
        Request::TunnelConnectAll => {
            let result = mgr.connect_all().await;
            RpcResponse::success(id, result)
        }
        Request::TunnelDisconnectAll => {
            let result = mgr.disconnect_all().await;
            let mgr = mgr.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                mgr.start_on_demand_stubs().await;
            });
            RpcResponse::success(id, result)
        }
        Request::TunnelRestartAll => {
            let result = mgr.restart_all().await;
            RpcResponse::success(id, result)
        }
        Request::DaemonStatus => {
            let uptime = started.elapsed().as_secs();
            let tunnel_count = mgr.tunnel_count().await;
            RpcResponse::success(
                id,
                DaemonInfo {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    uptime_seconds: uptime,
                    tunnel_count,
                    started_at: started_epoch.to_string(),
                },
            )
        }
        Request::DaemonShutdown => {
            RpcResponse::success(id, json!({ "status": "shutting_down" }))
        }
        Request::ConfigReload => match crate::config::load_config() {
            Ok(cfg) => {
                let result = mgr.reload_config(&cfg).await;
                RpcResponse::success(id, result)
            }
            Err(crate::config::ConfigError::NotFound(_)) => {
                RpcResponse::error(id, protocol::CONFIG_ERROR, "config file not found")
            }
            Err(e) => RpcResponse::error(id, protocol::CONFIG_ERROR, e.to_string()),
        },
    }
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &RpcResponse,
) -> Result<(), std::io::Error> {
    let mut buf = serde_json::to_string(response).expect("response must serialize");
    buf.push('\n');
    writer.write_all(buf.as_bytes()).await
}
