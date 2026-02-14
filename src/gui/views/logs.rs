use iced::font::Weight;
use iced::widget::{
    button, column, container, row, rule, space, text, text_editor,
};
use iced::{Center, Element, Font, Length};

use crate::gui::app::Message;
use crate::gui::ipc_client::LogEvent;

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};

const MONO: Font = Font {
    family: iced::font::Family::Monospace,
    ..Font::DEFAULT
};

/// Log viewer panel: selectable text editor (read-only) with Clear and Export buttons.
pub fn view<'a>(content: &'a text_editor::Content, has_logs: bool) -> Element<'a, Message> {
    let header = row![
        text("Logs").size(18).font(BOLD),
        space::horizontal(),
        button(text("Clear").size(13))
            .on_press_maybe(has_logs.then_some(Message::ClearLogs))
            .style(button::secondary)
            .padding([4, 12]),
        button(text("Export").size(13))
            .on_press_maybe(has_logs.then_some(Message::ExportLogs))
            .style(button::secondary)
            .padding([4, 12]),
    ]
    .spacing(8)
    .align_y(Center);

    let editor = text_editor(content)
        .font(MONO)
        .size(12)
        .on_action(Message::LogEditorAction)
        .height(Length::Fill);

    container(column![header, rule::horizontal(1), editor].spacing(8).height(Length::Fill))
        .padding(16)
        .width(Length::Fill)
        .height(Length::FillPortion(1))
        .into()
}

/// Format a single log event into a display line.
pub fn format_log_line(event: &LogEvent) -> String {
    let time = extract_time(&event.timestamp);
    format!("{time} {:<5} [{}] {}", event.level, event.target, event.message)
}

/// Build full text content from the log buffer.
pub fn build_log_text(logs: &[LogEvent]) -> String {
    let mut out = String::new();
    for (i, event) in logs.iter().enumerate() {
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
    fn format_includes_level_and_target() {
        let event = LogEvent {
            timestamp: "2024-01-15T10:30:01.123".into(),
            level: "ERROR".into(),
            target: "daemon::tunnel".into(),
            message: "connection refused".into(),
        };
        assert_eq!(
            format_log_line(&event),
            "10:30:01 ERROR [daemon::tunnel] connection refused"
        );
    }
}
