use std::path::Path;
use std::time::{Instant, SystemTime};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::watch;

use crate::common::log_broadcast::LogBroadcast;
use crate::config::TunnelConfig;
use crate::ipc::protocol::{self, DaemonInfo, Request, RpcNotification, RpcRequest, RpcResponse};

use super::manager::TunnelManager;
use super::DaemonError;

/// Bind the IPC socket, readable only by the current user.
pub fn bind(socket_path: &Path) -> Result<UnixListener, DaemonError> {
    let listener = UnixListener::bind(socket_path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// Serve requests until daemon.shutdown, SIGTERM or SIGINT.
pub async fn run(
    listener: UnixListener,
    mgr: TunnelManager,
    broadcast: LogBroadcast,
) -> Result<(), DaemonError> {
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

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
                        let broadcast = broadcast.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(
                                stream, started, started_epoch, shutdown_tx, mgr, broadcast,
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
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM");
                break;
            }
            _ = sigint.recv() => {
                tracing::info!("received SIGINT");
                break;
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
    broadcast: LogBroadcast,
) -> Result<(), std::io::Error> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut log_rx: Option<tokio::sync::broadcast::Receiver<protocol::LogLine>> = None;
    let mut event_rx: Option<tokio::sync::broadcast::Receiver<Vec<protocol::TunnelInfo>>> = None;

    loop {
        // When not subscribed, these branches use pending() (never fires, zero cost).
        let log_recv = async {
            match log_rx.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        let event_recv = async {
            match event_rx.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            line_result = lines.next_line() => {
                let Some(line) = line_result? else { break };
                if line.trim().is_empty() {
                    continue;
                }

                let response = match serde_json::from_str::<RpcRequest>(&line) {
                    Ok(rpc_req) => match Request::from_rpc(&rpc_req) {
                        Ok(Request::LogsSubscribe { last_n }) => {
                            // Flush recent lines as notifications, then subscribe
                            let recent = broadcast.recent(last_n as usize);
                            for log_line in &recent {
                                write_notification(&mut writer, &RpcNotification::log_line(log_line)).await?;
                            }
                            log_rx = Some(broadcast.subscribe());
                            RpcResponse::success(rpc_req.id, serde_json::json!({ "status": "subscribed" }))
                        }
                        Ok(Request::EventsSubscribe) => {
                            // Send current state as initial notification, then subscribe
                            let tunnels = mgr.list().await;
                            write_notification(&mut writer, &RpcNotification::tunnel_changed(&tunnels)).await?;
                            event_rx = Some(mgr.subscribe_events().await);
                            RpcResponse::success(rpc_req.id, serde_json::json!({ "status": "subscribed" }))
                        }
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
            log_result = log_recv => {
                match log_result {
                    Ok(log_line) => {
                        write_notification(&mut writer, &RpcNotification::log_line(&log_line)).await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(skipped = n, "log subscriber lagged, some lines dropped");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log_rx = None;
                    }
                }
            }
            event_result = event_recv => {
                let tunnels = match event_result {
                    Ok(tunnels) => tunnels,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Intermediate states don't matter; re-fetch current state
                        mgr.list().await
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        event_rx = None;
                        continue;
                    }
                };
                write_notification(&mut writer, &RpcNotification::tunnel_changed(&tunnels)).await?;
            }
        }
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
                // disconnect() waited for SSH to exit, so the port is free.
                mgr.restart_stub_if_needed(&tid).await;
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
            mgr.start_on_demand_stubs().await;
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
        Request::TunnelAdd {
            id: tid,
            config: config_json,
        } => handle_tunnel_add(id, &tid, config_json, mgr).await,
        Request::TunnelUpdate {
            id: tid,
            config: config_json,
        } => handle_tunnel_update(id, &tid, config_json, mgr).await,
        Request::TunnelRemove {
            id: tid,
            force,
        } => handle_tunnel_remove(id, &tid, force, mgr).await,
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
        // Handled in handle_connection before dispatching here
        Request::LogsSubscribe { .. } | Request::EventsSubscribe => {
            RpcResponse::error(id, protocol::INTERNAL_ERROR, "unexpected dispatch")
        }
    }
}

async fn handle_tunnel_add(
    req_id: u64,
    tunnel_id: &str,
    config_json: Value,
    mgr: &TunnelManager,
) -> RpcResponse {
    if !crate::config::is_valid_tunnel_id(tunnel_id) {
        return RpcResponse::error(
            req_id,
            protocol::VALIDATION_ERROR,
            format!("invalid tunnel ID '{tunnel_id}': must be lowercase alphanumeric with hyphens"),
        );
    }

    let tunnel_config: TunnelConfig = match serde_json::from_value(config_json) {
        Ok(c) => c,
        Err(e) => {
            return RpcResponse::error(
                req_id,
                protocol::VALIDATION_ERROR,
                format!("invalid tunnel config: {e}"),
            );
        }
    };

    // Load existing config, insert new tunnel, validate, save, reload
    let mut cfg = match crate::config::load_config() {
        Ok(c) => c,
        Err(crate::config::ConfigError::NotFound(_)) => crate::config::Config {
            defaults: crate::config::Defaults::default(),
            tunnel: indexmap::IndexMap::new(),
        },
        Err(e) => {
            return RpcResponse::error(req_id, protocol::CONFIG_ERROR, e.to_string());
        }
    };

    if cfg.tunnel.contains_key(tunnel_id) {
        return RpcResponse::error(
            req_id,
            protocol::TUNNEL_ALREADY_EXISTS,
            format!("tunnel '{tunnel_id}' already exists"),
        );
    }

    cfg.tunnel.insert(tunnel_id.to_string(), tunnel_config);

    let result = crate::config::validate_config(&cfg);
    if !result.is_ok() {
        let msg = result
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return RpcResponse::error(req_id, protocol::VALIDATION_ERROR, msg);
    }

    if let Err(e) = crate::config::save_config(&cfg) {
        return RpcResponse::error(req_id, protocol::CONFIG_ERROR, e.to_string());
    }

    let reload = mgr.reload_config(&cfg).await;
    tracing::info!(tunnel_id = %tunnel_id, "tunnel added via IPC");
    RpcResponse::success(req_id, json!({ "status": "added", "reload": reload }))
}

async fn handle_tunnel_update(
    req_id: u64,
    tunnel_id: &str,
    config_json: Value,
    mgr: &TunnelManager,
) -> RpcResponse {
    let tunnel_config: TunnelConfig = match serde_json::from_value(config_json) {
        Ok(c) => c,
        Err(e) => {
            return RpcResponse::error(
                req_id,
                protocol::VALIDATION_ERROR,
                format!("invalid tunnel config: {e}"),
            );
        }
    };

    let mut cfg = match crate::config::load_config() {
        Ok(c) => c,
        Err(e) => {
            return RpcResponse::error(req_id, protocol::CONFIG_ERROR, e.to_string());
        }
    };

    if !cfg.tunnel.contains_key(tunnel_id) {
        return RpcResponse::error(
            req_id,
            protocol::TUNNEL_NOT_FOUND,
            format!("tunnel '{tunnel_id}' not found"),
        );
    }

    cfg.tunnel.insert(tunnel_id.to_string(), tunnel_config);

    let result = crate::config::validate_config(&cfg);
    if !result.is_ok() {
        let msg = result
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return RpcResponse::error(req_id, protocol::VALIDATION_ERROR, msg);
    }

    if let Err(e) = crate::config::save_config(&cfg) {
        return RpcResponse::error(req_id, protocol::CONFIG_ERROR, e.to_string());
    }

    let reload = mgr.reload_config(&cfg).await;
    tracing::info!(tunnel_id = %tunnel_id, "tunnel updated via IPC");
    RpcResponse::success(req_id, json!({ "status": "updated", "reload": reload }))
}

async fn handle_tunnel_remove(
    req_id: u64,
    tunnel_id: &str,
    force: bool,
    mgr: &TunnelManager,
) -> RpcResponse {
    let mut cfg = match crate::config::load_config() {
        Ok(c) => c,
        Err(e) => {
            return RpcResponse::error(req_id, protocol::CONFIG_ERROR, e.to_string());
        }
    };

    if !cfg.tunnel.contains_key(tunnel_id) {
        return RpcResponse::error(
            req_id,
            protocol::TUNNEL_NOT_FOUND,
            format!("tunnel '{tunnel_id}' not found"),
        );
    }

    if force {
        let _ = mgr.disconnect(tunnel_id).await;
    }

    cfg.tunnel.shift_remove(tunnel_id);

    if let Err(e) = crate::config::save_config(&cfg) {
        return RpcResponse::error(req_id, protocol::CONFIG_ERROR, e.to_string());
    }

    let reload = mgr.reload_config(&cfg).await;
    tracing::info!(tunnel_id = %tunnel_id, "tunnel removed via IPC");
    RpcResponse::success(req_id, json!({ "status": "removed", "reload": reload }))
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &RpcResponse,
) -> Result<(), std::io::Error> {
    let mut buf = serde_json::to_string(response).expect("response must serialize");
    buf.push('\n');
    writer.write_all(buf.as_bytes()).await
}

async fn write_notification(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    notification: &RpcNotification,
) -> Result<(), std::io::Error> {
    let mut buf = serde_json::to_string(notification).expect("notification must serialize");
    buf.push('\n');
    writer.write_all(buf.as_bytes()).await
}
