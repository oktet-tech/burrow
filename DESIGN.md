# Burrow — SSH Tunnel Manager

## Overview

Burrow is a macOS/Linux application for managing persistent SSH tunnels (local forwards, reverse forwards, SOCKS proxies). It consists of three components:

1. **Daemon** — Background process managing tunnel lifecycle
2. **Menu Bar App** — GUI for status and quick actions
3. **CLI** — Command-line interface for scripting and control

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         User                                     │
└─────────────────┬───────────────────────────┬───────────────────┘
                  │                           │
                  ▼                           ▼
         ┌───────────────┐           ┌───────────────┐
         │  Menu Bar App │           │      CLI      │
         │    (iced +    │           │               │
         │  tray-icon)   │           │               │
         └───────┬───────┘           └───────┬───────┘
                 │                           │
                 │     Unix Socket IPC       │
                 │        (JSON)             │
                 ▼                           ▼
         ┌─────────────────────────────────────────┐
         │                Daemon                    │
         │  ┌─────────────────────────────────┐    │
         │  │       Tunnel Manager            │    │
         │  │  ┌─────┐ ┌─────┐ ┌─────┐       │    │
         │  │  │ SSH │ │ SSH │ │ SSH │ ...   │    │
         │  │  │ Proc│ │ Proc│ │ Proc│       │    │
         │  │  └─────┘ └─────┘ └─────┘       │    │
         │  └─────────────────────────────────┘    │
         │  ┌─────────────────────────────────┐    │
         │  │    Network Monitor              │    │
         │  │  (SCNetworkReachability/netlink)│    │
         │  └─────────────────────────────────┘    │
         │  ┌─────────────────────────────────┐    │
         │  │    On-Demand Stub Listeners     │    │
         │  └─────────────────────────────────┘    │
         └─────────────────────────────────────────┘
```

## Tunnel Types

| Type | SSH Flag | Direction | Config Fields |
|------|----------|-----------|---------------|
| `local` | `-L` | local:port → remote:host:port | `local_port`, `remote_host`, `remote_port` |
| `reverse` | `-R` | remote:port → local:host:port | `local_port`, `local_host`, `remote_port`, `remote_bind` |
| `socks` | `-D` | local:port → dynamic SOCKS5 | `local_port` |

## Tunnel Modes

| Mode | Behavior |
|------|----------|
| `auto` | Start on daemon launch, auto-reconnect on failure/network change |
| `manual` | Only start/stop via explicit user action |
| `on-demand` | Daemon binds a stub listener; tunnel established on first connection attempt |

## Configuration

### File Location

- macOS: `~/Library/Application Support/Burrow/config.toml`
- Linux: `~/.config/burrow/config.toml`

### Schema

```toml
[defaults]
ssh_binary = "ssh"           # Path to SSH binary
keepalive = true             # Enable SSH keepalive

[tunnel.dev-db]
name = "Dev Database"        # Human-readable name (required)
host = "bastion.example.com" # SSH host (required)
port = 22                    # SSH port (optional, defers to ssh config)
type = "local"               # local | reverse | socks (required)
mode = "auto"                # auto | manual | on-demand (default: auto)

# Local forward specific
local_port = 5432            # Local bind port (required)
remote_host = "db.internal"  # Target host (required for local)
remote_port = 5432           # Target port (required for local)

# Optional overrides
identity = "~/.ssh/work_key" # SSH identity file
jump_host = "gateway.example.com"  # ProxyJump host
jump_port = 22               # ProxyJump port (optional, defers to ssh config)
ssh_binary = "/usr/local/bin/ssh"  # Override SSH binary
keepalive = true             # Override keepalive setting

[tunnel.expose-api]
name = "Expose Local API"
host = "jumphost.example.com"
type = "reverse"
mode = "manual"
local_port = 8080            # Local port to forward
local_host = "127.0.0.1"     # Local bind address (default: 127.0.0.1)
remote_port = 9000           # Remote port to open
remote_bind = "0.0.0.0"      # Remote bind address (default: localhost)

[tunnel.home-proxy]
name = "Home SOCKS"
host = "home.example.com"
type = "socks"
mode = "on-demand"
local_port = 1080
```

### Config Validation Rules

- Tunnel ID: lowercase alphanumeric with hyphens, no spaces (e.g., `dev-db`, `prod-api`)
- `name` must be non-empty
- `name` should be unique (warning, not error)
- `local_port` must be unique across all tunnels (including on-demand)
- `type = "local"` requires `remote_host` and `remote_port`
- `type = "socks"` ignores `remote_host`, `remote_port`
- `identity` path is expanded (`~` → home directory)

## State Persistence

### File Location

- macOS: `~/Library/Application Support/Burrow/state.json`
- Linux: `~/.local/state/burrow/state.json`

### Schema

```json
{
  "tunnels": {
    "dev-db": {
      "enabled": true,
      "status": "connected",
      "last_connected": "2025-02-01T10:30:00Z",
      "last_error": null,
      "stats": {
        "total_connections": 142,
        "current_session_start": "2025-02-01T10:30:00Z",
        "total_uptime_seconds": 36000,
        "reconnect_count": 3
      }
    }
  },
  "daemon_started": "2025-02-01T08:00:00Z"
}

## Daemon

### Lifecycle

1. **Auto-start**: First CLI command or GUI launch starts daemon if not running
2. **Explicit control**: `burrow daemon stop`, `burrow daemon restart`
3. **System integration** (optional): `burrow service install` generates launchd/systemd unit

### Startup Sequence

1. Load and validate config
2. Load persisted state
3. Start IPC listener (Unix socket)
4. Start network monitor
5. For each tunnel with `mode = "auto"` and `enabled = true`:
   - Attempt connection
   - On failure: log, notify, schedule retry
6. For each tunnel with `mode = "on-demand"`:
   - Bind stub listener on `local_port`

### Tunnel Process Management

Each tunnel spawns an SSH subprocess:

```bash
# Local forward
ssh -N -L 127.0.0.1:5432:db.internal:5432 \
    -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
    -o ExitOnForwardFailure=yes \
    -o BatchMode=yes -o ConnectTimeout=15 -o LogLevel=VERBOSE \
    -i ~/.ssh/work_key \
    -J gateway.example.com \
    -p 22 \
    bastion.example.com

# Reverse forward
ssh -N -R 0.0.0.0:9000:127.0.0.1:8080 ...

# SOCKS
ssh -N -D 127.0.0.1:1080 ...
```

### SSH Options (hardcoded)

- `-N` — No remote command
- `-o ExitOnForwardFailure=yes` — Fail fast if port bind fails
- `-o BatchMode=yes` — Never prompt (no terminal); auth must use keys/agent
- `-o ConnectTimeout=15` — Don't hang on unreachable hosts
- `-o LogLevel=VERBOSE` — Emits "Authenticated to ..." on stderr; the tunnel
  moves from `connecting` to `connected` when it appears (or after 20s alive)
- `-o ServerAliveInterval=30` (when keepalive=true)
- `-o ServerAliveCountMax=3` (when keepalive=true)

### Reconnection Logic

```
On tunnel process exit:
  if exit was requested by user:
    set status = disconnected
    return
  
  if mode == manual:
    set status = disconnected
    notify user
    return
  
  set status = error
  notify user
  
  backoff = initial_backoff (2 seconds)
  max_backoff = 5 minutes
  
  loop:
    wait(backoff)
    attempt reconnect
    if success:
      set status = connected
      notify user
      break
    backoff = min(backoff * 2, max_backoff)
```

### Network Change Detection

**macOS**: `SCNetworkReachability` API via `system-configuration` crate

**Linux**: `netlink` socket monitoring via `rtnetlink` crate

**Behavior**:
1. Detect network interface change (WiFi switch, VPN connect/disconnect)
2. Debounce: wait 2 seconds for network to stabilize
3. For each `auto` mode tunnel not currently connected:
   - Reset backoff to initial value
   - Attempt immediate reconnection

### On-Demand Stub Listener

For tunnels with `mode = "on-demand"`:

1. Daemon binds `local_port` with a TCP listener
2. On incoming connection:
   a. Accept and hold the connection
   b. Send macOS/Linux notification: "Establish tunnel [name]?" with actions
   c. If user approves:
      - Close stub listener
      - Spawn SSH process
      - Wait for tunnel ready (port bound)
      - Proxy held connection to real tunnel
   d. If user denies or timeout (30s):
      - Close incoming connection
      - Keep stub listener active

## IPC Protocol

### Socket Location

- macOS: `~/Library/Application Support/Burrow/burrow.sock`
- Linux: `$XDG_RUNTIME_DIR/burrow.sock` or `/tmp/burrow-$UID.sock`

### Message Format

JSON-RPC 2.0 over Unix socket, newline-delimited.

### Commands

```typescript
// Request
{ "jsonrpc": "2.0", "id": 1, "method": "tunnel.list", "params": {} }

// Response
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "tunnels": [
      {
        "id": "dev-db",
        "name": "Dev Database",
        "type": "local",
        "mode": "auto",
        "status": "connected",  // connected | disconnected | connecting | error
        "local_port": 5432,
        "remote": "db.internal:5432",
        "host": "bastion.example.com",
        "enabled": true,
        "last_error": null,
        "stats": { ... }
      }
    ]
  }
}
```

**Methods**:

| Method | Params | Description |
|--------|--------|-------------|
| `tunnel.list` | — | List all tunnels with status |
| `tunnel.get` | `{ "id": "..." }` | Get single tunnel details |
| `tunnel.connect` | `{ "id": "..." }` | Start tunnel |
| `tunnel.disconnect` | `{ "id": "..." }` | Stop tunnel |
| `tunnel.enable` | `{ "id": "..." }` | Enable tunnel (persisted) |
| `tunnel.disable` | `{ "id": "..." }` | Disable tunnel (persisted) |
| `tunnel.connect_all` | — | Connect all enabled auto/manual tunnels |
| `tunnel.disconnect_all` | — | Disconnect all tunnels |
| `tunnel.restart_all` | — | Disconnect + connect all |
| `tunnel.add` | `{ "id": "...", "config": {...} }` | Add new tunnel to config |
| `tunnel.remove` | `{ "id": "..." }` | Remove tunnel from config |
| `tunnel.modify` | `{ "id": "...", "config": {...} }` | Modify tunnel config |
| `config.reload` | — | Reload config from disk |
| `daemon.status` | — | Daemon uptime, version, etc. |
| `daemon.shutdown` | — | Stop daemon |
| `logs.subscribe` | — | Stream log events (server-sent) |
| `logs.recent` | `{ "lines": 100 }` | Get recent log lines |

### Log Event Streaming

For `logs.subscribe`, daemon sends newline-delimited events:

```json
{"jsonrpc":"2.0","method":"log","params":{"timestamp":"...","level":"info","tunnel":"dev-db","message":"Connected"}}
```

## CLI

### Commands

```
burrow
├── status                    # Show all tunnel statuses (default command)
├── list                      # Alias for status
├── connect <tunnel-id>       # Connect specific tunnel
├── disconnect <tunnel-id>    # Disconnect specific tunnel
├── connect-all               # Connect all enabled tunnels
├── disconnect-all            # Disconnect all tunnels
├── restart-all               # Restart all tunnels (after config change)
├── enable <tunnel-id>        # Enable tunnel
├── disable <tunnel-id>       # Disable tunnel
├── logs [--follow] [--tunnel <id>]  # View logs
├── tunnel
│   ├── add <tunnel-id>       # Add new tunnel (ID required, e.g., "dev-db")
│   │   --name "Human Name"   # Required: display name
│   │   --host <ssh-host>
│   │   --port <ssh-port>
│   │   --type <local|reverse|socks>
│   │   --mode <auto|manual|on-demand>
│   │   --local-port <port>
│   │   --remote-host <host>  # for local type
│   │   --remote-port <port>  # for local/reverse type
│   │   --local-host <host>   # for reverse type
│   │   --remote-bind <addr>  # for reverse type
│   │   --identity <path>
│   │   --jump-host <host>
│   │   --jump-port <port>
│   │   --ssh-binary <path>
│   │   --keepalive <true|false>
│   ├── remove <tunnel-id>    # Remove tunnel from config
│   ├── modify <tunnel-id>    # Modify existing tunnel (same flags as add)
│   └── show <tunnel-id>      # Show tunnel config details
├── config
│   ├── path                  # Print config file path
│   ├── edit                  # Open config in $EDITOR
│   ├── validate              # Validate config
│   └── reload                # Reload config in running daemon
├── daemon
│   ├── start                 # Start daemon (usually automatic)
│   ├── stop                  # Stop daemon
│   ├── restart               # Restart daemon
│   └── status                # Daemon info
└── service
    ├── install               # Install launchd/systemd service
    └── uninstall             # Remove service

burrow gui                    # Launch menu bar app
burrow --gui                  # Alternative syntax
```

### Output Formats

Default: Human-readable table

```
$ burrow status
TUNNEL          STATUS       LOCAL              REMOTE                    
Dev Database    ● connected  localhost:5432  →  db.internal:5432          
Expose API      ○ disabled   localhost:8080  ←  0.0.0.0:9000              
Home SOCKS      ◐ connecting localhost:1080  ⇄  SOCKS5                    
```

With `--json` flag: JSON output for scripting

## GUI (Menu Bar + Window)

### Technology

- **Window**: `iced` crate (pure Rust, cross-platform)
- **Tray**: `tray-icon` + `muda` crates for native menu bar integration
- **Notifications**: `notify-rust` crate

### Tray Menu

```
● Dev Database (localhost:5432)
○ Expose Local API (disconnected)
◐ Home SOCKS (connecting...)
⚠ Prod Tunnel (error: connection refused)
─────────────────────────────────
Connect All
Disconnect All
─────────────────────────────────
Open Window
─────────────────────────────────
Quit
```

**Status indicators**:
- `●` connected (green)
- `○` disconnected (gray)
- `◐` connecting (yellow/animated)
- `⚠` error (red)

**Tray icon**: Changes based on aggregate status
- All connected: green
- Some connected: yellow
- None connected: gray
- Any error: red

### Main Window

```
┌─────────────────────────────────────────────────────────────────┐
│  Burrow                                          [─] [□] [×]    │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────────────────────────────────────────────────────┐│
│  │ Tunnels                            [Connect All] [Restart]  ││
│  ├─────────────────────────────────────────────────────────────┤│
│  │ ● Dev Database          localhost:5432 → db.internal:5432   ││
│  │   bastion.example.com   uptime: 2h 15m          [Disconnect]││
│  ├─────────────────────────────────────────────────────────────┤│
│  │ ○ Expose Local API      localhost:8080 ← 0.0.0.0:9000       ││
│  │   jumphost.example.com  mode: manual             [Connect]  ││
│  ├─────────────────────────────────────────────────────────────┤│
│  │ ⚠ Prod Tunnel           localhost:3306 → prod-db:3306       ││
│  │   prod.example.com      Error: Connection refused   [Retry] ││
│  └─────────────────────────────────────────────────────────────┘│
│                                                                  │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │ Logs                                    [Clear] [Export]    ││
│  ├─────────────────────────────────────────────────────────────┤│
│  │ 10:30:01 [dev-db] Connected to bastion.example.com          ││
│  │ 10:30:01 [dev-db] Local forward established on :5432        ││
│  │ 10:29:45 [prod-tunnel] Connection refused                   ││
│  │ 10:29:45 [prod-tunnel] Retry in 4 seconds...                ││
│  │ 10:29:41 [prod-tunnel] Attempting connection...             ││
│  │ ...                                                         ││
│  └─────────────────────────────────────────────────────────────┘│
├─────────────────────────────────────────────────────────────────┤
│  Daemon: running (uptime: 2h 30m)    Config: valid   [Reload]  │
└─────────────────────────────────────────────────────────────────┘
```

### Notifications

| Event | Notification |
|-------|--------------|
| Tunnel connected | "✓ [name] connected" |
| Tunnel disconnected (user action) | None |
| Tunnel disconnected (unexpected) | "⚠ [name] disconnected — retrying" |
| Tunnel error (after retries exhausted) | "✗ [name] failed: [error]" |
| Port conflict | "✗ [name] — port [port] already in use" |
| On-demand connection request | "Connect [name]?" with Accept/Deny actions |
| Config reload | "Configuration reloaded" |
| Network change detected | "Network changed — reconnecting tunnels" |

## Logging

### First-Run Behavior

On first run (no config file exists):

1. Create config directory if needed
2. Write sample config file with commented examples:

```toml
# Burrow SSH Tunnel Manager Configuration
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
# type = "local"
# mode = "auto"              # auto | manual | on-demand
# local_port = 5432
# remote_host = "db.internal"
# remote_port = 5432
# identity = "~/.ssh/id_rsa"
# jump_host = "gateway.example.com"

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
```

3. Print message:
   ```
   Created sample configuration at: ~/.config/burrow/config.toml
   
   Edit the config file to add your tunnels, then run:
     burrow config reload
   
   Or add a tunnel with:
     burrow tunnel add my-tunnel --name "My Tunnel" --host example.com --type local \
       --local-port 5432 --remote-host db.internal --remote-port 5432
   ```

4. Exit with code 0 (not an error)

### Location

- macOS: `~/Library/Logs/Burrow/burrow.log`
- Linux: `~/.local/state/burrow/burrow.log`

### Format

```
2025-02-01T10:30:01.123Z INFO  [daemon] Started, version 0.1.0
2025-02-01T10:30:01.456Z INFO  [dev-db] Connecting to bastion.example.com:22
2025-02-01T10:30:02.789Z INFO  [dev-db] SSH process started, pid=12345
2025-02-01T10:30:03.012Z INFO  [dev-db] Local forward established on 127.0.0.1:5432
2025-02-01T10:30:03.012Z INFO  [dev-db] Status: connected
```

### Rotation

- Max file size: 10 MB
- Keep: 5 rotated files
- Rotation: `burrow.log` → `burrow.log.1` → ... → `burrow.log.5`

### Log Levels

Configurable via environment variable `BURROW_LOG` or config:

```toml
[defaults]
log_level = "info"  # trace | debug | info | warn | error
```

## Error Handling

### Port Conflicts

1. On tunnel start, attempt to bind port
2. If `EADDRINUSE`:
   - Set tunnel status to `error`
   - Notify user: "Port [port] already in use"
   - Do not retry automatically
   - User must resolve conflict and manually retry

### SSH Process Failures

| Exit Code | Meaning | Action |
|-----------|---------|--------|
| 0 | Normal exit | Unexpected for `-N`; treat as error, retry |
| 255 | SSH error | Parse stderr, retry with backoff |
| Signal | Killed | If by daemon: expected. Otherwise: retry |

### Config Errors

- On daemon start: refuse to start, print error, exit 1
- On reload: keep running with old config, notify error

## Security Considerations

- No secrets stored in config (rely on ssh-agent)
- Unix socket permissions: 0600 (owner only)
- Config file permissions: warn if world-readable
- Identity file paths: validated and expanded, not executed

## Crate Dependencies (Preliminary)

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
clap = { version = "4", features = ["derive"] }
directories = "5"              # XDG paths
notify-rust = "4"              # Desktop notifications
iced = "0.12"                  # GUI
tray-icon = "0.14"             # System tray
muda = "0.11"                  # Native menus

[target.'cfg(target_os = "macos")'.dependencies]
system-configuration = "0.5"   # Network monitoring

[target.'cfg(target_os = "linux")'.dependencies]
rtnetlink = "0.13"             # Network monitoring
```

## File Structure

```
burrow/
├── Cargo.toml
├── src/
│   ├── main.rs                # Entry point, CLI dispatch
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── commands.rs        # CLI command implementations
│   │   └── output.rs          # Table/JSON formatting
│   ├── daemon/
│   │   ├── mod.rs
│   │   ├── server.rs          # IPC server, main loop
│   │   ├── tunnel.rs          # Tunnel struct, SSH process management
│   │   ├── manager.rs         # Tunnel lifecycle, reconnection logic
│   │   ├── network.rs         # Network change detection
│   │   ├── stub.rs            # On-demand stub listeners
│   │   └── state.rs           # State persistence
│   ├── gui/
│   │   ├── mod.rs
│   │   ├── app.rs             # Iced application
│   │   ├── tray.rs            # Tray icon and menu
│   │   ├── views/
│   │   │   ├── tunnel_list.rs
│   │   │   └── logs.rs
│   │   └── style.rs           # Theming
│   ├── config/
│   │   ├── mod.rs
│   │   ├── schema.rs          # Config structs
│   │   └── validation.rs
│   ├── ipc/
│   │   ├── mod.rs
│   │   ├── protocol.rs        # JSON-RPC types
│   │   ├── client.rs          # For CLI/GUI
│   │   └── server.rs          # For daemon
│   └── common/
│       ├── mod.rs
│       ├── paths.rs           # Platform-specific paths
│       ├── error.rs           # Error types
│       └── logging.rs         # Log setup
├── assets/
│   ├── icon.png               # App icon
│   ├── tray-connected.png
│   ├── tray-disconnected.png
│   └── tray-error.png
└── resources/
    ├── com.burrow.daemon.plist    # launchd template
    └── burrow.service             # systemd template
```

---

## Design Decisions

| Topic | Decision |
|-------|----------|
| Byte counters | Skip in v1; track uptime/connection count only |
| Jump host port | Separate `jump_port` field |
| Multiple forwards | One tunnel = one forward |
| Config editing | Structured CLI commands (`burrow tunnel add/remove/modify`) |
| GUI config editing | Read-only view; edit externally |
| First-run | Create sample config with commented examples, prompt user to edit |
| Auto-update | Out of scope for v1 |
| Tunnel ID | User must explicitly provide (not auto-generated) |
| Config writes | Atomic: write to temp file, then rename |
| Daemon auto-start | Only on commands requiring tunnels (`connect`, `status`, `connect-all`, etc.) |
| GUI binary | Same binary as CLI (`burrow gui`), can split later |

---

## Implementation Details

### Config File Atomicity

When modifying config via CLI (`tunnel add/remove/modify`):

1. Read existing config
2. Apply modification in memory
3. Validate new config
4. Write to temp file: `config.toml.tmp`
5. Atomic rename: `config.toml.tmp` → `config.toml`
6. Signal daemon to reload (if running)

### Daemon Auto-Start

Commands that auto-start daemon if not running:
- `burrow status` / `burrow list`
- `burrow connect <id>`
- `burrow connect-all`
- `burrow restart-all`
- `burrow logs`
- `burrow gui`

Commands that do NOT auto-start daemon:
- `burrow daemon stop`
- `burrow daemon status` (reports "not running")
- `burrow config *` (operates on file only)
- `burrow tunnel add/remove/modify` (modifies file, optionally signals reload)
- `burrow service *`

### GUI Launch

```bash
burrow gui          # Launch menu bar app (auto-starts daemon)
burrow --gui        # Alternative syntax
```

GUI process:
1. Auto-starts daemon if not running
2. Connects to daemon via IPC
3. Shows both a menu bar icon and a Dock icon on macOS
4. Window hidden by default, shown via tray menu or by clicking the Dock icon

---

## Future Enhancements (Out of Scope for v1)

- Native SSH library (for byte counters, better control)
- Multiple forwards per SSH connection
- GUI config editor
- Auto-update mechanism
- WireGuard tunnels
- Plain TCP tunnels
- SSTP tunnels
