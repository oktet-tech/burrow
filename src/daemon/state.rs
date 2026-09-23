use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Persisted daemon state, written to state.json.
/// Config defines what tunnels exist; state tracks runtime info across restarts.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub tunnels: HashMap<String, PersistedTunnel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_started: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedTunnel {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_connected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub stats: PersistedTunnelStats,
}

impl Default for PersistedTunnel {
    fn default() -> Self {
        Self {
            enabled: true,
            last_connected: None,
            last_error: None,
            stats: PersistedTunnelStats::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedTunnelStats {
    pub total_connections: u64,
    pub total_uptime_seconds: u64,
    pub reconnect_count: u64,
}

/// Platform-appropriate state file path.
pub fn state_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        directories::BaseDirs::new()
            .expect("cannot determine home directory")
            .home_dir()
            .join("Library/Application Support/Burrow/state.json")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dirs = directories::ProjectDirs::from("", "", "burrow")
            .expect("cannot determine state directory");
        // state_dir() is None off Linux; `~` isn't expanded by the fs, so
        // fall back to the data dir rather than a literal "~" path.
        dirs.state_dir()
            .unwrap_or_else(|| dirs.data_local_dir())
            .join("state.json")
    }
}

/// Load persisted state. Returns default on any error (missing file, parse failure).
pub fn load_state() -> State {
    load_state_from(&state_path())
}

pub fn load_state_from(path: &Path) -> State {
    match std::fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(state) => {
                tracing::info!("loaded state from {}", path.display());
                state
            }
            Err(e) => {
                tracing::warn!(error = %e, "corrupt state file, starting fresh");
                State::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("no state file, starting fresh");
            State::default()
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to read state file, starting fresh");
            State::default()
        }
    }
}

/// Atomic write: write to .tmp then rename to avoid partial reads.
pub fn save_state_to(path: &Path, state: &State) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json.as_bytes())?;
    std::fs::rename(&tmp_path, path)?;

    tracing::debug!("saved state to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_default() {
        let state = State::default();
        assert!(state.tunnels.is_empty());
        assert!(state.daemon_started.is_none());
    }

    #[test]
    fn roundtrip_serialization() {
        let mut state = State::default();
        state.daemon_started = Some("2025-02-01T08:00:00Z".into());
        state.tunnels.insert(
            "dev-db".into(),
            PersistedTunnel {
                enabled: true,
                last_connected: Some("2025-02-01T10:30:00Z".into()),
                last_error: None,
                stats: PersistedTunnelStats {
                    total_connections: 42,
                    total_uptime_seconds: 36000,
                    reconnect_count: 3,
                },
            },
        );

        let json = serde_json::to_string(&state).unwrap();
        let parsed: State = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.tunnels.len(), 1);
        let t = &parsed.tunnels["dev-db"];
        assert!(t.enabled);
        assert_eq!(t.stats.total_connections, 42);
        assert_eq!(t.stats.reconnect_count, 3);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let state = load_state_from(Path::new("/nonexistent/state.json"));
        assert!(state.tunnels.is_empty());
    }

    #[test]
    fn load_corrupt_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "not json").unwrap();

        let state = load_state_from(&path);
        assert!(state.tunnels.is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut state = State::default();
        state.tunnels.insert(
            "tun-1".into(),
            PersistedTunnel {
                enabled: false,
                last_connected: None,
                last_error: Some("connection refused".into()),
                stats: PersistedTunnelStats {
                    total_connections: 10,
                    total_uptime_seconds: 500,
                    reconnect_count: 2,
                },
            },
        );

        save_state_to(&path, &state).unwrap();
        let loaded = load_state_from(&path);

        assert_eq!(loaded.tunnels.len(), 1);
        let t = &loaded.tunnels["tun-1"];
        assert!(!t.enabled);
        assert_eq!(t.last_error.as_deref(), Some("connection refused"));
        assert_eq!(t.stats.total_connections, 10);
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/state.json");

        let state = State::default();
        save_state_to(&path, &state).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn atomic_write_no_partial() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let tmp_path = path.with_extension("json.tmp");

        let state = State::default();
        save_state_to(&path, &state).unwrap();

        // Temp file should not linger after successful save
        assert!(!tmp_path.exists());
        assert!(path.exists());
    }

    #[test]
    fn disabled_tunnel_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let json = r#"{
            "tunnels": {
                "my-tunnel": {
                    "enabled": false,
                    "stats": { "total_connections": 0, "total_uptime_seconds": 0, "reconnect_count": 0 }
                }
            }
        }"#;
        std::fs::write(&path, json).unwrap();

        let state = load_state_from(&path);
        assert!(!state.tunnels["my-tunnel"].enabled);
    }

    #[test]
    fn partial_json_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        // Minimal valid JSON with missing optional fields
        let json = r#"{ "tunnels": { "t1": { "enabled": true } } }"#;
        std::fs::write(&path, json).unwrap();

        let state = load_state_from(&path);
        let t = &state.tunnels["t1"];
        assert!(t.enabled);
        assert!(t.last_connected.is_none());
        assert_eq!(t.stats.total_connections, 0);
    }
}
