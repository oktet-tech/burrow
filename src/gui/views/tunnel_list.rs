use std::time::{SystemTime, UNIX_EPOCH};

use iced::font::Weight;
use iced::widget::{button, column, container, row, rule, scrollable, space, text};
use iced::{Border, Center, Color, Element, Font, Length, padding};

use super::error_hint;
use crate::gui::app::Message;
use crate::gui::style;
use crate::ipc::protocol::{TunnelInfo, TunnelStatus};

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};

const MONO: Font = Font {
    family: iced::font::Family::Monospace,
    ..Font::DEFAULT
};

/// Where a tunnel is listed. Failing tunnels come first so the one thing
/// that needs the user is at the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Attention,
    Active,
    Standby,
    Idle,
    Disabled,
}

fn section_of(t: &TunnelInfo) -> Section {
    if !t.enabled {
        return Section::Disabled;
    }
    match t.status {
        TunnelStatus::Error => Section::Attention,
        TunnelStatus::Connected | TunnelStatus::Connecting => Section::Active,
        TunnelStatus::Standby => Section::Standby,
        TunnelStatus::Disconnected => Section::Idle,
    }
}

/// Tunnel list grouped by state, with toolbar.
pub fn view(tunnels: &[TunnelInfo], show_disabled: bool) -> Element<'_, Message> {
    let now = unix_now();

    let header = row![
        text("Tunnels").size(20).font(BOLD),
        text(env!("BURROW_REVISION")).size(11).color(style::MUTED),
        space::horizontal(),
        small_button("Reload", Message::ReloadConfig, style::secondary_button),
        small_button("Restart all", Message::RestartAll, style::secondary_button),
        small_button("New Tunnel", Message::ShowNewTunnelForm, style::primary_button),
    ]
    .spacing(8)
    .align_y(Center);

    let in_section = |s: Section| -> Vec<&TunnelInfo> {
        tunnels.iter().filter(|t| section_of(t) == s).collect()
    };

    let mut body = column![].spacing(16);

    if tunnels.is_empty() {
        body = body.push(
            text("No tunnels yet. Create one with New Tunnel.")
                .size(14)
                .color(style::MUTED),
        );
    }

    let attention = in_section(Section::Attention);
    if !attention.is_empty() {
        let cards = attention.iter().map(|t| attention_card(t, now));
        body = body.push(
            column![section_label(
                "NEEDS ATTENTION",
                attention.len(),
                style::ERROR_TEXT
            )]
            .extend(cards)
            .spacing(6),
        );
    }

    for (section, label, color) in [
        (Section::Active, "CONNECTED", style::CONNECTED_TEXT),
        (
            Section::Standby,
            "STANDBY · STARTS ON FIRST CONNECTION",
            style::MUTED,
        ),
        (Section::Idle, "STOPPED", style::MUTED),
    ] {
        let items = in_section(section);
        if !items.is_empty() {
            body = body.push(
                column![section_label(label, items.len(), color), card(&items, now)].spacing(6),
            );
        }
    }

    let disabled = in_section(Section::Disabled);
    if !disabled.is_empty() {
        let arrow = if show_disabled {
            "\u{25BE}"
        } else {
            "\u{25B8}"
        };
        let toggle = button(
            text(format!("{arrow} DISABLED · {}", disabled.len()))
                .size(11)
                .font(BOLD)
                .color(style::MUTED),
        )
        .on_press(Message::ToggleDisabledSection)
        .style(button::text)
        .padding(0);
        let mut group = column![toggle].spacing(6);
        if show_disabled {
            group = group.push(card(&disabled, now));
        }
        body = body.push(group);
    }

    container(
        column![
            header,
            scrollable(body.padding(padding::right(12))).height(Length::Fill)
        ]
        .spacing(12)
        .height(Length::Fill),
    )
    .padding(padding::top(14).left(20).right(8).bottom(8))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn section_label<'a>(label: &str, count: usize, color: Color) -> Element<'a, Message> {
    text(format!("{label} · {count}"))
        .size(11)
        .font(BOLD)
        .color(color)
        .into()
}

/// Bordered group of one-line rows separated by hairlines.
fn card<'a>(items: &[&'a TunnelInfo], now: u64) -> Element<'a, Message> {
    let mut rows = column![];
    for (i, t) in items.iter().enumerate() {
        if i > 0 {
            rows = rows.push(rule::horizontal(1));
        }
        rows = rows.push(tunnel_row(t, now));
    }
    surface(rows, style::CARD_BG, style::CARD_BORDER)
}

fn tunnel_row(t: &TunnelInfo, now: u64) -> Element<'_, Message> {
    let muted = if t.enabled {
        style::TEXT_SECONDARY
    } else {
        style::DISABLED
    };
    let name_color = if t.enabled {
        Color::BLACK
    } else {
        style::DISABLED
    };
    let (detail, detail_color) = row_detail(t, now);

    row![
        status_dot(t),
        text(&t.name)
            .size(14)
            .font(BOLD)
            .color(name_color)
            .width(130),
        text(route(t))
            .size(12)
            .font(MONO)
            .color(muted)
            .width(Length::Fill),
        text(detail).size(12).color(detail_color),
        primary_action(t),
        link_button("Log", Message::ShowLogs(Some(t.id.clone()))),
        link_button("Edit", Message::EditTunnel(t.id.clone())),
    ]
    .spacing(10)
    .align_y(Center)
    .padding([8, 14])
    .into()
}

/// Expanded card for a failing tunnel: what failed, why, and what to do.
fn attention_card(t: &TunnelInfo, now: u64) -> Element<'_, Message> {
    let error = t.last_error.as_deref().unwrap_or("unknown error");
    let summary = error_hint::summarize(error);

    let title_row = row![
        status_dot(t),
        text(&t.name).size(14).font(BOLD),
        text(route(t)).size(12).font(MONO).color(style::MUTED),
        space::horizontal(),
        text(retry_label(t.next_retry_at, now))
            .size(12)
            .color(style::MUTED),
    ]
    .spacing(10)
    .align_y(Center);

    let mut explanation = column![
        text(summary.title)
            .size(13)
            .font(BOLD)
            .color(style::ERROR_TEXT),
        text(error_hint::key_line(error))
            .size(12)
            .font(MONO)
            .color(style::TEXT_SECONDARY),
    ]
    .spacing(4);
    if let Some(hint) = summary.hint {
        explanation = explanation.push(text(hint).size(12).color(style::TEXT_SECONDARY));
    }

    let actions = row![
        small_button("Retry now", Message::Connect(t.id.clone()), style::primary_button),
        small_button(
            "View log",
            Message::ShowLogs(Some(t.id.clone())),
            style::secondary_button
        ),
        small_button("Edit", Message::EditTunnel(t.id.clone()), style::secondary_button),
    ]
    .spacing(8);

    let content = column![
        title_row,
        container(column![explanation, actions].spacing(10)).padding(padding::left(22)),
    ]
    .spacing(8)
    .padding([12, 14]);

    surface(content, style::ATTENTION_BG, style::ATTENTION_BORDER)
}

fn surface<'a>(
    content: impl Into<Element<'a, Message>>,
    background: Color,
    border: Color,
) -> Element<'a, Message> {
    container(content)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(background.into()),
            border: Border {
                color: border,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

fn small_button<'a>(
    label: &'a str,
    msg: Message,
    style: fn(&iced::Theme, button::Status) -> button::Style,
) -> Element<'a, Message> {
    button(text(label).size(13))
        .on_press(msg)
        .style(style)
        .padding([6, 14])
        .into()
}

fn link_button(label: &str, msg: Message) -> Element<'_, Message> {
    button(text(label).size(12))
        .on_press(msg)
        .style(style::link_button)
        .padding([4, 2])
        .into()
}

fn status_dot(t: &TunnelInfo) -> Element<'_, Message> {
    let (glyph, color) = if !t.enabled {
        ("\u{2014}", style::DISABLED)
    } else {
        match t.status {
            TunnelStatus::Connected => ("\u{25CF}", style::CONNECTED),
            TunnelStatus::Connecting => ("\u{25D0}", style::CONNECTING),
            TunnelStatus::Error => ("\u{25CF}", style::ERROR),
            TunnelStatus::Standby | TunnelStatus::Disconnected => ("\u{25CB}", style::DISCONNECTED),
        }
    };
    text(glyph).size(12).color(color).width(12).into()
}

fn primary_action(t: &TunnelInfo) -> Element<'_, Message> {
    let id = t.id.clone();
    let (label, msg) = if !t.enabled {
        ("Enable", Message::ToggleEnabled(id, true))
    } else {
        match t.status {
            TunnelStatus::Connected => ("Disconnect", Message::Disconnect(id)),
            TunnelStatus::Connecting => ("Cancel", Message::Disconnect(id)),
            TunnelStatus::Standby => ("Connect now", Message::Connect(id)),
            TunnelStatus::Disconnected | TunnelStatus::Error => ("Connect", Message::Connect(id)),
        }
    };
    button(text(label).size(12))
        .on_press(msg)
        .style(style::secondary_button)
        .padding([4, 10])
        .into()
}

/// Right-hand status text for a row and its color.
fn row_detail(t: &TunnelInfo, now: u64) -> (String, Color) {
    if !t.enabled {
        return ("disabled".into(), style::DISABLED);
    }
    match t.status {
        TunnelStatus::Connected => (session_uptime(t, now).unwrap_or_default(), style::MUTED),
        TunnelStatus::Connecting => ("connecting\u{2026}".into(), style::MUTED),
        TunnelStatus::Standby if t.last_error.is_some() => {
            ("last attempt failed".into(), style::ERROR_TEXT)
        }
        TunnelStatus::Standby => (String::new(), style::MUTED),
        TunnelStatus::Disconnected => (format!("{} mode", t.mode), style::MUTED),
        TunnelStatus::Error => (retry_label(t.next_retry_at, now), style::ERROR_TEXT),
    }
}

/// ":9000 → 172.19.110.38:80 via aros"; SOCKS and reverse read naturally too.
fn route(t: &TunnelInfo) -> String {
    let remote = t.remote.as_deref().unwrap_or("?");
    match t.tunnel_type.as_str() {
        "socks" => format!(":{} SOCKS5 via {}", t.local_port, t.host),
        "reverse" => format!("{remote} \u{2192} :{} via {}", t.local_port, t.host),
        _ => format!(":{} \u{2192} {remote} via {}", t.local_port, t.host),
    }
}

fn retry_label(next_retry_at: Option<u64>, now: u64) -> String {
    match next_retry_at {
        Some(at) if at > now => format!("retry in {}", format_duration(at - now)),
        Some(_) => "retrying\u{2026}".into(),
        None => String::new(),
    }
}

/// Length of the current session, from the daemon's session start time.
fn session_uptime(t: &TunnelInfo, now: u64) -> Option<String> {
    let start = t.stats.as_ref()?.current_session_start.as_deref()?;
    let start = chrono::DateTime::parse_from_rfc3339(start)
        .ok()?
        .timestamp();
    let secs = now.saturating_sub(u64::try_from(start).ok()?);
    Some(format_duration(secs))
}

fn format_duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {}h", secs / 86400, (secs % 86400) / 3600),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::protocol::TunnelStats;

    fn tunnel(status: TunnelStatus, enabled: bool) -> TunnelInfo {
        TunnelInfo {
            id: "t".into(),
            name: "T".into(),
            tunnel_type: "local".into(),
            mode: "auto".into(),
            status,
            local_port: 5432,
            remote: Some("db.internal:5432".into()),
            host: "bastion".into(),
            enabled,
            last_error: None,
            stats: None,
            next_retry_at: None,
        }
    }

    #[test]
    fn sections_by_state() {
        assert_eq!(
            section_of(&tunnel(TunnelStatus::Error, true)),
            Section::Attention
        );
        assert_eq!(
            section_of(&tunnel(TunnelStatus::Connecting, true)),
            Section::Active
        );
        assert_eq!(
            section_of(&tunnel(TunnelStatus::Standby, true)),
            Section::Standby
        );
        assert_eq!(
            section_of(&tunnel(TunnelStatus::Disconnected, true)),
            Section::Idle
        );
        assert_eq!(
            section_of(&tunnel(TunnelStatus::Error, false)),
            Section::Disabled
        );
    }

    #[test]
    fn routes_per_type() {
        let mut t = tunnel(TunnelStatus::Connected, true);
        assert_eq!(route(&t), ":5432 \u{2192} db.internal:5432 via bastion");
        t.tunnel_type = "socks".into();
        assert_eq!(route(&t), ":5432 SOCKS5 via bastion");
        t.tunnel_type = "reverse".into();
        t.remote = Some("0.0.0.0:9000".into());
        assert_eq!(route(&t), "0.0.0.0:9000 \u{2192} :5432 via bastion");
    }

    #[test]
    fn retry_countdown() {
        assert_eq!(retry_label(Some(116), 100), "retry in 16s");
        assert_eq!(retry_label(Some(100), 100), "retrying\u{2026}");
        assert_eq!(retry_label(None, 100), "");
    }

    #[test]
    fn uptime_uses_current_session() {
        let mut t = tunnel(TunnelStatus::Connected, true);
        t.stats = Some(TunnelStats {
            total_connections: 3,
            current_session_start: Some("2026-01-01T00:00:00+00:00".into()),
            total_uptime_seconds: 999_999,
            reconnect_count: 0,
        });
        let start = 1_767_225_600; // 2026-01-01T00:00:00Z
        assert_eq!(session_uptime(&t, start + 8100).as_deref(), Some("2h 15m"));
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(125), "2m");
        assert_eq!(format_duration(90000), "1d 1h");
    }
}
