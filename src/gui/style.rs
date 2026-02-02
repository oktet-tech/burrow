use iced::Color;

// Status indicator colors (Apple system colors, matching tray icons)
pub const CONNECTED: Color = Color::from_rgb(0.298, 0.851, 0.392);
pub const CONNECTING: Color = Color::from_rgb(1.0, 0.8, 0.0);
pub const ERROR: Color = Color::from_rgb(1.0, 0.231, 0.188);
pub const DISCONNECTED: Color = Color::from_rgb(0.6, 0.6, 0.6);

pub const WARN: Color = Color::from_rgb(0.8, 0.55, 0.0);
pub const MUTED: Color = Color::from_rgb(0.5, 0.5, 0.5);
pub const DISABLED: Color = Color::from_rgb(0.7, 0.7, 0.7);
