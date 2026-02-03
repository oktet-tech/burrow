use iced::font::Weight;
use iced::widget::{
    button, column, container, horizontal_rule, horizontal_space, row, text, toggler,
};
use iced::{Center, Color, Element, Font, Length};

use crate::gui::app::Message;
use crate::gui::style;
use crate::ipc::protocol::{TunnelInfo, TunnelStatus};

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};

/// Tunnel list panel: header with bulk actions, then one two-line row per tunnel.
pub fn view(tunnels: &[TunnelInfo]) -> Element<'_, Message> {
    let has_tunnels = !tunnels.is_empty();

    let header = row![
        text("Tunnels").size(18).font(BOLD),
        horizontal_space(),
        button(text("+ New").size(13))
            .on_press(Message::ShowNewTunnelForm)
            .style(button::primary)
            .padding([4, 12]),
        button(text("Connect All").size(13))
            .on_press_maybe(has_tunnels.then_some(Message::ConnectAll))
            .style(button::secondary)
            .padding([4, 12]),
        button(text("Restart All").size(13))
            .on_press_maybe(has_tunnels.then_some(Message::RestartAll))
            .style(button::secondary)
            .padding([4, 12]),
    ]
    .spacing(8)
    .align_y(Center);

    let mut content = column![header, horizontal_rule(1)].spacing(8);

    if tunnels.is_empty() {
        content = content.push(
            text("No tunnels configured")
                .size(14)
                .color(style::MUTED),
        );
    } else {
        for tunnel in tunnels {
            content = content.push(tunnel_row(tunnel));
            content = content.push(horizontal_rule(1));
        }
    }

    container(content)
        .padding(16)
        .width(Length::Fill)
        .into()
}

/// Two-line tunnel row:
///   [dot] Name                    localhost:port -> remote
///         host       uptime/error/mode           [Button] [Edit]
fn tunnel_row(t: &TunnelInfo) -> Element<'_, Message> {
    let enabled = t.enabled;
    let (dot, dot_color) = status_indicator(t);

    let status_dot = container(text(dot).size(14).color(dot_color)).width(20.0);

    let port_color = if enabled { style::MUTED } else { style::DISABLED };

    // Line 1: name + port mapping
    let name_text = if enabled {
        text(&t.name).size(15).font(BOLD)
    } else {
        text(&t.name).size(15).font(BOLD).color(style::DISABLED)
    };
    let line1 = row![
        name_text,
        horizontal_space(),
        text(port_mapping(t)).size(14).color(port_color),
    ]
    .spacing(8)
    .align_y(Center);

    // Line 2: host + detail + action button + edit button
    let line2 = row![
        text(&t.host).size(13).color(port_color),
        horizontal_space(),
        text(status_detail(t))
            .size(13)
            .color(detail_color(t)),
        action_button(t),
        button(text("Edit").size(13))
            .on_press(Message::EditTunnel(t.id.clone()))
            .style(button::secondary)
            .padding([4, 8]),
    ]
    .spacing(8)
    .align_y(Center);

    let right = column![line1, line2].spacing(2).width(Length::Fill);

    let id = t.id.clone();
    let toggle = toggler(enabled)
        .on_toggle(move |val| Message::ToggleEnabled(id.clone(), val))
        .size(18.0);

    row![toggle, status_dot, right]
        .spacing(8)
        .width(Length::Fill)
        .padding([4, 0])
        .into()
}

// -- Helpers --

fn status_indicator(t: &TunnelInfo) -> (&'static str, Color) {
    if !t.enabled {
        return ("\u{2014}", style::DISABLED); // em dash
    }
    match t.status {
        TunnelStatus::Connected => ("\u{25CF}", style::CONNECTED),   // filled circle
        TunnelStatus::Disconnected => ("\u{25CB}", style::DISCONNECTED), // open circle
        TunnelStatus::Connecting => ("\u{25D0}", style::CONNECTING), // half circle
        TunnelStatus::Error => ("\u{26A0}", style::ERROR),           // warning
    }
}

fn direction_arrow(tunnel_type: &str) -> &'static str {
    match tunnel_type {
        "reverse" => "\u{2190}", // <-
        "socks" => "\u{21C4}",  // bidi
        _ => "\u{2192}",        // -> (local)
    }
}

fn port_mapping(t: &TunnelInfo) -> String {
    let arrow = direction_arrow(&t.tunnel_type);
    let remote = t.remote.as_deref().unwrap_or("-");
    format!("localhost:{} {} {}", t.local_port, arrow, remote)
}

fn status_detail(t: &TunnelInfo) -> String {
    if !t.enabled {
        return "disabled".into();
    }
    match t.status {
        TunnelStatus::Connected => match t.stats {
            Some(ref s) => format!("uptime: {}", format_uptime(s.total_uptime_seconds)),
            None => "connected".into(),
        },
        TunnelStatus::Disconnected => format!("mode: {}", t.mode),
        TunnelStatus::Connecting => "connecting...".into(),
        TunnelStatus::Error => {
            let msg = t.last_error.as_deref().unwrap_or("unknown error");
            if msg.chars().count() > 40 {
                let short: String = msg.chars().take(40).collect();
                format!("Error: {short}...")
            } else {
                format!("Error: {msg}")
            }
        }
    }
}

fn detail_color(t: &TunnelInfo) -> Color {
    if !t.enabled {
        return style::DISABLED;
    }
    match t.status {
        TunnelStatus::Error => style::ERROR,
        _ => style::MUTED,
    }
}

fn action_button(t: &TunnelInfo) -> Element<'_, Message> {
    if !t.enabled {
        return horizontal_space().width(0).into();
    }
    match t.status {
        TunnelStatus::Connected => button(text("Disconnect").size(13))
            .on_press(Message::Disconnect(t.id.clone()))
            .style(button::danger)
            .padding([4, 12])
            .into(),
        TunnelStatus::Disconnected => button(text("Connect").size(13))
            .on_press(Message::Connect(t.id.clone()))
            .style(button::primary)
            .padding([4, 12])
            .into(),
        TunnelStatus::Connecting => button(text("Connecting...").size(13))
            .style(button::secondary)
            .padding([4, 12])
            .into(),
        TunnelStatus::Error => button(text("Retry").size(13))
            .on_press(Message::Connect(t.id.clone()))
            .style(button::primary)
            .padding([4, 12])
            .into(),
    }
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_seconds() {
        assert_eq!(format_uptime(45), "45s");
    }

    #[test]
    fn uptime_minutes() {
        assert_eq!(format_uptime(125), "2m");
    }

    #[test]
    fn uptime_hours() {
        assert_eq!(format_uptime(8100), "2h 15m");
    }

    #[test]
    fn uptime_days() {
        assert_eq!(format_uptime(90000), "1d 1h");
    }

    #[test]
    fn arrow_local() {
        assert_eq!(direction_arrow("local"), "\u{2192}");
    }

    #[test]
    fn arrow_reverse() {
        assert_eq!(direction_arrow("reverse"), "\u{2190}");
    }

    #[test]
    fn arrow_socks() {
        assert_eq!(direction_arrow("socks"), "\u{21C4}");
    }

    #[test]
    fn port_mapping_local() {
        let t = TunnelInfo {
            id: "t".into(),
            name: "T".into(),
            tunnel_type: "local".into(),
            mode: "auto".into(),
            status: TunnelStatus::Connected,
            local_port: 5432,
            remote: Some("db.internal:5432".into()),
            host: "host".into(),
            enabled: true,
            last_error: None,
            stats: None,
        };
        assert_eq!(port_mapping(&t), "localhost:5432 \u{2192} db.internal:5432");
    }

    #[test]
    fn port_mapping_no_remote() {
        let t = TunnelInfo {
            id: "t".into(),
            name: "T".into(),
            tunnel_type: "local".into(),
            mode: "auto".into(),
            status: TunnelStatus::Disconnected,
            local_port: 8080,
            remote: None,
            host: "host".into(),
            enabled: true,
            last_error: None,
            stats: None,
        };
        assert_eq!(port_mapping(&t), "localhost:8080 \u{2192} -");
    }

    #[test]
    fn detail_connected_with_stats() {
        let t = TunnelInfo {
            id: "t".into(),
            name: "T".into(),
            tunnel_type: "local".into(),
            mode: "auto".into(),
            status: TunnelStatus::Connected,
            local_port: 5432,
            remote: None,
            host: "host".into(),
            enabled: true,
            last_error: None,
            stats: Some(crate::ipc::protocol::TunnelStats {
                total_connections: 1,
                current_session_start: None,
                total_uptime_seconds: 8100,
                reconnect_count: 0,
            }),
        };
        assert_eq!(status_detail(&t), "uptime: 2h 15m");
    }

    #[test]
    fn detail_error_truncates() {
        let long_msg = "a]".repeat(30); // 60 chars
        let t = TunnelInfo {
            id: "t".into(),
            name: "T".into(),
            tunnel_type: "local".into(),
            mode: "auto".into(),
            status: TunnelStatus::Error,
            local_port: 5432,
            remote: None,
            host: "host".into(),
            enabled: true,
            last_error: Some(long_msg),
            stats: None,
        };
        let detail = status_detail(&t);
        assert!(detail.starts_with("Error: "));
        assert!(detail.ends_with("..."));
        // "Error: " (7) + 40 chars + "..." (3) = 50 chars
        assert!(detail.chars().count() <= 50);
    }
}
