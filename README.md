# Burrow

SSH tunnel manager for macOS and Linux. Runs as a background daemon with auto-reconnect, network-aware lifecycle, and a menu bar GUI.

## Features

- **Tunnel types** -- local (`-L`), reverse (`-R`), SOCKS5 (`-D`)
- **Connection modes** -- auto (start on launch), manual (explicit), on-demand (connect on first use)
- **Auto-reconnect** with exponential backoff
- **Network detection** -- reconnects tunnels when connectivity changes (macOS)
- **On-demand listeners** -- stub sockets that trigger tunnel setup on first connection
- **Menu bar GUI** -- system tray icon, tunnel list window
- **CLI** -- full control over tunnels, daemon, config, and service management

## Install

Requires a [Rust toolchain](https://rustup.rs/).

```bash
cargo install --path .
```

### macOS .app bundle

```bash
./scripts/bundle.sh
cp -r target/release/bundle/Burrow.app /Applications/
```

## Quick start

```bash
# Add a tunnel
burrow tunnel add dev-db \
  --name "Dev Database" \
  --host bastion.example.com \
  --type local \
  --local-port 5432 \
  --remote-host db.internal \
  --remote-port 5432

# Start the daemon (connects auto-mode tunnels)
burrow daemon start

# Check status
burrow status
```

## Configuration

| Platform | Path |
|----------|------|
| macOS    | `~/Library/Application Support/Burrow/config.toml` |
| Linux    | `~/.config/burrow/config.toml` |

A sample config is generated on first run. Minimal example:

```toml
[defaults]
keepalive = true

[tunnel.dev-db]
name = "Dev Database"
host = "bastion.example.com"
type = "local"
local_port = 5432
remote_host = "db.internal"
remote_port = 5432
```

Validate with `burrow config validate`. See [DESIGN.md](DESIGN.md) for full schema.

## CLI reference

```
burrow                              Show all tunnel statuses
burrow status                       Same as above
burrow connect <id>                 Connect a tunnel
burrow disconnect <id>              Disconnect a tunnel
burrow connect-all                  Connect all enabled tunnels
burrow disconnect-all               Disconnect all tunnels
burrow restart-all                  Restart all tunnels
burrow enable <id>                  Enable a tunnel
burrow disable <id>                 Disable a tunnel
burrow logs [--follow] [--tunnel]   View logs

burrow tunnel add <id> [flags]      Add a tunnel
burrow tunnel remove <id>           Remove a tunnel
burrow tunnel modify <id> [flags]   Modify a tunnel
burrow tunnel show <id>             Show tunnel config

burrow config path                  Print config file path
burrow config edit                  Open config in $EDITOR
burrow config validate              Validate config
burrow config reload                Reload config in running daemon

burrow daemon start|stop|restart    Manage the daemon
burrow daemon status                Daemon info

burrow service install|uninstall    Manage launchd/systemd service

burrow gui                          Launch menu bar app
```

See [DESIGN.md](DESIGN.md) for full flag details.

## GUI

Launch via `burrow gui` or open `Burrow.app`. Provides a system tray icon with a menu bar window showing tunnel status. The daemon runs independently -- the GUI connects over IPC.

## Service install

Register the daemon to start at login:

```bash
burrow service install    # launchd (macOS) or systemd (Linux)
burrow service uninstall  # remove
```

## Platform support

| Platform | Status | Notes |
|----------|--------|-------|
| macOS    | Primary | Full support including network detection, launchd service, .app bundle |
| Linux    | Secondary | Core functionality works; network detection not yet implemented |
