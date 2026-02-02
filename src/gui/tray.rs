use std::io::Write;
use std::sync::mpsc as sync_mpsc;
use std::time::Duration;

use serde_json::json;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::ipc::protocol::{TunnelInfo, TunnelStatus};

use super::ipc_client::{DaemonEvent, GuiIpcClient};

const ICON_SIZE: u32 = 22;
const TUNNEL_ID_PREFIX: &str = "tunnel:";
const POLL_INTERVAL: Duration = Duration::from_secs(3);

// -- Aggregate tunnel status for tray icon color --

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AggregateStatus {
    AllConnected,  // green
    SomeConnected, // yellow (includes connecting)
    AnyError,      // red
    NoneConnected, // gray (default / daemon offline)
}

fn aggregate_status(tunnels: &[TunnelInfo], daemon_connected: bool) -> AggregateStatus {
    if !daemon_connected || tunnels.is_empty() {
        return AggregateStatus::NoneConnected;
    }

    if tunnels.iter().any(|t| t.status == TunnelStatus::Error) {
        return AggregateStatus::AnyError;
    }

    let enabled: Vec<_> = tunnels.iter().filter(|t| t.enabled).collect();
    if enabled.is_empty() {
        return AggregateStatus::NoneConnected;
    }

    let connected = enabled
        .iter()
        .filter(|t| t.status == TunnelStatus::Connected)
        .count();

    if connected == enabled.len() {
        AggregateStatus::AllConnected
    } else if connected > 0 || enabled.iter().any(|t| t.status == TunnelStatus::Connecting) {
        AggregateStatus::SomeConnected
    } else {
        AggregateStatus::NoneConnected
    }
}

// -- Commands from main thread to IPC background task --

enum TrayCmd {
    ToggleTunnel { id: String, connected: bool },
    ConnectAll,
    DisconnectAll,
}

// -- State pushed from background to main thread --

struct MenuState {
    tunnels: Vec<TunnelInfo>,
    daemon_connected: bool,
}

/// Build the system tray icon and enter the main event loop.
/// Must be called from the main thread (macOS requirement).
pub fn run() {
    let (update_tx, update_rx) = sync_mpsc::channel::<MenuState>();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<TrayCmd>(32);

    // Background thread runs tokio runtime for async IPC
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        rt.block_on(ipc_task(update_tx, cmd_rx));
    });

    let menu = build_menu(&[], false);

    let tray = TrayIconBuilder::new()
        .with_icon(create_template_icon())
        .with_tooltip("Burrow - SSH Tunnel Manager")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true)
        .with_icon_as_template(true) // macOS: adapts to light/dark menu bar
        .build()
        .expect("failed to create tray icon");

    let menu_rx = MenuEvent::receiver();
    let mut tunnels: Vec<TunnelInfo> = Vec::new();
    let mut daemon_connected = false;
    let mut current_status = AggregateStatus::NoneConnected;

    loop {
        // macOS: process pending events so the tray menu works.
        #[cfg(target_os = "macos")]
        pump_macos_events();

        #[cfg(not(target_os = "macos"))]
        std::thread::sleep(Duration::from_millis(50));

        // Drain pending state updates, keep latest
        let mut needs_rebuild = false;
        while let Ok(state) = update_rx.try_recv() {
            tunnels = state.tunnels;
            daemon_connected = state.daemon_connected;
            needs_rebuild = true;
        }
        if needs_rebuild {
            tray.set_menu(Some(Box::new(build_menu(&tunnels, daemon_connected))));

            let new_status = aggregate_status(&tunnels, daemon_connected);
            if new_status != current_status {
                current_status = new_status;
                let (icon, is_template) = icon_for_status(new_status);
                let _ = tray.set_icon(Some(icon));
                tray.set_icon_as_template(is_template);
            }
        }

        if let Ok(event) = menu_rx.try_recv() {
            handle_menu_event(event.id.as_ref(), &tunnels, &cmd_tx);
        }
    }
}

fn handle_menu_event(
    id: &str,
    tunnels: &[TunnelInfo],
    cmd_tx: &tokio::sync::mpsc::Sender<TrayCmd>,
) {
    match id {
        "quit" => shutdown_and_exit(),
        "open-window" => { /* TODO: launch iced window */ }
        "connect-all" => {
            let _ = cmd_tx.try_send(TrayCmd::ConnectAll);
        }
        "disconnect-all" => {
            let _ = cmd_tx.try_send(TrayCmd::DisconnectAll);
        }
        _ => {
            if let Some(tunnel_id) = id.strip_prefix(TUNNEL_ID_PREFIX) {
                let connected = tunnels.iter().any(|t| {
                    t.id == tunnel_id && t.status == TunnelStatus::Connected
                });
                let _ = cmd_tx.try_send(TrayCmd::ToggleTunnel {
                    id: tunnel_id.to_string(),
                    connected,
                });
            }
        }
    }
}

// -- Menu construction --

fn build_menu(tunnels: &[TunnelInfo], daemon_connected: bool) -> Menu {
    let menu = Menu::new();

    if !daemon_connected {
        let _ = menu.append(&MenuItem::with_id(
            "no-daemon",
            "Daemon not running",
            false,
            None,
        ));
    } else if tunnels.is_empty() {
        let _ = menu.append(&MenuItem::with_id(
            "no-tunnels",
            "No tunnels configured",
            false,
            None,
        ));
    } else {
        for tunnel in tunnels {
            let id = format!("{TUNNEL_ID_PREFIX}{}", tunnel.id);
            let label = tunnel_label(tunnel);
            let _ = menu.append(&MenuItem::with_id(id, label, tunnel.enabled, None));
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());

    let has_tunnels = daemon_connected && !tunnels.is_empty();
    let _ = menu.append(&MenuItem::with_id(
        "connect-all",
        "Connect All",
        has_tunnels,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        "disconnect-all",
        "Disconnect All",
        has_tunnels,
        None,
    ));

    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id("open-window", "Open Window", true, None));

    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id("quit", "Quit Burrow", true, None));

    menu
}

fn tunnel_label(t: &TunnelInfo) -> String {
    let prefix = match t.status {
        TunnelStatus::Connected => "\u{25CF}",   // filled circle
        TunnelStatus::Disconnected => "\u{25CB}", // open circle
        TunnelStatus::Connecting => "\u{25D0}",   // half circle
        TunnelStatus::Error => "\u{26A0}",        // warning
    };

    let detail = match t.status {
        TunnelStatus::Connected => format!("localhost:{}", t.local_port),
        TunnelStatus::Disconnected => "disconnected".into(),
        TunnelStatus::Connecting => "connecting...".into(),
        TunnelStatus::Error => {
            let msg = t.last_error.as_deref().unwrap_or("unknown");
            if msg.chars().count() > 25 {
                let short: String = msg.chars().take(25).collect();
                format!("error: {short}...")
            } else {
                format!("error: {msg}")
            }
        }
    };

    format!("{prefix} {} ({detail})", t.name)
}

// -- Background IPC task --

async fn ipc_task(
    update_tx: sync_mpsc::Sender<MenuState>,
    mut cmd_rx: tokio::sync::mpsc::Receiver<TrayCmd>,
) {
    let (client, mut event_rx) = GuiIpcClient::spawn();
    let mut daemon_connected = false;
    let mut poll_timer = tokio::time::interval(POLL_INTERVAL);

    loop {
        tokio::select! {
            _ = poll_timer.tick() => {
                if daemon_connected {
                    send_state(&client, &update_tx, true).await;
                }
            }
            event = event_rx.recv() => match event {
                Some(DaemonEvent::Connected) => {
                    daemon_connected = true;
                    send_state(&client, &update_tx, true).await;
                }
                Some(DaemonEvent::Disconnected(_)) => {
                    daemon_connected = false;
                    let _ = update_tx.send(MenuState {
                        tunnels: Vec::new(),
                        daemon_connected: false,
                    });
                }
                Some(DaemonEvent::LogLine(_)) => {}
                None => break,
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(TrayCmd::ToggleTunnel { id, connected }) => {
                    if connected {
                        let _ = client.tunnel_disconnect(&id).await;
                    } else {
                        let _ = client.tunnel_connect(&id).await;
                    }
                    send_state(&client, &update_tx, daemon_connected).await;
                    poll_timer.reset();
                }
                Some(TrayCmd::ConnectAll) => {
                    let _ = client.request("tunnel.connect_all", json!({})).await;
                    send_state(&client, &update_tx, daemon_connected).await;
                    poll_timer.reset();
                }
                Some(TrayCmd::DisconnectAll) => {
                    let _ = client.request("tunnel.disconnect_all", json!({})).await;
                    send_state(&client, &update_tx, daemon_connected).await;
                    poll_timer.reset();
                }
                None => break,
            },
        }
    }
}

async fn send_state(
    client: &GuiIpcClient,
    tx: &sync_mpsc::Sender<MenuState>,
    daemon_connected: bool,
) {
    let tunnels = client.tunnel_list().await.unwrap_or_default();
    let _ = tx.send(MenuState {
        tunnels,
        daemon_connected,
    });
}

// -- Icon generation --

/// Map aggregate status to a colored icon.
/// Returns (icon, is_template). Template icons let macOS auto-adapt to
/// light/dark menu bar; colored icons are non-template.
fn icon_for_status(status: AggregateStatus) -> (Icon, bool) {
    match status {
        // Apple system colors for native look
        AggregateStatus::AllConnected => (create_colored_icon(76, 217, 100), false),
        AggregateStatus::SomeConnected => (create_colored_icon(255, 204, 0), false),
        AggregateStatus::AnyError => (create_colored_icon(255, 59, 48), false),
        AggregateStatus::NoneConnected => (create_template_icon(), true),
    }
}

/// Black-on-transparent circle. macOS renders template icons adapting to
/// the menu bar appearance (dark on light, light on dark).
fn create_template_icon() -> Icon {
    create_circle_icon(0, 0, 0)
}

fn create_colored_icon(r: u8, g: u8, b: u8) -> Icon {
    create_circle_icon(r, g, b)
}

fn create_circle_icon(r: u8, g: u8, b: u8) -> Icon {
    let mut rgba = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
    let center = ICON_SIZE as f32 / 2.0;
    let radius = 5.0f32;

    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x as f32 - center + 0.5;
            let dy = y as f32 - center + 0.5;
            if dx * dx + dy * dy <= radius * radius {
                let i = ((y * ICON_SIZE + x) * 4) as usize;
                rgba[i] = r;
                rgba[i + 1] = g;
                rgba[i + 2] = b;
                rgba[i + 3] = 255;
            }
        }
    }

    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).expect("failed to create icon")
}

// -- Platform helpers --

/// Run the CoreFoundation run loop briefly so macOS delivers tray/menu events.
#[cfg(target_os = "macos")]
fn pump_macos_events() {
    use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
    CFRunLoop::run_in_mode(
        unsafe { kCFRunLoopDefaultMode },
        Duration::from_millis(50),
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tunnel(id: &str, status: TunnelStatus, enabled: bool) -> TunnelInfo {
        TunnelInfo {
            id: id.into(),
            name: id.into(),
            tunnel_type: "local".into(),
            mode: "auto".into(),
            status,
            local_port: 5432,
            remote: None,
            host: "host".into(),
            enabled,
            last_error: None,
            stats: None,
        }
    }

    #[test]
    fn status_daemon_offline() {
        let tunnels = vec![make_tunnel("a", TunnelStatus::Connected, true)];
        assert_eq!(aggregate_status(&tunnels, false), AggregateStatus::NoneConnected);
    }

    #[test]
    fn status_no_tunnels() {
        assert_eq!(aggregate_status(&[], true), AggregateStatus::NoneConnected);
    }

    #[test]
    fn status_all_disabled() {
        let tunnels = vec![
            make_tunnel("a", TunnelStatus::Disconnected, false),
            make_tunnel("b", TunnelStatus::Disconnected, false),
        ];
        assert_eq!(aggregate_status(&tunnels, true), AggregateStatus::NoneConnected);
    }

    #[test]
    fn status_all_connected() {
        let tunnels = vec![
            make_tunnel("a", TunnelStatus::Connected, true),
            make_tunnel("b", TunnelStatus::Connected, true),
            make_tunnel("c", TunnelStatus::Disconnected, false), // disabled, ignored
        ];
        assert_eq!(aggregate_status(&tunnels, true), AggregateStatus::AllConnected);
    }

    #[test]
    fn status_some_connected() {
        let tunnels = vec![
            make_tunnel("a", TunnelStatus::Connected, true),
            make_tunnel("b", TunnelStatus::Disconnected, true),
        ];
        assert_eq!(aggregate_status(&tunnels, true), AggregateStatus::SomeConnected);
    }

    #[test]
    fn status_connecting_counts_as_some() {
        let tunnels = vec![
            make_tunnel("a", TunnelStatus::Connecting, true),
            make_tunnel("b", TunnelStatus::Disconnected, true),
        ];
        assert_eq!(aggregate_status(&tunnels, true), AggregateStatus::SomeConnected);
    }

    #[test]
    fn status_error_takes_priority() {
        let tunnels = vec![
            make_tunnel("a", TunnelStatus::Connected, true),
            make_tunnel("b", TunnelStatus::Error, true),
        ];
        assert_eq!(aggregate_status(&tunnels, true), AggregateStatus::AnyError);
    }

    #[test]
    fn status_none_connected_all_disconnected() {
        let tunnels = vec![
            make_tunnel("a", TunnelStatus::Disconnected, true),
            make_tunnel("b", TunnelStatus::Disconnected, true),
        ];
        assert_eq!(aggregate_status(&tunnels, true), AggregateStatus::NoneConnected);
    }
}

/// Best-effort daemon shutdown, then exit.
fn shutdown_and_exit() -> ! {
    let socket = crate::daemon::socket_path();
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket) {
        let req =
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"daemon.shutdown\",\"params\":{}}\n";
        let _ = stream.write_all(req.as_bytes());
    }
    std::process::exit(0);
}
