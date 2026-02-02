use iced::font::Weight;
use iced::widget::{button, column, container, horizontal_rule, horizontal_space, row, scrollable, text};
use iced::{Center, Color, Element, Font, Length};

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

/// Log viewer panel: scrollable log lines with Clear and Export buttons.
/// Anchored to bottom so new entries are always visible.
pub fn view<'a>(logs: &'a [LogEvent]) -> Element<'a, Message> {
    let has_logs = !logs.is_empty();

    let header = row![
        text("Logs").size(18).font(BOLD),
        horizontal_space(),
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

    let mut log_col = column![].spacing(1);
    for event in logs {
        log_col = log_col.push(log_line(event));
    }

    let scroll = scrollable(log_col)
        .anchor_bottom()
        .height(Length::Fill)
        .width(Length::Fill);

    container(column![header, horizontal_rule(1), scroll].spacing(8))
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn log_line(event: &LogEvent) -> Element<'_, Message> {
    let time = extract_time(&event.timestamp);

    let mut msg = text(&event.message).size(12).font(MONO);
    if let Some(color) = level_color(&event.level) {
        msg = msg.color(color);
    }

    row![
        text(time).size(12).font(MONO).color(style::MUTED),
        text(format!("[{}]", event.target))
            .size(12)
            .font(MONO)
            .color(style::MUTED),
        msg,
    ]
    .spacing(6)
    .into()
}

fn level_color(level: &str) -> Option<Color> {
    match level.to_uppercase().as_str() {
        "ERROR" => Some(style::ERROR),
        "WARN" => Some(style::WARN),
        _ => None,
    }
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
    fn level_error_is_red() {
        assert!(level_color("ERROR").is_some());
        assert!(level_color("error").is_some());
    }

    #[test]
    fn level_warn_is_colored() {
        assert!(level_color("WARN").is_some());
        assert!(level_color("warn").is_some());
    }

    #[test]
    fn level_info_is_default() {
        assert!(level_color("INFO").is_none());
        assert!(level_color("DEBUG").is_none());
    }
}
