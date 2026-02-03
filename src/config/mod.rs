pub mod sample;
pub mod schema;
pub mod validation;

use std::path::{Path, PathBuf};

pub use schema::{Config, Defaults, TunnelConfig, TunnelMode, TunnelType};
pub use validation::{expand_tilde, is_valid_tunnel_id, validate_config};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("config validation failed:\n{}", format_errors(.0))]
    Validation(Vec<validation::ValidationError>),
    #[error("config file not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("failed to serialize config: {0}")]
    Serialize(String),
    #[error("failed to write config: {0}")]
    Write(String),
}

fn format_errors(errors: &[validation::ValidationError]) -> String {
    errors
        .iter()
        .map(|e| format!("  - {e}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Platform-appropriate config file path.
///
/// macOS: ~/Library/Application Support/Burrow/config.toml
/// Linux: ~/.config/burrow/config.toml (XDG)
pub fn config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        directories::BaseDirs::new()
            .expect("cannot determine home directory")
            .home_dir()
            .join("Library/Application Support/Burrow/config.toml")
    }
    #[cfg(not(target_os = "macos"))]
    {
        directories::ProjectDirs::from("", "", "burrow")
            .expect("cannot determine config directory")
            .config_dir()
            .join("config.toml")
    }
}

/// Load and validate config from the default platform path.
pub fn load_config() -> Result<Config, ConfigError> {
    load_config_from(&config_path())
}

/// Load and validate config from a specific path.
pub fn load_config_from(path: &Path) -> Result<Config, ConfigError> {
    if !path.exists() {
        return Err(ConfigError::NotFound(path.to_path_buf()));
    }

    let contents = std::fs::read_to_string(path)?;
    let mut config: Config = toml::from_str(&contents)?;

    // Expand ~ in identity paths before validation checks file existence
    for tunnel in config.tunnel.values_mut() {
        if let Some(ref identity) = tunnel.identity {
            let expanded = expand_tilde(identity);
            tunnel.identity = Some(expanded.to_string_lossy().into_owned());
        }
    }

    let result = validate_config(&config);

    for warning in &result.warnings {
        tracing::warn!("{}", warning.message);
    }

    if !result.is_ok() {
        return Err(ConfigError::Validation(result.errors));
    }

    Ok(config)
}

/// Atomically write config to the given path (write tmp + rename).
pub fn save_config_to(path: &Path, config: &Config) -> Result<(), ConfigError> {
    let toml_str =
        toml::to_string_pretty(config).map_err(|e| ConfigError::Serialize(e.to_string()))?;

    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::Write("config path has no parent directory".into()))?;
    std::fs::create_dir_all(parent)?;

    let tmp_path = parent.join(".config.toml.tmp");
    std::fs::write(&tmp_path, toml_str.as_bytes())?;
    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        ConfigError::Write(format!("rename failed: {e}"))
    })?;

    Ok(())
}

/// Save config to the default platform path.
pub fn save_config(config: &Config) -> Result<(), ConfigError> {
    save_config_to(&config_path(), config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_valid_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"
[defaults]
ssh_binary = "ssh"

[tunnel.dev-db]
name = "Dev Database"
host = "bastion.example.com"
type = "local"
local_port = 5432
remote_host = "db.internal"
remote_port = 5432
"#
        )
        .unwrap();

        let config = load_config_from(&path).unwrap();
        assert_eq!(config.tunnel.len(), 1);
        assert!(config.tunnel.contains_key("dev-db"));
    }

    #[test]
    fn load_missing_file_returns_not_found() {
        let result = load_config_from(Path::new("/nonexistent/config.toml"));
        assert!(matches!(result, Err(ConfigError::NotFound(_))));
    }

    #[test]
    fn load_invalid_toml_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not valid toml [[[").unwrap();

        let result = load_config_from(&path);
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[test]
    fn load_invalid_config_returns_validation_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // local tunnel missing remote_host and remote_port
        std::fs::write(
            &path,
            r#"
[tunnel.bad]
name = "Bad"
host = "example.com"
type = "local"
local_port = 5432
"#,
        )
        .unwrap();

        let result = load_config_from(&path);
        assert!(matches!(result, Err(ConfigError::Validation(_))));
    }

    #[test]
    fn identity_tilde_expanded_before_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[tunnel.t]
name = "T"
host = "example.com"
type = "socks"
local_port = 1080
identity = "~/.ssh/id_rsa"
"#,
        )
        .unwrap();

        // Should load successfully (identity not found is just a warning)
        let config = load_config_from(&path).unwrap();
        let identity = config.tunnel["t"].identity.as_ref().unwrap();
        assert!(!identity.starts_with('~'));
    }

    #[test]
    fn save_and_reload_roundtrip() {
        use crate::config::schema::{Defaults, TunnelConfig, TunnelMode, TunnelType};
        use std::collections::HashMap;

        let mut tunnels = HashMap::new();
        tunnels.insert(
            "dev-db".to_string(),
            TunnelConfig {
                name: "Dev Database".to_string(),
                host: "bastion.example.com".to_string(),
                port: None,
                tunnel_type: TunnelType::Local,
                mode: TunnelMode::Auto,
                local_port: 5432,
                remote_host: Some("db.internal".to_string()),
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
        let config = Config {
            defaults: Defaults::default(),
            tunnel: tunnels,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        save_config_to(&path, &config).unwrap();
        let loaded = load_config_from(&path).unwrap();

        assert_eq!(loaded.tunnel.len(), 1);
        let t = &loaded.tunnel["dev-db"];
        assert_eq!(t.name, "Dev Database");
        assert_eq!(t.host, "bastion.example.com");
        assert_eq!(t.tunnel_type, TunnelType::Local);
        assert_eq!(t.local_port, 5432);
        assert_eq!(t.remote_host.as_deref(), Some("db.internal"));
        assert_eq!(t.remote_port, Some(5432));
    }
}
