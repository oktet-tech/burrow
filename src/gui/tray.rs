use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::ipc::protocol::{TunnelInfo, TunnelStatus};

const ICON_SIZE: u32 = 22;
pub const TUNNEL_ID_PREFIX: &str = "tunnel:";

// -- Aggregate tunnel status for tray icon color --

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateStatus {
    AllConnected,  // green
    SomeConnected, // yellow (includes connecting)
    AnyError,      // red
    NoneConnected, // gray (default / daemon offline)
}

pub fn aggregate_status(tunnels: &[TunnelInfo], daemon_connected: bool) -> AggregateStatus {
    if !daemon_connected || tunnels.is_empty() {
        return AggregateStatus::NoneConnected;
    }

    if tunnels.iter().any(|t| t.status == TunnelStatus::Error) {
        return AggregateStatus::AnyError;
    }

    // Standby tunnels are healthy and idle by design; they shouldn't turn
    // the icon yellow.
    let enabled: Vec<_> = tunnels
        .iter()
        .filter(|t| t.enabled && t.status != TunnelStatus::Standby)
        .collect();
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

// -- Tray creation --

/// Build the tray icon with initial disconnected-state menu.
/// Must be called from the main thread (macOS requirement).
pub fn create_tray() -> TrayIcon {
    let menu = build_menu(&[], false);

    TrayIconBuilder::new()
        .with_icon(create_template_icon())
        .with_tooltip("Burrow - SSH Tunnel Manager")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true)
        .with_icon_as_template(true)
        .build()
        .expect("failed to create tray icon")
}

/// Return the global MenuEvent receiver. Wrapper for convenient access.
pub fn menu_event_receiver() -> &'static tray_icon::menu::MenuEventReceiver {
    MenuEvent::receiver()
}

// -- Menu construction --

pub fn build_menu(tunnels: &[TunnelInfo], daemon_connected: bool) -> Menu {
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

    if daemon_connected {
        let _ = menu.append(&MenuItem::with_id("new-tunnel", "New Tunnel...", true, None));
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
        TunnelStatus::Standby => "\u{25CC}",      // dotted circle
    };

    let detail = match t.status {
        TunnelStatus::Connected => format!("localhost:{}", t.local_port),
        TunnelStatus::Disconnected => "disconnected".into(),
        TunnelStatus::Standby => "standby".into(),
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

// -- Icon generation --

/// Map aggregate status to a colored icon.
/// Returns (icon, is_template). Template icons let macOS auto-adapt to
/// light/dark menu bar; colored icons are non-template.
pub fn icon_for_status(status: AggregateStatus) -> (Icon, bool) {
    match status {
        AggregateStatus::AllConnected => (create_colored_icon(76, 217, 100), false),
        AggregateStatus::SomeConnected => (create_colored_icon(255, 204, 0), false),
        AggregateStatus::AnyError => (create_colored_icon(255, 59, 48), false),
        AggregateStatus::NoneConnected => (create_template_icon(), true),
    }
}

pub fn create_template_icon() -> Icon {
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
            next_retry_at: None,
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
    fn standby_does_not_downgrade_status() {
        let tunnels = vec![
            make_tunnel("a", TunnelStatus::Connected, true),
            make_tunnel("b", TunnelStatus::Standby, true),
        ];
        assert_eq!(aggregate_status(&tunnels, true), AggregateStatus::AllConnected);
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
