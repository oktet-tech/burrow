use crate::ipc::protocol;

/// Failure of a tunnel lifecycle operation, mapped to a distinct RPC code so
/// clients can tell "no such tunnel" from "port busy" or "ssh won't start".
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("tunnel '{0}' not found")]
    NotFound(String),
    #[error("tunnel '{0}' is already connected")]
    AlreadyConnected(String),
    #[error("failed to start tunnel '{id}': port {port} already in use")]
    PortInUse { id: String, port: u16 },
    #[error("failed to start tunnel '{id}': {source}")]
    Start { id: String, source: std::io::Error },
}

impl TunnelError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            Self::NotFound(_) => protocol::TUNNEL_NOT_FOUND,
            Self::AlreadyConnected(_) => protocol::TUNNEL_ALREADY_CONNECTED,
            Self::PortInUse { .. } => protocol::PORT_IN_USE,
            Self::Start { .. } => protocol::TUNNEL_START_FAILED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_distinct() {
        let errors = [
            TunnelError::NotFound("a".into()),
            TunnelError::AlreadyConnected("a".into()),
            TunnelError::PortInUse {
                id: "a".into(),
                port: 1,
            },
            TunnelError::Start {
                id: "a".into(),
                source: std::io::ErrorKind::NotFound.into(),
            },
        ];
        let codes: std::collections::HashSet<i32> = errors.iter().map(|e| e.rpc_code()).collect();
        assert_eq!(codes.len(), errors.len());
    }
}
