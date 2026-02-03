use std::path::PathBuf;

use crate::config::schema::{Config, TunnelType};

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("tunnel '{0}': invalid ID (must be lowercase alphanumeric with hyphens)")]
    InvalidTunnelId(String),
    #[error("tunnel '{0}': name must not be empty")]
    EmptyName(String),
    #[error("tunnel '{0}': local_port {1} conflicts with tunnel '{2}'")]
    PortConflict(String, u16, String),
    #[error("tunnel '{0}': type 'local' requires remote_host")]
    MissingRemoteHost(String),
    #[error("tunnel '{0}': type 'local' requires remote_port")]
    MissingRemotePort(String),
    #[error("tunnel '{0}': type 'reverse' requires remote_port")]
    MissingReverseRemotePort(String),
}

impl ValidationError {
    pub fn tunnel_id(&self) -> &str {
        match self {
            Self::InvalidTunnelId(id)
            | Self::EmptyName(id)
            | Self::MissingRemoteHost(id)
            | Self::MissingRemotePort(id)
            | Self::MissingReverseRemotePort(id) => id,
            Self::PortConflict(id, _, _) => id,
        }
    }

    /// Which config field this error relates to (None for section-level issues).
    #[allow(dead_code)] // useful accessor for future error display
    pub fn field_name(&self) -> Option<&str> {
        match self {
            Self::InvalidTunnelId(_) => None,
            Self::EmptyName(_) => Some("name"),
            Self::PortConflict(..) => Some("local_port"),
            Self::MissingRemoteHost(_) => Some("remote_host"),
            Self::MissingRemotePort(_) | Self::MissingReverseRemotePort(_) => Some("remote_port"),
        }
    }

    pub fn suggestion(&self) -> &str {
        match self {
            Self::InvalidTunnelId(_) => {
                "use lowercase alphanumeric with hyphens, e.g. 'my-tunnel'"
            }
            Self::EmptyName(_) => "add: name = \"My Tunnel\"",
            Self::PortConflict(..) => "each tunnel must use a unique local_port",
            Self::MissingRemoteHost(_) => "add: remote_host = \"hostname\"",
            Self::MissingRemotePort(_) | Self::MissingReverseRemotePort(_) => {
                "add: remote_port = <port>"
            }
        }
    }
}

#[derive(Debug)]
pub struct ValidationWarning {
    /// Tunnel ID if tunnel-specific, None for global warnings.
    pub tunnel_id: Option<String>,
    pub message: String,
}

pub struct ValidationResult {
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
}

impl ValidationResult {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate the full config. Returns all errors and warnings at once
/// so the user can fix everything in one pass.
pub fn validate_config(config: &Config) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let mut port_owners: std::collections::HashMap<u16, &str> = std::collections::HashMap::new();
    let mut names_seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();

    for (id, tunnel) in &config.tunnel {
        // Tunnel ID: lowercase alphanumeric + hyphens
        if !is_valid_tunnel_id(id) {
            errors.push(ValidationError::InvalidTunnelId(id.clone()));
        }

        // Name must be non-empty
        if tunnel.name.trim().is_empty() {
            errors.push(ValidationError::EmptyName(id.clone()));
        }

        // Name uniqueness (warning, not error per DESIGN.md)
        if let Some(&other_id) = names_seen.get(tunnel.name.as_str()) {
            warnings.push(ValidationWarning {
                tunnel_id: Some(id.clone()),
                message: format!(
                    "tunnel '{}': name '{}' is also used by tunnel '{}'",
                    id, tunnel.name, other_id
                ),
            });
        } else {
            names_seen.insert(&tunnel.name, id);
        }

        // local_port must be unique across all tunnels
        if let Some(&other_id) = port_owners.get(&tunnel.local_port) {
            errors.push(ValidationError::PortConflict(
                id.clone(),
                tunnel.local_port,
                other_id.to_string(),
            ));
        } else {
            port_owners.insert(tunnel.local_port, id);
        }

        // Type-specific required fields
        match tunnel.tunnel_type {
            TunnelType::Local => {
                if tunnel.remote_host.is_none() {
                    errors.push(ValidationError::MissingRemoteHost(id.clone()));
                }
                if tunnel.remote_port.is_none() {
                    errors.push(ValidationError::MissingRemotePort(id.clone()));
                }
            }
            TunnelType::Reverse => {
                if tunnel.remote_port.is_none() {
                    errors.push(ValidationError::MissingReverseRemotePort(id.clone()));
                }
            }
            TunnelType::Socks => {}
        }

        // Warn if identity file doesn't exist (path already expanded by loader)
        if let Some(ref identity) = tunnel.identity {
            let path = PathBuf::from(identity);
            if !path.exists() {
                warnings.push(ValidationWarning {
                    tunnel_id: Some(id.clone()),
                    message: format!(
                        "tunnel '{}': identity file not found: {}",
                        id,
                        path.display()
                    ),
                });
            }
        }

        // Check per-tunnel SSH binary override
        if let Some(ref binary) = tunnel.ssh_binary {
            if let Some(msg) = check_ssh_binary(binary) {
                warnings.push(ValidationWarning {
                    tunnel_id: Some(id.clone()),
                    message: format!("tunnel '{}': {}", id, msg),
                });
            }
        }
    }

    // Check default SSH binary
    if let Some(msg) = check_ssh_binary(&config.defaults.ssh_binary) {
        warnings.push(ValidationWarning {
            tunnel_id: None,
            message: format!("defaults: {}", msg),
        });
    }

    ValidationResult { errors, warnings }
}

/// Check if an SSH binary exists and is executable.
/// Returns an error message if not found, None if OK.
fn check_ssh_binary(binary: &str) -> Option<String> {
    let path = std::path::Path::new(binary);
    if path.is_absolute() {
        if !path.exists() {
            return Some(format!("SSH binary not found: {binary}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = path.metadata() {
                if meta.permissions().mode() & 0o111 == 0 {
                    return Some(format!("SSH binary not executable: {binary}"));
                }
            }
        }
    } else if !find_in_path(binary) {
        return Some(format!("SSH binary '{binary}' not found in PATH"));
    }
    None
}

/// Search PATH for a binary by name.
fn find_in_path(name: &str) -> bool {
    let Ok(path_var) = std::env::var("PATH") else {
        return false;
    };
    for dir in path_var.split(':') {
        let candidate = std::path::Path::new(dir).join(name);
        if candidate.is_file() {
            return true;
        }
    }
    false
}

/// Tunnel IDs must be lowercase alphanumeric with hyphens, non-empty,
/// and must not start or end with a hyphen.
pub fn is_valid_tunnel_id(id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// Expand `~` prefix to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    } else if path == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    PathBuf::from(path)
}

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{Config, Defaults, TunnelConfig, TunnelMode, TunnelType};

    fn local_tunnel(name: &str, port: u16) -> TunnelConfig {
        TunnelConfig {
            name: name.to_string(),
            host: "example.com".to_string(),
            port: None,
            tunnel_type: TunnelType::Local,
            mode: TunnelMode::Auto,
            local_port: port,
            remote_host: Some("db.internal".to_string()),
            remote_port: Some(5432),
            local_host: None,
            remote_bind: None,
            identity: None,
            jump_host: None,
            jump_port: None,
            ssh_binary: None,
            keepalive: None,
        }
    }

    fn make_config(tunnels: Vec<(&str, TunnelConfig)>) -> Config {
        Config {
            defaults: Defaults::default(),
            tunnel: tunnels
                .into_iter()
                .map(|(id, t)| (id.to_string(), t))
                .collect::<indexmap::IndexMap<_, _>>(),
        }
    }

    #[test]
    fn valid_tunnel_ids() {
        assert!(is_valid_tunnel_id("dev-db"));
        assert!(is_valid_tunnel_id("a"));
        assert!(is_valid_tunnel_id("my-tunnel-123"));
        assert!(!is_valid_tunnel_id(""));
        assert!(!is_valid_tunnel_id("-bad"));
        assert!(!is_valid_tunnel_id("bad-"));
        assert!(!is_valid_tunnel_id("Has-Caps"));
        assert!(!is_valid_tunnel_id("has spaces"));
        assert!(!is_valid_tunnel_id("under_score"));
    }

    #[test]
    fn valid_config_passes() {
        let config = make_config(vec![("dev-db", local_tunnel("Dev DB", 5432))]);
        let result = validate_config(&config);
        assert!(result.is_ok());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn duplicate_port_is_error() {
        let config = make_config(vec![
            ("tunnel-a", local_tunnel("Tunnel A", 5432)),
            ("tunnel-b", local_tunnel("Tunnel B", 5432)),
        ]);
        let result = validate_config(&config);
        assert!(!result.is_ok());
        assert!(result.errors.iter().any(|e| matches!(e, ValidationError::PortConflict(..))));
    }

    #[test]
    fn duplicate_name_is_warning() {
        let config = make_config(vec![
            ("tunnel-a", local_tunnel("Same Name", 5432)),
            ("tunnel-b", local_tunnel("Same Name", 5433)),
        ]);
        let result = validate_config(&config);
        assert!(result.is_ok()); // warnings don't block
        assert!(!result.warnings.is_empty());
    }

    #[test]
    fn local_requires_remote_fields() {
        let mut t = local_tunnel("Bad", 5432);
        t.remote_host = None;
        t.remote_port = None;
        let config = make_config(vec![("bad", t)]);
        let result = validate_config(&config);
        assert!(!result.is_ok());
        assert!(result.errors.iter().any(|e| matches!(e, ValidationError::MissingRemoteHost(..))));
        assert!(result.errors.iter().any(|e| matches!(e, ValidationError::MissingRemotePort(..))));
    }

    #[test]
    fn socks_needs_no_remote_fields() {
        let t = TunnelConfig {
            name: "SOCKS".to_string(),
            host: "example.com".to_string(),
            port: None,
            tunnel_type: TunnelType::Socks,
            mode: TunnelMode::Auto,
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
        };
        let config = make_config(vec![("proxy", t)]);
        let result = validate_config(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn reverse_requires_remote_port() {
        let t = TunnelConfig {
            name: "Reverse".to_string(),
            host: "example.com".to_string(),
            port: None,
            tunnel_type: TunnelType::Reverse,
            mode: TunnelMode::Manual,
            local_port: 8080,
            remote_host: None,
            remote_port: None,
            local_host: None,
            remote_bind: None,
            identity: None,
            jump_host: None,
            jump_port: None,
            ssh_binary: None,
            keepalive: None,
        };
        let config = make_config(vec![("rev", t)]);
        let result = validate_config(&config);
        assert!(!result.is_ok());
        assert!(result
            .errors
            .iter()
            .any(|e| matches!(e, ValidationError::MissingReverseRemotePort(..))));
    }

    #[test]
    fn empty_name_is_error() {
        let mut t = local_tunnel("", 5432);
        t.name = "   ".to_string();
        let config = make_config(vec![("bad", t)]);
        let result = validate_config(&config);
        assert!(result.errors.iter().any(|e| matches!(e, ValidationError::EmptyName(..))));
    }

    #[test]
    fn expand_tilde_works() {
        let expanded = expand_tilde("~/.ssh/id_rsa");
        let s = expanded.to_string_lossy();
        assert!(!s.starts_with('~'));
        assert!(s.ends_with(".ssh/id_rsa"));
    }

    #[test]
    fn expand_tilde_no_prefix() {
        let expanded = expand_tilde("/absolute/path");
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn ssh_binary_absolute_not_found() {
        let msg = check_ssh_binary("/nonexistent/ssh");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("not found"));
    }

    #[test]
    fn ssh_binary_in_path_found() {
        // "ssh" should exist on any dev machine
        let msg = check_ssh_binary("ssh");
        assert!(msg.is_none());
    }

    #[test]
    fn ssh_binary_bogus_name_not_in_path() {
        let msg = check_ssh_binary("definitely-not-a-real-binary-xyz");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("not found in PATH"));
    }

    #[test]
    fn validation_error_metadata() {
        let e = ValidationError::MissingRemoteHost("dev-db".into());
        assert_eq!(e.tunnel_id(), "dev-db");
        assert_eq!(e.field_name(), Some("remote_host"));
        assert!(!e.suggestion().is_empty());
    }

    #[test]
    fn bad_ssh_binary_produces_warning() {
        let mut t = local_tunnel("Test", 5432);
        t.ssh_binary = Some("/nonexistent/custom-ssh".into());
        let config = make_config(vec![("test", t)]);
        let result = validate_config(&config);
        assert!(result.warnings.iter().any(|w| w.message.contains("SSH binary not found")));
    }
}
