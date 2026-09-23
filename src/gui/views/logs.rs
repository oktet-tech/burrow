use iced::font::Weight;
use iced::widget::{button, column, container, row, space, text, text_editor};
use iced::{Center, Element, Font, Length, padding};

use crate::gui::app::Message;
use crate::gui::ipc_client::LogEvent;
use crate::gui::style;

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};

const MONO: Font = Font {
    family: iced::font::Family::Monospace,
    ..Font::DEFAULT
};

/// Open log drawer: scope chip (all tunnels or one), actions, and a
/// selectable read-only editor.
pub fn panel<'a>(
    content: &'a text_editor::Content,
    has_logs: bool,
    scope: Option<&'a str>,
) -> Element<'a, Message> {
    let scope_chip: Element<'a, Message> = match scope {
        Some(name) => button(text(format!("{name}  \u{00D7}")).size(12))
            .on_press(Message::ShowLogs(None))
            .style(style::secondary_button)
            .padding([2, 8])
            .into(),
        None => text("All tunnels").size(12).color(style::MUTED).into(),
    };

    let header = row![
        text("Logs").size(15).font(BOLD),
        scope_chip,
        space::horizontal(),
        button(text("Clear").size(12))
            .on_press_maybe(has_logs.then_some(Message::ClearLogs))
            .style(style::secondary_button)
            .padding([3, 10]),
        button(text("Export").size(12))
            .on_press_maybe(has_logs.then_some(Message::ExportLogs))
            .style(style::secondary_button)
            .padding([3, 10]),
        button(text("Hide").size(12))
            .on_press(Message::HideLogs)
            .style(style::secondary_button)
            .padding([3, 10]),
    ]
    .spacing(8)
    .align_y(Center);

    let editor = text_editor(content)
        .font(MONO)
        .size(12)
        .on_action(Message::LogEditorAction)
        .height(Length::Fill);

    container(column![header, editor].spacing(8).height(Length::Fill))
        .padding(padding::top(10).left(20).right(20).bottom(14))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Collapsed drawer: a button to open all logs plus the latest line.
pub fn collapsed_bar(last: Option<&LogEvent>) -> Element<'_, Message> {
    let preview = last.map(format_log_line).unwrap_or_default();
    row![
        button(text("Show logs").size(12))
            .on_press(Message::ShowLogs(None))
            .style(style::secondary_button)
            .padding([3, 10]),
        text(preview)
            .size(11)
            .font(MONO)
            .color(style::MUTED)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(10)
    .align_y(Center)
    .padding([8, 20])
    .into()
}

/// Whether an event belongs in a log view scoped to `tunnel_id`.
pub fn matches_scope(event: &LogEvent, tunnel_id: Option<&str>) -> bool {
    tunnel_id.is_none_or(|id| event.tunnel_id.as_deref() == Some(id))
}

/// Format a single log event into a display line.
pub fn format_log_line(event: &LogEvent) -> String {
    let time = extract_time(&event.timestamp);
    format!(
        "{time} {:<5} [{}] {}",
        event.level, event.target, event.message
    )
}

/// Build full text content from the log buffer.
pub fn build_log_text<'a>(logs: impl IntoIterator<Item = &'a LogEvent>) -> String {
    let mut out = String::new();
    for (i, event) in logs.into_iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format_log_line(event));
    }
    out
}

/// Extract HH:MM:SS from a chrono-formatted timestamp ("2024-01-15T10:30:01.123").
/// Falls back to the raw string (truncated) for non-ISO formats.
fn extract_time(timestamp: &str) -> String {
    if let Some(t_pos) = timestamp.find('T') {
        let rest = &timestamp[t_pos + 1..];
        if rest.len() >= 8 {
            return rest[..8].to_string();
        }
    }
    if timestamp.len() > 12 {
        timestamp[..12].to_string()
    } else {
        timestamp.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_iso_time() {
        assert_eq!(extract_time("2024-01-15T10:30:01.123"), "10:30:01");
    }

    #[test]
    fn extract_iso_with_zone() {
        assert_eq!(extract_time("2024-01-15T10:30:01.123456Z"), "10:30:01");
    }

    #[test]
    fn extract_non_iso_short() {
        assert_eq!(extract_time("10:30:01"), "10:30:01");
    }

    #[test]
    fn extract_non_iso_long() {
        assert_eq!(extract_time("some long timestamp value"), "some long ti");
    }

    #[test]
    fn scope_filters_by_tunnel() {
        let mut event = LogEvent {
            timestamp: String::new(),
            level: "INFO".into(),
            target: "t".into(),
            message: "m".into(),
            tunnel_id: Some("db".into()),
        };
        assert!(matches_scope(&event, None));
        assert!(matches_scope(&event, Some("db")));
        assert!(!matches_scope(&event, Some("other")));
        event.tunnel_id = None;
        assert!(!matches_scope(&event, Some("db")));
    }

    #[test]
    fn format_includes_level_and_target() {
        let event = LogEvent {
            timestamp: "2024-01-15T10:30:01.123".into(),
            level: "ERROR".into(),
            target: "daemon::tunnel".into(),
            message: "connection refused".into(),
            tunnel_id: None,
        };
        assert_eq!(
            format_log_line(&event),
            "10:30:01 ERROR [daemon::tunnel] connection refused"
        );
    }
}
