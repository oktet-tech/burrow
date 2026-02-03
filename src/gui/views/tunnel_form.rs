use iced::font::Weight;
use iced::widget::{
    button, column, container, horizontal_rule, horizontal_space, pick_list, row, scrollable, text,
    text_input,
};
use iced::{Center, Element, Font, Length};

use crate::gui::app::Message;
use crate::gui::style;
use crate::ipc::protocol::TunnelInfo;

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};

// -- Form state --

#[derive(Debug, Clone)]
pub struct TunnelFormState {
    /// None = create mode, Some(id) = edit mode
    pub editing_id: Option<String>,
    pub delete_confirming: bool,
    pub name: String,
    pub id: String,
    pub id_manually_edited: bool,
    pub host: String,
    pub ssh_port: String,
    pub local_port: String,
    pub tunnel_type: TunnelTypeChoice,
    pub mode: ModeChoice,
    // Local-specific
    pub remote_host: String,
    pub remote_port: String,
    // Reverse-specific (remote_port reused, plus these)
    pub local_host: String,
    pub remote_bind: String,
    // Optional SSH
    pub identity: String,
    pub jump_host: String,
    pub jump_port: String,
}

impl Default for TunnelFormState {
    fn default() -> Self {
        Self {
            editing_id: None,
            delete_confirming: false,
            name: String::new(),
            id: String::new(),
            id_manually_edited: false,
            host: String::new(),
            ssh_port: "22".to_string(),
            local_port: String::new(),
            tunnel_type: TunnelTypeChoice::Local,
            mode: ModeChoice::Auto,
            remote_host: "localhost".to_string(),
            remote_port: String::new(),
            local_host: String::new(),
            remote_bind: String::new(),
            identity: String::new(),
            jump_host: String::new(),
            jump_port: String::new(),
        }
    }
}

impl TunnelFormState {
    /// Populate form from an existing tunnel for editing.
    pub fn from_tunnel_info(info: &TunnelInfo) -> Self {
        let tunnel_type = match info.tunnel_type.as_str() {
            "reverse" => TunnelTypeChoice::Reverse,
            "socks" => TunnelTypeChoice::Socks,
            _ => TunnelTypeChoice::Local,
        };
        let mode = match info.mode.as_str() {
            "manual" => ModeChoice::Manual,
            "on-demand" => ModeChoice::OnDemand,
            _ => ModeChoice::Auto,
        };

        // Parse "host:port" from info.remote
        let (remote_host, remote_port) = match info.remote.as_deref() {
            Some(r) if tunnel_type != TunnelTypeChoice::Socks => {
                if let Some((h, p)) = r.rsplit_once(':') {
                    (h.to_string(), p.to_string())
                } else {
                    (r.to_string(), String::new())
                }
            }
            _ => ("localhost".to_string(), String::new()),
        };

        Self {
            editing_id: Some(info.id.clone()),
            delete_confirming: false,
            name: info.name.clone(),
            id: info.id.clone(),
            id_manually_edited: true,
            host: info.host.clone(),
            ssh_port: "22".to_string(), // not exposed in TunnelInfo
            local_port: info.local_port.to_string(),
            tunnel_type,
            mode,
            remote_host,
            remote_port,
            local_host: String::new(),
            remote_bind: String::new(),
            identity: String::new(),
            jump_host: String::new(),
            jump_port: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelTypeChoice {
    Local,
    Reverse,
    Socks,
}

impl std::fmt::Display for TunnelTypeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "Local"),
            Self::Reverse => write!(f, "Reverse"),
            Self::Socks => write!(f, "SOCKS"),
        }
    }
}

impl TunnelTypeChoice {
    pub const ALL: &'static [Self] = &[Self::Local, Self::Reverse, Self::Socks];

    pub fn to_config_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Reverse => "reverse",
            Self::Socks => "socks",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeChoice {
    Auto,
    Manual,
    OnDemand,
}

impl std::fmt::Display for ModeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "Auto"),
            Self::Manual => write!(f, "Manual"),
            Self::OnDemand => write!(f, "On-Demand"),
        }
    }
}

impl ModeChoice {
    pub const ALL: &'static [Self] = &[Self::Auto, Self::Manual, Self::OnDemand];

    pub fn to_config_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
            Self::OnDemand => "on-demand",
        }
    }
}

/// Which form field changed.
#[derive(Debug, Clone)]
pub enum FormField {
    Name,
    Id,
    Host,
    SshPort,
    LocalPort,
    RemoteHost,
    RemotePort,
    LocalHost,
    RemoteBind,
    Identity,
    JumpHost,
    JumpPort,
}

/// Slugify a name into a valid tunnel ID.
pub fn slugify(name: &str) -> String {
    let mut result = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
        } else if ch == ' ' || ch == '_' {
            if !result.ends_with('-') {
                result.push('-');
            }
        }
    }
    // Trim leading/trailing hyphens
    result.trim_matches('-').to_string()
}

/// Build a JSON Value from form state, suitable for tunnel.add IPC.
pub fn build_config_json(state: &TunnelFormState) -> serde_json::Value {
    let mut obj = serde_json::Map::new();

    obj.insert("name".into(), serde_json::Value::String(state.name.clone()));
    obj.insert("host".into(), serde_json::Value::String(state.host.clone()));
    obj.insert("type".into(), serde_json::Value::String(state.tunnel_type.to_config_str().into()));
    obj.insert("mode".into(), serde_json::Value::String(state.mode.to_config_str().into()));

    if let Ok(port) = state.ssh_port.parse::<u16>() {
        obj.insert("port".into(), serde_json::json!(port));
    }
    if let Ok(port) = state.local_port.parse::<u16>() {
        obj.insert("local_port".into(), serde_json::json!(port));
    }

    match state.tunnel_type {
        TunnelTypeChoice::Local => {
            if !state.remote_host.is_empty() {
                obj.insert("remote_host".into(), serde_json::Value::String(state.remote_host.clone()));
            }
            if let Ok(port) = state.remote_port.parse::<u16>() {
                obj.insert("remote_port".into(), serde_json::json!(port));
            }
        }
        TunnelTypeChoice::Reverse => {
            if let Ok(port) = state.remote_port.parse::<u16>() {
                obj.insert("remote_port".into(), serde_json::json!(port));
            }
            if !state.local_host.is_empty() {
                obj.insert("local_host".into(), serde_json::Value::String(state.local_host.clone()));
            }
            if !state.remote_bind.is_empty() {
                obj.insert("remote_bind".into(), serde_json::Value::String(state.remote_bind.clone()));
            }
        }
        TunnelTypeChoice::Socks => {}
    }

    if !state.identity.is_empty() {
        obj.insert("identity".into(), serde_json::Value::String(state.identity.clone()));
    }
    if !state.jump_host.is_empty() {
        obj.insert("jump_host".into(), serde_json::Value::String(state.jump_host.clone()));
    }
    if let Ok(port) = state.jump_port.parse::<u16>() {
        obj.insert("jump_port".into(), serde_json::json!(port));
    }

    serde_json::Value::Object(obj)
}

// -- View --

pub fn view<'a>(state: &'a TunnelFormState, error: &'a Option<String>) -> Element<'a, Message> {
    let is_edit = state.editing_id.is_some();
    let title = if is_edit { "Edit Tunnel" } else { "New Tunnel" };
    let submit_label = if is_edit { "Save" } else { "Create" };

    let header = row![
        button(text("Cancel").size(13))
            .on_press(Message::CancelNewTunnelForm)
            .style(button::secondary)
            .padding([4, 12]),
        horizontal_space(),
        text(title).size(18).font(BOLD),
        horizontal_space(),
        button(text(submit_label).size(13))
            .on_press(Message::SubmitNewTunnel)
            .style(button::primary)
            .padding([4, 12]),
    ]
    .spacing(8)
    .align_y(Center);

    let mut form = column![header, horizontal_rule(1)].spacing(10);

    // Name + ID
    form = form.push(field_row(
        "Name",
        text_input("My Database Tunnel", &state.name)
            .on_input(|v| Message::FormFieldChanged(FormField::Name, v))
            .size(14)
            .into(),
    ));

    // ID field: read-only when editing
    let id_input = if is_edit {
        text_input("my-database-tunnel", &state.id).size(14)
    } else {
        text_input("my-database-tunnel", &state.id)
            .on_input(|v| Message::FormFieldChanged(FormField::Id, v))
            .size(14)
    };
    form = form.push(field_row(
        "ID",
        column![
            id_input,
            text("Lowercase alphanumeric with hyphens")
                .size(11)
                .color(style::MUTED),
        ]
        .spacing(2)
        .into(),
    ));

    // Host + SSH port
    form = form.push(
        row![
            field_col(
                "Host",
                text_input("bastion.example.com", &state.host)
                    .on_input(|v| Message::FormFieldChanged(FormField::Host, v))
                    .size(14)
                    .into(),
            ),
            field_col(
                "SSH Port",
                text_input("22", &state.ssh_port)
                    .on_input(|v| Message::FormFieldChanged(FormField::SshPort, v))
                    .size(14)
                    .width(80)
                    .into(),
            ),
        ]
        .spacing(12),
    );

    // Type + Mode pickers
    form = form.push(
        row![
            field_col(
                "Type",
                pick_list(
                    TunnelTypeChoice::ALL,
                    Some(state.tunnel_type),
                    |v| Message::FormTypeChanged(v),
                )
                .text_size(14)
                .into(),
            ),
            field_col(
                "Mode",
                pick_list(ModeChoice::ALL, Some(state.mode), |v| Message::FormModeChanged(v))
                    .text_size(14)
                    .into(),
            ),
        ]
        .spacing(12),
    );

    // Local port
    form = form.push(field_row(
        "Local Port",
        text_input("5432", &state.local_port)
            .on_input(|v| Message::FormFieldChanged(FormField::LocalPort, v))
            .size(14)
            .width(120)
            .into(),
    ));

    // Type-conditional fields
    match state.tunnel_type {
        TunnelTypeChoice::Local => {
            form = form.push(
                row![
                    field_col(
                        "Remote Host",
                        text_input("localhost", &state.remote_host)
                            .on_input(|v| Message::FormFieldChanged(FormField::RemoteHost, v))
                            .size(14)
                            .into(),
                    ),
                    field_col(
                        "Remote Port",
                        text_input("5432", &state.remote_port)
                            .on_input(|v| Message::FormFieldChanged(FormField::RemotePort, v))
                            .size(14)
                            .width(120)
                            .into(),
                    ),
                ]
                .spacing(12),
            );
        }
        TunnelTypeChoice::Reverse => {
            form = form.push(field_row(
                "Remote Port",
                text_input("9000", &state.remote_port)
                    .on_input(|v| Message::FormFieldChanged(FormField::RemotePort, v))
                    .size(14)
                    .width(120)
                    .into(),
            ));
            form = form.push(
                row![
                    field_col(
                        "Local Host (opt)",
                        text_input("127.0.0.1", &state.local_host)
                            .on_input(|v| Message::FormFieldChanged(FormField::LocalHost, v))
                            .size(14)
                            .into(),
                    ),
                    field_col(
                        "Remote Bind (opt)",
                        text_input("localhost", &state.remote_bind)
                            .on_input(|v| Message::FormFieldChanged(FormField::RemoteBind, v))
                            .size(14)
                            .into(),
                    ),
                ]
                .spacing(12),
            );
        }
        TunnelTypeChoice::Socks => {}
    }

    // Optional SSH section
    form = form.push(horizontal_rule(1));
    form = form.push(text("SSH Options (optional)").size(14).color(style::MUTED));

    form = form.push(field_row(
        "Identity File",
        text_input("~/.ssh/id_rsa", &state.identity)
            .on_input(|v| Message::FormFieldChanged(FormField::Identity, v))
            .size(14)
            .into(),
    ));
    form = form.push(
        row![
            field_col(
                "Jump Host",
                text_input("jump.example.com", &state.jump_host)
                    .on_input(|v| Message::FormFieldChanged(FormField::JumpHost, v))
                    .size(14)
                    .into(),
            ),
            field_col(
                "Jump Port",
                text_input("22", &state.jump_port)
                    .on_input(|v| Message::FormFieldChanged(FormField::JumpPort, v))
                    .size(14)
                    .width(80)
                    .into(),
            ),
        ]
        .spacing(12),
    );

    // Delete section (edit mode only)
    if is_edit {
        form = form.push(horizontal_rule(1));
        if state.delete_confirming {
            form = form.push(
                row![
                    text("Delete this tunnel?").size(14),
                    horizontal_space(),
                    button(text("Cancel").size(13))
                        .on_press(Message::FormCancelDelete)
                        .style(button::secondary)
                        .padding([4, 12]),
                    button(text("Delete").size(13))
                        .on_press(Message::FormDeleteTunnel)
                        .style(button::danger)
                        .padding([4, 12]),
                ]
                .spacing(8)
                .align_y(Center),
            );
        } else {
            form = form.push(
                button(text("Delete Tunnel").size(13).color(style::ERROR))
                    .on_press(Message::FormConfirmDelete)
                    .style(button::text)
                    .padding([4, 12]),
            );
        }
    }

    // Error display
    if let Some(err) = error {
        form = form.push(horizontal_rule(1));
        form = form.push(text(err).size(13).color(style::ERROR));
    }

    container(scrollable(form).height(Length::Fill))
        .padding(16)
        .width(Length::Fill)
        .into()
}

fn field_row<'a>(label: &'a str, input: Element<'a, Message>) -> Element<'a, Message> {
    column![text(label).size(13).color(style::MUTED), input]
        .spacing(4)
        .into()
}

fn field_col<'a>(label: &'a str, input: Element<'a, Message>) -> Element<'a, Message> {
    column![text(label).size(13).color(style::MUTED), input]
        .spacing(4)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Dev Database"), "dev-database");
    }

    #[test]
    fn slugify_underscores() {
        assert_eq!(slugify("my_tunnel_123"), "my-tunnel-123");
    }

    #[test]
    fn slugify_special_chars() {
        assert_eq!(slugify("Hello! World@#$"), "hello-world");
    }

    #[test]
    fn slugify_leading_trailing_spaces() {
        assert_eq!(slugify(" test "), "test");
    }

    #[test]
    fn slugify_empty() {
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn build_json_local() {
        let state = TunnelFormState {
            name: "Test".into(),
            host: "example.com".into(),
            ssh_port: "22".into(),
            local_port: "5432".into(),
            tunnel_type: TunnelTypeChoice::Local,
            remote_host: "db.internal".into(),
            remote_port: "5432".into(),
            ..Default::default()
        };
        let json = build_config_json(&state);
        assert_eq!(json["type"], "local");
        assert_eq!(json["remote_host"], "db.internal");
        assert_eq!(json["remote_port"], 5432);
    }

    #[test]
    fn build_json_socks_no_remote() {
        let state = TunnelFormState {
            name: "Proxy".into(),
            host: "example.com".into(),
            local_port: "1080".into(),
            tunnel_type: TunnelTypeChoice::Socks,
            ..Default::default()
        };
        let json = build_config_json(&state);
        assert_eq!(json["type"], "socks");
        assert!(json.get("remote_host").is_none());
        assert!(json.get("remote_port").is_none());
    }
}
