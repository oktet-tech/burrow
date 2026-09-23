use iced::Color;

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
