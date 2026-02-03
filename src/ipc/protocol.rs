use serde::{Deserialize, Serialize};
use serde_json::Value;

// -- JSON-RPC 2.0 wire types --

pub const JSONRPC_VERSION: &str = "2.0";

/// Incoming JSON-RPC request (newline-delimited on the socket).
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Outgoing JSON-RPC response.
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Server-pushed notification (no id, no response expected).
/// Used for log streaming via logs.subscribe.
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

impl RpcNotification {
    pub fn log_line(line: &LogLine) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "log.line".to_string(),
            params: serde_json::to_value(line).expect("LogLine must serialize"),
        }
    }

    pub fn tunnel_changed(tunnels: &[TunnelInfo]) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "tunnel.changed".to_string(),
            params: serde_json::to_value(tunnels).expect("TunnelInfo must serialize"),
        }
    }
}

/// A structured log line, broadcast over IPC to subscribed clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub timestamp: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

impl RpcResponse {
    pub fn success(id: u64, result: impl Serialize) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(serde_json::to_value(result).expect("result must be serializable")),
            error: None,
        }
    }

    pub fn error(id: u64, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// -- Standard JSON-RPC 2.0 error codes --

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

// Application-specific codes (reserved range -32000..-32099)
pub const TUNNEL_NOT_FOUND: i32 = -32000;
pub const TUNNEL_ALREADY_CONNECTED: i32 = -32001;
pub const TUNNEL_NOT_CONNECTED: i32 = -32002;
pub const CONFIG_ERROR: i32 = -32003;
pub const TUNNEL_ALREADY_EXISTS: i32 = -32004;
pub const VALIDATION_ERROR: i32 = -32005;

// -- Domain types (shared between requests and responses) --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelStatus {
    Connected,
    Disconnected,
    Connecting,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TunnelStats {
    pub total_connections: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_session_start: Option<String>,
    pub total_uptime_seconds: u64,
    pub reconnect_count: u64,
}

/// Tunnel info returned by tunnel.list and tunnel.get.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub tunnel_type: String,
    pub mode: String,
    pub status: TunnelStatus,
    pub local_port: u16,
    /// e.g. "db.internal:5432" for local, "SOCKS5" for socks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    pub host: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<TunnelStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub version: String,
    pub uptime_seconds: u64,
    pub tunnel_count: usize,
    pub started_at: String,
}

/// Result of a bulk operation (connect-all, disconnect-all, restart-all).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkResult {
    pub succeeded: u32,
    pub failed: u32,
    pub errors: Vec<String>,
}

/// Result of a config reload operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadResult {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub updated: Vec<String>,
    pub errors: Vec<String>,
}

// -- Typed request enum --

/// Parsed request ready for the daemon to dispatch.
#[derive(Debug)]
pub enum Request {
    TunnelList,
    TunnelGet { id: String },
    TunnelConnect { id: String },
    TunnelDisconnect { id: String },
    TunnelEnable { id: String },
    TunnelDisable { id: String },
    TunnelConnectAll,
    TunnelDisconnectAll,
    TunnelRestartAll,
    DaemonStatus,
    DaemonShutdown,
    TunnelAdd { id: String, config: Value },
    TunnelUpdate { id: String, config: Value },
    TunnelRemove { id: String, force: bool },
    ConfigReload,
    LogsSubscribe { last_n: u64 },
    EventsSubscribe,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("invalid params: {0}")]
    InvalidParams(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
struct TunnelIdParams {
    id: String,
}

#[derive(Debug, Deserialize)]
struct TunnelAddParams {
    id: String,
    config: Value,
}

#[derive(Debug, Deserialize)]
struct TunnelRemoveParams {
    id: String,
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
struct LogsSubscribeParams {
    #[serde(default = "default_last_n")]
    last_n: u64,
}

fn default_last_n() -> u64 {
    100
}

impl Request {
    /// Parse a typed Request from a raw JSON-RPC request.
    pub fn from_rpc(req: &RpcRequest) -> Result<Self, ProtocolError> {
        match req.method.as_str() {
            "tunnel.list" => Ok(Self::TunnelList),
            "tunnel.get" => Ok(Self::TunnelGet {
                id: parse_tunnel_id(&req.params)?,
            }),
            "tunnel.connect" => Ok(Self::TunnelConnect {
                id: parse_tunnel_id(&req.params)?,
            }),
            "tunnel.disconnect" => Ok(Self::TunnelDisconnect {
                id: parse_tunnel_id(&req.params)?,
            }),
            "tunnel.enable" => Ok(Self::TunnelEnable {
                id: parse_tunnel_id(&req.params)?,
            }),
            "tunnel.disable" => Ok(Self::TunnelDisable {
                id: parse_tunnel_id(&req.params)?,
            }),
            "tunnel.add" => {
                let p: TunnelAddParams = serde_json::from_value(req.params.clone())?;
                Ok(Self::TunnelAdd {
                    id: p.id,
                    config: p.config,
                })
            }
            "tunnel.update" => {
                let p: TunnelAddParams = serde_json::from_value(req.params.clone())?;
                Ok(Self::TunnelUpdate {
                    id: p.id,
                    config: p.config,
                })
            }
            "tunnel.remove" => {
                let p: TunnelRemoveParams = serde_json::from_value(req.params.clone())?;
                Ok(Self::TunnelRemove {
                    id: p.id,
                    force: p.force,
                })
            }
            "tunnel.connect_all" => Ok(Self::TunnelConnectAll),
            "tunnel.disconnect_all" => Ok(Self::TunnelDisconnectAll),
            "tunnel.restart_all" => Ok(Self::TunnelRestartAll),
            "daemon.status" => Ok(Self::DaemonStatus),
            "daemon.shutdown" => Ok(Self::DaemonShutdown),
            "config.reload" => Ok(Self::ConfigReload),
            "logs.subscribe" => {
                let p: LogsSubscribeParams = serde_json::from_value(req.params.clone())?;
                Ok(Self::LogsSubscribe { last_n: p.last_n })
            }
            "events.subscribe" => Ok(Self::EventsSubscribe),
            other => Err(ProtocolError::UnknownMethod(other.to_string())),
        }
    }
}

fn parse_tunnel_id(params: &Value) -> Result<String, ProtocolError> {
    let p: TunnelIdParams = serde_json::from_value(params.clone())?;
    Ok(p.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_rpc(method: &str, params: Value) -> RpcRequest {
        RpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: 1,
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn parse_tunnel_list() {
        let req = make_rpc("tunnel.list", json!({}));
        assert!(matches!(Request::from_rpc(&req).unwrap(), Request::TunnelList));
    }

    #[test]
    fn parse_tunnel_get() {
        let req = make_rpc("tunnel.get", json!({"id": "dev-db"}));
        match Request::from_rpc(&req).unwrap() {
            Request::TunnelGet { id } => assert_eq!(id, "dev-db"),
            other => panic!("expected TunnelGet, got {other:?}"),
        }
    }

    #[test]
    fn parse_tunnel_connect() {
        let req = make_rpc("tunnel.connect", json!({"id": "dev-db"}));
        assert!(matches!(
            Request::from_rpc(&req).unwrap(),
            Request::TunnelConnect { .. }
        ));
    }

    #[test]
    fn parse_tunnel_disconnect() {
        let req = make_rpc("tunnel.disconnect", json!({"id": "x"}));
        match Request::from_rpc(&req).unwrap() {
            Request::TunnelDisconnect { id } => assert_eq!(id, "x"),
            other => panic!("expected TunnelDisconnect, got {other:?}"),
        }
    }

    #[test]
    fn parse_daemon_status() {
        let req = make_rpc("daemon.status", json!({}));
        assert!(matches!(Request::from_rpc(&req).unwrap(), Request::DaemonStatus));
    }

    #[test]
    fn parse_daemon_shutdown() {
        let req = make_rpc("daemon.shutdown", json!({}));
        assert!(matches!(Request::from_rpc(&req).unwrap(), Request::DaemonShutdown));
    }

    #[test]
    fn unknown_method_rejected() {
        let req = make_rpc("bogus.method", json!({}));
        assert!(matches!(
            Request::from_rpc(&req),
            Err(ProtocolError::UnknownMethod(_))
        ));
    }

    #[test]
    fn missing_id_param_rejected() {
        let req = make_rpc("tunnel.get", json!({}));
        assert!(matches!(
            Request::from_rpc(&req),
            Err(ProtocolError::InvalidParams(_))
        ));
    }

    #[test]
    fn rpc_response_success_roundtrip() {
        let resp = RpcResponse::success(42, json!({"tunnels": []}));
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: RpcResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 42);
        assert!(parsed.error.is_none());
        assert!(parsed.result.is_some());
    }

    #[test]
    fn rpc_response_error_roundtrip() {
        let resp = RpcResponse::error(7, TUNNEL_NOT_FOUND, "tunnel 'x' not found");
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: RpcResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 7);
        assert!(parsed.result.is_none());
        let err = parsed.error.unwrap();
        assert_eq!(err.code, TUNNEL_NOT_FOUND);
    }

    #[test]
    fn tunnel_info_serialization() {
        let info = TunnelInfo {
            id: "dev-db".to_string(),
            name: "Dev Database".to_string(),
            tunnel_type: "local".to_string(),
            mode: "auto".to_string(),
            status: TunnelStatus::Connected,
            local_port: 5432,
            remote: Some("db.internal:5432".to_string()),
            host: "bastion.example.com".to_string(),
            enabled: true,
            last_error: None,
            stats: None,
        };
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["type"], "local");
        assert_eq!(v["status"], "connected");
        // last_error and stats should be absent (skip_serializing_if)
        assert!(v.get("last_error").is_none());
        assert!(v.get("stats").is_none());
    }

    #[test]
    fn rpc_request_deserialize_from_wire() {
        let wire = r#"{"jsonrpc":"2.0","id":1,"method":"tunnel.list","params":{}}"#;
        let req: RpcRequest = serde_json::from_str(wire).unwrap();
        assert_eq!(req.method, "tunnel.list");
        assert_eq!(req.id, 1);
    }
}
