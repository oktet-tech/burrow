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

    let icon = create_icon();
    let menu = build_menu(&[], false);

    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Burrow - SSH Tunnel Manager")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true)
        .with_icon_as_template(true) // macOS: adapts to light/dark menu bar
        .build()
        .expect("failed to create tray icon");

    let menu_rx = MenuEvent::receiver();
    let mut tunnels: Vec<TunnelInfo> = Vec::new();
    let mut daemon_connected = false;

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

// -- Icon / platform helpers --

/// Generate a small filled circle as the tray icon.
/// Black-on-transparent so macOS template rendering adapts to dark/light mode.
fn create_icon() -> Icon {
    let mut rgba = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
    let center = ICON_SIZE as f32 / 2.0;
    let radius = 5.0f32;

    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x as f32 - center + 0.5;
            let dy = y as f32 - center + 0.5;
            if dx * dx + dy * dy <= radius * radius {
                let i = ((y * ICON_SIZE + x) * 4) as usize;
                // Black pixel, fully opaque (template icon)
                rgba[i + 3] = 255;
            }
        }
    }

    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).expect("failed to create icon")
}

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
