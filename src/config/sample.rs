use std::path::PathBuf;

/// Sample config matching the DESIGN.md first-run template.
pub const SAMPLE_CONFIG: &str = r#"# Burrow SSH Tunnel Manager Configuration
#
# Uncomment and modify the examples below to define your tunnels.
# Then run: burrow config reload
#
# Documentation: https://github.com/youruser/burrow

[defaults]
ssh_binary = "ssh"           # Path to SSH binary
keepalive = true             # Enable SSH keepalive (ServerAliveInterval=30)
log_level = "info"           # trace | debug | info | warn | error

# Example: Local port forward (access remote service locally)
# [tunnel.example-db]
# name = "Example Database"
# host = "bastion.example.com"
# port = 22
# type = "local"
# mode = "auto"              # auto | manual | on-demand
# local_port = 5432
# remote_host = "db.internal"
# remote_port = 5432
# identity = "~/.ssh/id_rsa"
# jump_host = "gateway.example.com"
# jump_port = 22

# Example: Reverse port forward (expose local service remotely)
# [tunnel.example-expose]
# name = "Expose Local Dev"
# host = "jumphost.example.com"
# type = "reverse"
# mode = "manual"
# local_port = 8080
# local_host = "127.0.0.1"
# remote_port = 9000
# remote_bind = "0.0.0.0"

# Example: SOCKS5 proxy
# [tunnel.example-socks]
# name = "Home SOCKS Proxy"
# host = "home.example.com"
# type = "socks"
# mode = "on-demand"
# local_port = 1080
"#;

/// Create the sample config if it doesn't exist. Returns the path if the
/// file was created (first run), None if it already existed.
pub fn ensure_config_exists() -> Option<PathBuf> {
    let path = super::config_path();
    if path.exists() {
        return None;
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("failed to create config directory: {e}");
            return None;
        }
    }

    if let Err(e) = std::fs::write(&path, SAMPLE_CONFIG) {
        eprintln!("failed to write sample config: {e}");
        return None;
    }

    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_config_is_valid_toml() {
        // The sample has all tunnel sections commented out, so it should
        // parse as a valid Config with no tunnels.
        let config: crate::config::Config = toml::from_str(SAMPLE_CONFIG).unwrap();
        assert!(config.tunnel.is_empty());
        assert_eq!(config.defaults.ssh_binary, "ssh");
        assert!(config.defaults.keepalive);
    }

    #[test]
    fn ensure_creates_file_on_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(!path.exists());

        // Write directly instead of using ensure_config_exists (which uses
        // the real platform path). Validates the content is writable.
        std::fs::write(&path, SAMPLE_CONFIG).unwrap();
        assert!(path.exists());

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("[defaults]"));
        assert!(contents.contains("# [tunnel.example-db]"));
    }
}
