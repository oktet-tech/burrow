use iced::widget::{button, container};
use iced::{Border, Color, Theme};

// Status indicator colors (Apple system colors, matching tray icons)
pub const CONNECTED: Color = Color::from_rgb(0.298, 0.851, 0.392);
pub const CONNECTING: Color = Color::from_rgb(1.0, 0.8, 0.0);
pub const ERROR: Color = Color::from_rgb(1.0, 0.231, 0.188);
pub const DISCONNECTED: Color = Color::from_rgb(0.6, 0.6, 0.6);

#[allow(dead_code)] // for aggregate tray status indicator
pub const WARN: Color = Color::from_rgb(0.8, 0.55, 0.0);
pub const MUTED: Color = Color::from_rgb(0.5, 0.5, 0.5);
pub const DISABLED: Color = Color::from_rgb(0.7, 0.7, 0.7);

// Text-safe variants of the status colors (4.5:1 on white) for labels.
pub const CONNECTED_TEXT: Color = Color::from_rgb(0.118, 0.498, 0.235);
pub const ERROR_TEXT: Color = Color::from_rgb(0.702, 0.149, 0.118);
pub const TEXT_SECONDARY: Color = Color::from_rgb(0.29, 0.29, 0.31);

// Surfaces for the grouped tunnel list
pub const CARD_BG: Color = Color::WHITE;
pub const CARD_BORDER: Color = Color::from_rgb(0.894, 0.890, 0.871);
pub const ATTENTION_BG: Color = Color::from_rgb(1.0, 0.969, 0.965);
pub const ATTENTION_BORDER: Color = Color::from_rgb(0.945, 0.788, 0.773);

// Window chrome and controls (warm neutral ground, macOS-like controls)
pub const WINDOW_BG: Color = Color::from_rgb(0.969, 0.965, 0.953);
pub const INK: Color = Color::from_rgb(0.114, 0.114, 0.122);
pub const ACCENT: Color = Color::from_rgb(0.039, 0.4, 0.851);
const ACCENT_PRESSED: Color = Color::from_rgb(0.035, 0.341, 0.729);
const CONTROL_BORDER: Color = Color::from_rgb(0.855, 0.851, 0.831);
const CONTROL_HOVER: Color = Color::from_rgb(0.949, 0.945, 0.929);
const CONTROL_PRESSED: Color = Color::from_rgb(0.91, 0.906, 0.886);
const CONTROL_RADIUS: f32 = 7.0;

/// Solid accent button for the main action on a surface.
pub fn primary_button(_: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Active => ACCENT,
        button::Status::Hovered | button::Status::Pressed => ACCENT_PRESSED,
        button::Status::Disabled => ACCENT.scale_alpha(0.45),
    };
    button::Style {
        background: Some(background.into()),
        text_color: Color::WHITE,
        border: Border {
            radius: CONTROL_RADIUS.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// White bordered button for everything else.
pub fn secondary_button(_: &Theme, status: button::Status) -> button::Style {
    let (background, text_color) = match status {
        button::Status::Active => (Color::WHITE, INK),
        button::Status::Hovered => (CONTROL_HOVER, INK),
        button::Status::Pressed => (CONTROL_PRESSED, INK),
        button::Status::Disabled => (Color::WHITE, DISABLED),
    };
    button::Style {
        background: Some(background.into()),
        text_color,
        border: Border {
            color: CONTROL_BORDER,
            width: 1.0,
            radius: CONTROL_RADIUS.into(),
        },
        ..button::Style::default()
    }
}

/// Borderless accent text, for secondary per-row actions like Log and Edit.
pub fn link_button(_: &Theme, status: button::Status) -> button::Style {
    let text_color = match status {
        button::Status::Active => ACCENT,
        button::Status::Hovered | button::Status::Pressed => ACCENT_PRESSED,
        button::Status::Disabled => DISABLED,
    };
    button::Style {
        background: None,
        text_color,
        ..button::Style::default()
    }
}

/// Plain window background behind cards and controls.
pub fn window(_: &Theme) -> container::Style {
    container::Style {
        background: Some(WINDOW_BG.into()),
        text_color: Some(INK),
        ..container::Style::default()
    }
}
