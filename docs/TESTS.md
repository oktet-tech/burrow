# Burrow -- End-to-End Test Plan

Manual integration tests for verifying Phase 1-3 functionality.
Run against a real SSH host (referred to as `main` below).

Prerequisites:
- SSH access to `main` with key-based auth (no password prompts)
- No other burrow daemon or service installed
- Ports 19222, 19333, 19444 available

Binary shorthand used throughout:

```bash
B=target/debug/burrow
cargo build
```

---

## 1. First-Run Experience

### 1.1 Sample config creation

```bash
# Back up existing config
cp "$CONFIG" "$CONFIG.bak"
rm "$CONFIG"

$B status
```

Expected:
- Prints "Created sample configuration at: ..."
- Prints instructions for `burrow config reload` and `burrow tunnel add`
- Config file exists and passes `$B config validate`

Restore: `cp "$CONFIG.bak" "$CONFIG"`

### 1.2 Config edit

```bash
EDITOR=true $B config edit
echo $?   # 0
```

Expected: exits 0, editor receives config path as argument.

---

## 2. Config Validation

### 2.1 Valid config

```bash
$B config validate
```

Expected: `Config is valid. (N tunnel(s))`, exit 0.

### 2.2 TOML parse error

Create a config with invalid syntax (e.g. `[[[bad`).

Expected: `Parse error:` with line/column from the TOML parser, exit 1.

### 2.3 Missing required fields

Create a local tunnel missing `remote_host` and `remote_port`.

Expected:
- Error lines with `line N:` prefix pointing to the `[tunnel.id]` header
- Hints like `add: remote_host = "hostname"`
- Exit 1

### 2.4 Port conflict between tunnels

Two tunnels with the same `local_port`.

Expected: error naming both tunnel IDs, exit 1.

### 2.5 SSH binary not found

Set `ssh_binary = "/nonexistent/ssh"` in a tunnel.

Expected: warning about SSH binary not found.

### 2.6 Identity file warning

Set `identity = "/nonexistent/key"` in a tunnel.

Expected: warning about identity file not found, still exits 0 (warnings only).

---

## 3. Daemon Lifecycle

### 3.1 Start

```bash
$B daemon start
```

Expected: `daemon started (pid ...)`.

### 3.2 Double start

```bash
$B daemon start
```

Expected: `daemon is already running`, exit 1.

### 3.3 Status

```bash
$B daemon status
```

Expected: `Daemon: running` with version, uptime, tunnel count.

### 3.4 Stop

```bash
$B daemon stop
```

Expected: `daemon stopped`. Socket file removed.

### 3.5 Status when stopped

```bash
$B daemon status
```

Expected: `Daemon: not running`.

### 3.6 Restart

```bash
$B daemon start
$B daemon restart
```

Expected: new PID, daemon running.

### 3.7 Fail-fast on invalid config

Create an invalid config (e.g. local tunnel missing remote_host), then:

```bash
$B daemon-foreground
echo $?   # 1
```

Expected: daemon exits immediately with error logged.

---

## 4. Tunnel CRUD

### 4.1 Add tunnel

```bash
$B tunnel add test-db --name "Test DB" --host main --type local \
  --local-port 19222 --remote-host localhost --remote-port 22
```

Expected: `tunnel 'test-db' added`. Config file updated.

### 4.2 Show tunnel

```bash
$B tunnel show test-db
```

Expected: all fields printed (name, host, type, ports, mode, etc).

### 4.3 Modify tunnel

```bash
$B tunnel modify test-db --local-port 19223
$B tunnel show test-db
```

Expected: local_port changed to 19223.

### 4.4 Remove tunnel

```bash
$B tunnel remove test-db
$B tunnel show test-db
```

Expected: removal confirmed, show returns error "not found".

### 4.5 Remove while connected

```bash
# Add, start daemon, connect, then try remove
$B tunnel add live --name "Live" --host main --type local \
  --local-port 19222 --remote-host localhost --remote-port 22
$B daemon start
$B connect live
$B tunnel remove live
```

Expected: error "tunnel is connected". Use `--force` to override.

```bash
$B tunnel remove live --force
```

Expected: tunnel disconnected and removed.

### 4.6 Invalid tunnel ID

```bash
$B tunnel add "Bad ID" --name "Bad" --host main --type local --local-port 19222
```

Expected: validation error about ID format.

---

## 5. Tunnel Types

Start daemon with tunnels of each type configured.

### 5.1 Local forward

```bash
$B tunnel add t-local --name "Local" --host main --type local \
  --local-port 19222 --remote-host localhost --remote-port 22
$B daemon start
$B connect t-local
ssh -p 19222 localhost echo OK
```

Expected: SSH through the tunnel succeeds.

### 5.2 Reverse forward

```bash
$B tunnel add t-reverse --name "Reverse" --host main --type reverse \
  --local-port 8080 --remote-port 9000
$B connect t-reverse
# From the remote host: curl localhost:9000
```

Expected: remote port 9000 forwards to local port 8080.

### 5.3 SOCKS proxy

```bash
$B tunnel add t-socks --name "Proxy" --host main --type socks \
  --local-port 1080
$B connect t-socks
curl --socks5 localhost:1080 http://example.com
```

Expected: HTTP request routed through the SOCKS proxy.

---

## 6. Tunnel Modes

### 6.1 Auto mode -- connect on start

```bash
# Tunnel with mode=auto (default)
$B daemon start
$B status
```

Expected: auto-mode tunnels show `connected` after daemon start.

### 6.2 Auto mode -- reconnect after failure

```bash
# Kill the SSH process
kill $(pgrep -f "ssh.*19222")
sleep 5
$B status
```

Expected: tunnel reconnects automatically (status returns to `connected`).

### 6.3 Manual mode -- no auto-connect

```bash
$B tunnel add manual-t --name "Manual" --host main --type local \
  --local-port 19444 --remote-host localhost --remote-port 22 --mode manual
$B config reload
$B status
```

Expected: manual tunnel stays `disconnected` after daemon start and after reload.

### 6.4 On-demand mode -- stub listener

```bash
$B tunnel add od-t --name "OnDemand" --host main --type local \
  --local-port 19333 --remote-host localhost --remote-port 22 --mode on-demand
$B daemon restart
lsof -iTCP:19333 -sTCP:LISTEN -P
```

Expected: burrow process listening on port 19333 (stub).

### 6.5 On-demand mode -- trigger connect

```bash
nc -w 3 localhost 19333 < /dev/null
$B status
```

Expected: tunnel transitions to `connected`. The nc client receives
the SSH banner through the proxy.

### 6.6 On-demand mode -- stub re-binds after disconnect

```bash
$B disconnect od-t
sleep 2
lsof -iTCP:19333 -sTCP:LISTEN -P
```

Expected: stub listener re-bound on port 19333 by the burrow process.

---

## 7. Enable / Disable

### 7.1 Disable stops tunnel and prevents auto-connect

```bash
$B disable t-local
$B status   # disconnected
$B connect t-local   # should error or remain disabled
```

Expected: tunnel disconnected, stays disabled.

### 7.2 Enable re-activates

```bash
$B enable t-local
$B status
```

Expected: auto-mode tunnel reconnects, on-demand tunnel starts stub.

---

## 8. Port Conflict Detection

### 8.1 Pre-spawn conflict

```bash
# Occupy the port
python3 -c "import socket,time; s=socket.socket(); s.bind(('127.0.0.1',19444)); s.listen(1); time.sleep(30)" &
PID=$!

$B connect conflict-test
$B status
```

Expected:
- Connect fails: "Address already in use"
- Status shows `error` with "port 19444 already in use"

```bash
kill $PID
```

### 8.2 No reconnect on port conflict

```bash
# After 8.1, wait 10s
sleep 10
$B status
```

Expected: tunnel still in `error` state, no reconnect attempts in logs.

### 8.3 Port conflict cleared on manual retry

```bash
# After freeing the port
$B connect conflict-test
$B status
```

Expected: tunnel connects successfully, port_conflict flag cleared.

---

## 9. Reconnection & Backoff

### 9.1 Exponential backoff

```bash
$B logs --tunnel test-local | grep "scheduling reconnect"
```

Expected: delays grow: 2s, 4s, 8s, 16s, ... up to 300s max.

### 9.2 Backoff resets on success

After a successful reconnect, kill SSH again and check the next
reconnect delay starts back at 2s.

### 9.3 Disconnect cancels pending reconnect

```bash
# While reconnect is pending:
$B disconnect test-local
$B logs | grep "test-local" | tail -5
```

Expected: no reconnect attempt fires after disconnect.

---

## 10. Config Reload

### 10.1 Add tunnel via reload

Edit config to add a new tunnel section, then:

```bash
$B config reload
$B status
```

Expected: `config reloaded: 1 added`. New tunnel visible in status.

### 10.2 Remove tunnel via reload

Remove a tunnel section from config:

```bash
$B config reload
$B status
```

Expected: `config reloaded: 1 removed`. Tunnel gone from status.

### 10.3 Update tunnel via reload

Change `local_port` of an existing tunnel:

```bash
$B config reload
$B status
```

Expected: `config reloaded: 1 updated`. Tunnel reconnects on new port.

### 10.4 Reload with invalid config

Introduce a validation error, then:

```bash
$B config reload
```

Expected: error reported, existing tunnels unaffected.

---

## 11. State Persistence

### 11.1 Enable flag survives restart

```bash
$B disable t-local
$B daemon restart
$B status
```

Expected: tunnel remains disabled after restart.

### 11.2 Stats survive restart

```bash
$B tunnel show t-local   # note total_connections
$B daemon restart
$B tunnel show t-local   # same total_connections
```

### 11.3 Corrupt state file

Replace state.json with garbage, restart daemon.

Expected: daemon starts normally, logs warning, resets to defaults.

---

## 12. Network Change Detection (macOS)

### 12.1 Recovery after network drop

```bash
# With a tunnel in error state (e.g. after toggling WiFi off/on)
$B status   # should show error
# Toggle network interface
$B status   # should show connected after ~2-3s
```

Expected: errored auto-mode tunnels reconnect automatically.

### 12.2 Debounce

Toggle network rapidly multiple times. Check logs:

```bash
$B logs | grep "network\|reconnect_errored"
```

Expected: only one reconnect_errored call per burst (2s debounce).

---

## 13. Service Install / Uninstall (macOS)

### 13.1 Install

```bash
$B daemon stop
$B service install
```

Expected:
- Plist at `~/Library/LaunchAgents/com.burrow.daemon.plist`
- Plist contains correct binary path and `daemon-foreground` argument
- `RunAtLoad` is true, `KeepAlive/SuccessfulExit` is false
- Daemon starts automatically (verify with `$B daemon status`)

### 13.2 Double install

```bash
$B service install
```

Expected: error "Service already installed", exit 1.

### 13.3 Uninstall

```bash
$B service uninstall
```

Expected:
- Plist removed
- Daemon stopped (verify with `$B daemon status`)

### 13.4 Uninstall when not installed

```bash
$B service uninstall
```

Expected: error "Service not installed", exit 1.

---

## 14. Logging

### 14.1 Log tail

```bash
$B logs
```

Expected: last 100 lines of daemon log.

### 14.2 Log follow

```bash
timeout 5 $B logs --follow &
$B disconnect test-local
$B connect test-local
wait
```

Expected: new log lines appear as tunnel activity occurs.

### 14.3 Tunnel filter

```bash
$B logs --tunnel test-local
```

Expected: only lines containing `test-local`.

### 14.4 Log rotation

Grow the log file past 5 MB, then trigger rotation (daemon restart
or explicit rotate). Verify `burrow.log.1` through `burrow.log.5` exist,
oldest dropped.

---

## 15. Bulk Operations

### 15.1 Connect all

```bash
$B disconnect-all
$B connect-all
$B status
```

Expected: all enabled tunnels connected, count printed.

### 15.2 Disconnect all

```bash
$B disconnect-all
$B status
```

Expected: all tunnels disconnected, on-demand stubs re-bind.

### 15.3 Restart all

```bash
$B restart-all
$B status
```

Expected: all enabled tunnels reconnected, count printed.

---

## 16. Edge Cases

### 16.1 Daemon not running

```bash
$B daemon stop
$B status
$B connect test-local
$B config reload
```

Expected: all commands print `daemon is not running`, exit 1.

### 16.2 Unknown tunnel ID

```bash
$B connect nonexistent
$B disconnect nonexistent
```

Expected: `tunnel 'nonexistent' not found`.

### 16.3 Empty config

Delete all tunnel sections:

```bash
$B config validate
$B daemon start
$B status
```

Expected: valid config with 0 tunnels, daemon starts, status shows
"No tunnels configured."

### 16.4 SSH binary not found at runtime

Set `ssh_binary = "/nonexistent"` on a tunnel, connect:

```bash
$B connect bad-binary
$B status
```

Expected: error with "failed to spawn SSH" or "No such file".

### 16.5 Stale socket cleanup

```bash
$B daemon stop
# Manually create a stale socket
touch "$HOME/Library/Application Support/Burrow/burrow.sock"
$B daemon start
```

Expected: stale socket detected, removed, daemon starts normally.

---

## Cleanup

After running all tests:

```bash
$B daemon stop
$B service uninstall 2>/dev/null
# Restore original config
cp "$CONFIG.bak" "$CONFIG"
rm -f "$CONFIG.bak"
```
