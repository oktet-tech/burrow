# Burrow - SSH Tunnel Manager

A macOS/Linux application for managing persistent SSH tunnels.

**Read DESIGN.md for full architecture, config schema, IPC protocol, and CLI specification.**

## Project Structure

```
burrow/
├── CLAUDE.md          # This file - development guidance
├── DESIGN.md          # Architecture and requirements (source of truth)
├── Cargo.toml
└── src/
    ├── main.rs        # Entry point, CLI dispatch
    ├── cli/           # CLI commands
    ├── daemon/        # Background daemon
    ├── gui/           # Menu bar app (Phase 4)
    ├── config/        # Config parsing and validation
    ├── ipc/           # IPC protocol and client/server
    └── common/        # Shared utilities
```

## Development Principles

1. **Compile often** — Run `cargo build` after each significant change
2. **Small modules** — Keep files focused, under 300 lines when possible
3. **Error handling** — Use `thiserror` for error types, propagate with `?`
4. **Logging** — Use `tracing` crate, not `println!`
5. **No unwrap in library code** — Use `expect()` only in main.rs or tests with clear messages
6. **Test as you go** — Add unit tests for parsing, validation, protocol types

## Code Conventions

```rust
// Error types with thiserror
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    ReadError(#[from] std::io::Error),
    #[error("invalid config: {0}")]
    ParseError(#[from] toml::de::Error),
    #[error("validation failed: {0}")]
    ValidationError(String),
}

// Use tracing for logs
tracing::info!(tunnel_id = %id, "connecting");
tracing::error!(error = ?e, "connection failed");

// Async with tokio
#[tokio::main]
async fn main() -> Result<()> { ... }
```

## Dependencies (add as needed)

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive"] }
directories = "5"

# Add later phases:
# notify-rust = "4"        # Phase 3: notifications
# iced = "0.13"            # Phase 4: GUI
# tray-icon = "0.19"       # Phase 4: system tray
# muda = "0.15"            # Phase 4: native menus
```

## Development Phases

### Phase 1: Foundation ← CURRENT
- [x] Config module (schema.rs, validation.rs, loading)
- [x] IPC protocol types (JSON-RPC request/response)
- [x] Basic daemon (start, IPC listener, shutdown)
- [x] CLI skeleton (clap, daemon start/stop/status)
- [x] Single tunnel spawn (SSH process, exit detection)
- [x] Basic `burrow status` showing tunnel states

**Skip:** GUI, on-demand mode, network detection, reconnection, notifications

### Phase 2: Tunnel Lifecycle
- [ ] All tunnel types (local, reverse, socks)
- [ ] Reconnection with exponential backoff
- [ ] State persistence (state.json)
- [ ] `tunnel add/remove/modify` CLI commands
- [ ] Config reload without restart
- [ ] `enable/disable` commands
- [ ] `connect-all`, `disconnect-all`, `restart-all`

### Phase 3: Robustness
- [ ] Network change detection (macOS: SCNetworkReachability)
- [ ] On-demand stub listeners
- [ ] Port conflict detection and handling
- [ ] Log rotation
- [ ] First-run sample config generation
- [ ] `burrow service install/uninstall`
- [ ] Linux network detection (netlink)

### Phase 4: GUI
- [ ] Tray icon with status
- [ ] Tray menu (tunnel list, quick actions)
- [ ] Main window (tunnel list view)
- [ ] Log viewer panel
- [ ] Desktop notifications
- [ ] Aggregate tray icon status (green/yellow/red)

## Testing Commands

```bash
# Build
cargo build

# Run CLI
cargo run -- status
cargo run -- daemon start
cargo run -- daemon stop

# Run with debug logging
RUST_LOG=debug cargo run -- daemon start

# Run tests
cargo test

# Check without building
cargo check

# Format
cargo fmt

# Lint
cargo clippy
```

## Platform Notes

**Primary platform: macOS**
- Test on macOS first
- Use `~/Library/Application Support/Burrow/` for config/state
- Use `~/Library/Logs/Burrow/` for logs
- Network detection via `system-configuration` crate

**Secondary platform: Linux**
- Use XDG paths (`~/.config/burrow/`, `~/.local/state/burrow/`)
- Network detection via `rtnetlink` crate
- System tray via `libappindicator`

## Common Tasks

**Adding a new CLI command:**
1. Add variant to CLI enum in `src/cli/mod.rs`
2. Implement handler in `src/cli/commands.rs`
3. Add IPC method if daemon interaction needed

**Adding a new IPC method:**
1. Add request/response types in `src/ipc/protocol.rs`
2. Add handler in `src/daemon/server.rs`
3. Add client method in `src/ipc/client.rs`

**Adding a new config field:**
1. Update struct in `src/config/schema.rs`
2. Update validation in `src/config/validation.rs`
3. Update sample config in first-run generation
4. Update DESIGN.md

## Troubleshooting

**Daemon won't start:** Check if socket file exists (`~/Library/Application Support/Burrow/burrow.sock`). Delete if stale.

**SSH process spawning issues:** Test SSH command manually first. Check identity file permissions (should be 600).

**IPC connection refused:** Ensure daemon is running. Check socket permissions.

