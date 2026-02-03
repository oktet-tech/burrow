use std::collections::{HashMap, HashSet};

use iced::futures::SinkExt;
use iced::widget::{column, container, text, text_editor};
use iced::window;
use iced::{Element, Length, Size, Subscription, Task};
use serde_json::json;
use tray_icon::TrayIcon;

use crate::ipc::protocol::{TunnelInfo, TunnelStatus};

use super::ipc_client::{DaemonEvent, GuiIpcClient, IpcError, LogEvent};
use super::notifications;
use super::tray::{self, AggregateStatus, TUNNEL_ID_PREFIX};
use super::views::tunnel_form::{
    self, FormField, ModeChoice, TunnelFormState, TunnelTypeChoice,
};

const MAX_LOG_LINES: usize = 1000;

// -- Messages --

#[derive(Debug, Clone)]
#[allow(dead_code)] // some variants are reserved for GUI actions not yet wired
pub enum Message {
    // IPC subscription events
    IpcReady(GuiIpcClient),
    DaemonConnected,
    DaemonDisconnected(String),
    TunnelStatusUpdate(Vec<TunnelInfo>),
    LogReceived(LogEvent),

    // Tray menu
    TrayMenuEvent(String),

    // Window lifecycle
    WindowOpened(window::Id),
    WindowCloseRequested(window::Id),
    Quit,

    // User actions -- tunnels
    ToggleEnabled(String, bool),
    Connect(String),
    Disconnect(String),
    ConnectAll,
    DisconnectAll,
    RestartAll,
    ReloadConfig,

    // Tunnel form (create + edit)
    ShowNewTunnelForm,
    EditTunnel(String),
    CancelNewTunnelForm,
    SubmitNewTunnel,
    FormFieldChanged(FormField, String),
    FormTypeChanged(TunnelTypeChoice),
    FormModeChanged(ModeChoice),
    TunnelAdded(Result<(), String>),
    TunnelUpdated(Result<(), String>),

    // Delete tunnel (from edit form)
    FormConfirmDelete,
    FormCancelDelete,
    FormDeleteTunnel,
    TunnelRemoved(Result<(), String>),

    // User actions -- logs
    ClearLogs,
    ExportLogs,
    LogEditorAction(text_editor::Action),

    // Action completions
    ActionDone,
    ConfigReloaded,
}

// -- Application state --

pub struct BurrowApp {
    tray: TrayIcon,
    current_tray_status: AggregateStatus,
    /// Tunnel snapshot from the last menu rebuild, used to avoid
    /// redundant set_menu calls that dismiss the open dropdown.
    last_menu_tunnels: Vec<TunnelInfo>,
    last_menu_daemon_connected: bool,
    window_id: Option<window::Id>,

    tunnels: Vec<TunnelInfo>,
    logs: Vec<LogEvent>,
    log_content: text_editor::Content,
    daemon_connected: bool,
    client: Option<GuiIpcClient>,
    initial_fetch_done: bool,
    pending_user_disconnect: HashSet<String>,

    show_new_tunnel_form: bool,
    form_state: TunnelFormState,
    form_error: Option<String>,
}

impl BurrowApp {
    /// Called inside the iced event loop, after NSApplication is initialized.
    pub fn new() -> (Self, Task<Message>) {
        #[cfg(target_os = "macos")]
        super::hide_from_dock();

        let tray = tray::create_tray();

        (
            Self {
                tray,
                current_tray_status: AggregateStatus::NoneConnected,
                last_menu_tunnels: Vec::new(),
                last_menu_daemon_connected: false,
                window_id: None,

                tunnels: Vec::new(),
                logs: Vec::new(),
                log_content: text_editor::Content::new(),
                daemon_connected: false,
                client: None,
                initial_fetch_done: false,
                pending_user_disconnect: HashSet::new(),

                show_new_tunnel_form: false,
                form_state: TunnelFormState::default(),
                form_error: None,
            },
            Task::none(),
        )
    }

    pub fn title(&self, _window: window::Id) -> String {
        "Burrow".into()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::IpcReady(client) => {
                self.client = Some(client);
                Task::none()
            }
            Message::DaemonConnected => {
                self.daemon_connected = true;
                self.initial_fetch_done = false;
                self.pending_user_disconnect.clear();
                self.sync_tray();
                self.fetch_tunnels()
            }
            Message::DaemonDisconnected(_) => {
                self.daemon_connected = false;
                self.initial_fetch_done = false;
                self.tunnels.clear();
                self.sync_tray();
                Task::none()
            }
            Message::TunnelStatusUpdate(tunnels) => {
                if self.initial_fetch_done {
                    self.emit_tunnel_notifications(&tunnels);
                } else {
                    self.initial_fetch_done = true;
                }
                self.tunnels = tunnels;
                self.sync_tray();
                Task::none()
            }
            Message::LogReceived(event) => {
                if event.message.contains("network change detected") {
                    notifications::network_changed();
                }
                if self.logs.len() >= MAX_LOG_LINES {
                    self.logs.remove(0);
                }
                self.logs.push(event);
                self.rebuild_log_content();
                Task::none()
            }
            // Tray menu dispatch
            Message::TrayMenuEvent(id) => self.handle_tray_event(&id),

            // Window lifecycle
            Message::WindowOpened(id) => {
                self.window_id = Some(id);
                Task::none()
            }
            Message::WindowCloseRequested(id) => {
                if self.window_id == Some(id) {
                    self.window_id = None;
                    window::close(id)
                } else {
                    Task::none()
                }
            }
            Message::Quit => self.quit(),

            // Tunnel actions
            Message::ToggleEnabled(id, enabled) => {
                if !enabled {
                    // Suppress "disconnected" notification for user-initiated disable
                    self.pending_user_disconnect.insert(id.clone());
                }
                let method = if enabled {
                    "tunnel.enable"
                } else {
                    "tunnel.disable"
                };
                self.send_action(method, json!({ "id": id }))
            }
            Message::Connect(id) => {
                self.send_action("tunnel.connect", json!({ "id": id }))
            }
            Message::Disconnect(id) => {
                self.pending_user_disconnect.insert(id.clone());
                self.send_action("tunnel.disconnect", json!({ "id": id }))
            }
            Message::ConnectAll => self.send_action("tunnel.connect_all", json!({})),
            Message::DisconnectAll => {
                for t in &self.tunnels {
                    if t.status == TunnelStatus::Connected {
                        self.pending_user_disconnect.insert(t.id.clone());
                    }
                }
                self.send_action("tunnel.disconnect_all", json!({}))
            }
            Message::RestartAll => self.send_action("tunnel.restart_all", json!({})),
            Message::ReloadConfig => self.send_config_reload(),

            // Tunnel form (create + edit)
            Message::ShowNewTunnelForm => {
                self.show_new_tunnel_form = true;
                self.form_state = TunnelFormState::default();
                self.form_error = None;
                self.open_window()
            }
            Message::EditTunnel(id) => {
                if let Some(info) = self.tunnels.iter().find(|t| t.id == id) {
                    self.show_new_tunnel_form = true;
                    self.form_state = TunnelFormState::from_tunnel_info(info);
                    self.form_error = None;
                }
                self.open_window()
            }
            Message::CancelNewTunnelForm => {
                self.show_new_tunnel_form = false;
                self.form_error = None;
                Task::none()
            }
            Message::SubmitNewTunnel => self.submit_tunnel(),
            Message::FormFieldChanged(field, value) => {
                self.update_form_field(field, value);
                Task::none()
            }
            Message::FormTypeChanged(t) => {
                self.form_state.tunnel_type = t;
                Task::none()
            }
            Message::FormModeChanged(m) => {
                self.form_state.mode = m;
                Task::none()
            }
            Message::TunnelAdded(Ok(())) | Message::TunnelUpdated(Ok(())) => {
                self.show_new_tunnel_form = false;
                self.form_error = None;
                Task::none()
            }
            Message::TunnelAdded(Err(msg)) | Message::TunnelUpdated(Err(msg)) => {
                self.form_error = Some(msg);
                Task::none()
            }

            // Delete tunnel (from edit form)
            Message::FormConfirmDelete => {
                self.form_state.delete_confirming = true;
                Task::none()
            }
            Message::FormCancelDelete => {
                self.form_state.delete_confirming = false;
                Task::none()
            }
            Message::FormDeleteTunnel => {
                if let Some(ref id) = self.form_state.editing_id {
                    let task = self.send_tunnel_remove(id);
                    self.show_new_tunnel_form = false;
                    task
                } else {
                    Task::none()
                }
            }
            Message::TunnelRemoved(Ok(())) => Task::none(),
            Message::TunnelRemoved(Err(msg)) => {
                tracing::error!(error = %msg, "tunnel remove failed");
                Task::none()
            }

            Message::ClearLogs => {
                self.logs.clear();
                self.log_content = text_editor::Content::new();
                Task::none()
            }
            Message::ExportLogs => {
                export_logs(&self.logs);
                Task::none()
            }
            Message::LogEditorAction(action) => {
                // Read-only: allow selection and cursor movement, block edits
                if !matches!(action, text_editor::Action::Edit(_)) {
                    self.log_content.perform(action);
                }
                Task::none()
            }
            Message::ActionDone => Task::none(),
            Message::ConfigReloaded => {
                notifications::config_reloaded();
                Task::none()
            }
        }
    }

    pub fn view(&self, id: window::Id) -> Element<'_, Message> {
        if self.window_id != Some(id) {
            return container(text("")).into();
        }

        if !self.daemon_connected {
            return container(
                text("Waiting for daemon...")
                    .size(16)
                    .color(super::style::MUTED),
            )
            .center(Length::Fill)
            .into();
        }

        if self.show_new_tunnel_form {
            tunnel_form::view(&self.form_state, &self.form_error)
        } else {
            column![
                container(super::views::tunnel_list::view(&self.tunnels))
                    .height(Length::FillPortion(2)),
                container(super::views::logs::view(&self.log_content, !self.logs.is_empty()))
                    .height(Length::FillPortion(1)),
            ]
            .height(Length::Fill)
            .into()
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run(ipc_subscription),
            Subscription::run(tray_event_subscription),
            window::close_requests().map(Message::WindowCloseRequested),
        ])
    }

    // -- Tray sync --

    fn sync_tray(&mut self) {
        // Only rebuild menu when menu-visible fields change; set_menu dismisses
        // an open dropdown, so we must not rebuild on stats-only changes (uptime).
        if !tray_relevant_eq(&self.tunnels, &self.last_menu_tunnels)
            || self.daemon_connected != self.last_menu_daemon_connected
        {
            self.tray.set_menu(Some(Box::new(tray::build_menu(
                &self.tunnels,
                self.daemon_connected,
            ))));
            self.last_menu_tunnels = self.tunnels.clone();
            self.last_menu_daemon_connected = self.daemon_connected;
        }

        let new_status = tray::aggregate_status(&self.tunnels, self.daemon_connected);
        if new_status != self.current_tray_status {
            self.current_tray_status = new_status;
            let (icon, is_template) = tray::icon_for_status(new_status);
            let _ = self.tray.set_icon(Some(icon));
            self.tray.set_icon_as_template(is_template);
        }
    }

    // -- Tray menu event handling --

    fn handle_tray_event(&mut self, id: &str) -> Task<Message> {
        match id {
            "open-window" => self.open_window(),
            "new-tunnel" => self.update(Message::ShowNewTunnelForm),
            "quit" => self.quit(),
            "connect-all" => self.update(Message::ConnectAll),
            "disconnect-all" => self.update(Message::DisconnectAll),
            _ => {
                if let Some(tunnel_id) = id.strip_prefix(TUNNEL_ID_PREFIX) {
                    let connected = self.tunnels.iter().any(|t| {
                        t.id == tunnel_id && t.status == TunnelStatus::Connected
                    });
                    if connected {
                        self.update(Message::Disconnect(tunnel_id.to_string()))
                    } else {
                        self.update(Message::Connect(tunnel_id.to_string()))
                    }
                } else {
                    Task::none()
                }
            }
        }
    }

    // -- Window management --

    fn open_window(&mut self) -> Task<Message> {
        if let Some(id) = self.window_id {
            return window::gain_focus(id);
        }

        let (id, open) = window::open(window::Settings {
            size: Size::new(800.0, 600.0),
            ..Default::default()
        });
        self.window_id = Some(id);
        open.map(Message::WindowOpened)
    }

    fn quit(&self) -> Task<Message> {
        // Best-effort daemon shutdown
        if let Some(client) = &self.client {
            let client = client.clone();
            return Task::perform(
                async move {
                    let _ = client.request("daemon.shutdown", json!({})).await;
                },
                |()| Message::Quit,
            )
            .chain(iced::exit());
        }
        iced::exit()
    }

    // -- IPC helpers --

    fn fetch_tunnels(&self) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        Task::perform(
            async move { client.tunnel_list().await.unwrap_or_default() },
            Message::TunnelStatusUpdate,
        )
    }

    fn send_action(&self, method: &str, params: serde_json::Value) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let method = method.to_string();
        Task::perform(
            async move {
                let _ = client.request(&method, params).await;
            },
            |()| Message::ActionDone,
        )
    }

    fn send_config_reload(&self) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                let _ = client.request("config.reload", json!({})).await;
            },
            |()| Message::ConfigReloaded,
        )
    }

    fn update_form_field(&mut self, field: FormField, value: String) {
        match field {
            FormField::Name => {
                self.form_state.name = value.clone();
                if !self.form_state.id_manually_edited {
                    self.form_state.id = tunnel_form::slugify(&value);
                }
            }
            FormField::Id => {
                self.form_state.id_manually_edited = true;
                self.form_state.id = value;
            }
            FormField::Host => self.form_state.host = value,
            FormField::SshPort => self.form_state.ssh_port = value,
            FormField::LocalPort => self.form_state.local_port = value,
            FormField::RemoteHost => self.form_state.remote_host = value,
            FormField::RemotePort => self.form_state.remote_port = value,
            FormField::LocalHost => self.form_state.local_host = value,
            FormField::RemoteBind => self.form_state.remote_bind = value,
            FormField::Identity => self.form_state.identity = value,
            FormField::JumpHost => self.form_state.jump_host = value,
            FormField::JumpPort => self.form_state.jump_port = value,
        }
    }

    fn submit_tunnel(&mut self) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let id = self.form_state.id.clone();
        let config_json = tunnel_form::build_config_json(&self.form_state);
        let is_edit = self.form_state.editing_id.is_some();

        if is_edit {
            Task::perform(
                async move {
                    client
                        .tunnel_update(&id, config_json)
                        .await
                        .map(|_| ())
                        .map_err(|e| match e {
                            IpcError::Rpc { message, .. } => message,
                            other => other.to_string(),
                        })
                },
                Message::TunnelUpdated,
            )
        } else {
            Task::perform(
                async move {
                    client
                        .tunnel_add(&id, config_json)
                        .await
                        .map(|_| ())
                        .map_err(|e| match e {
                            IpcError::Rpc { message, .. } => message,
                            other => other.to_string(),
                        })
                },
                Message::TunnelAdded,
            )
        }
    }

    fn send_tunnel_remove(&self, tunnel_id: &str) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let id = tunnel_id.to_string();
        Task::perform(
            async move {
                client
                    .tunnel_remove(&id, true)
                    .await
                    .map(|_| ())
                    .map_err(|e| match e {
                        IpcError::Rpc { message, .. } => message,
                        other => other.to_string(),
                    })
            },
            Message::TunnelRemoved,
        )
    }

    fn rebuild_log_content(&mut self) {
        let text = super::views::logs::build_log_text(&self.logs);
        self.log_content = text_editor::Content::with_text(&text);
        self.log_content
            .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    }

    fn emit_tunnel_notifications(&mut self, new: &[TunnelInfo]) {
        let old: HashMap<&str, &TunnelStatus> = self
            .tunnels
            .iter()
            .map(|t| (t.id.as_str(), &t.status))
            .collect();

        for t in new {
            let prev = old.get(t.id.as_str()).copied();
            match (&t.status, prev) {
                (TunnelStatus::Connected, Some(s)) if *s != TunnelStatus::Connected => {
                    notifications::tunnel_connected(&t.name);
                }
                (TunnelStatus::Error, Some(s)) if *s != TunnelStatus::Error => {
                    let error = t.last_error.as_deref().unwrap_or("unknown error");
                    notifications::tunnel_error(&t.name, error);
                }
                (TunnelStatus::Disconnected, Some(TunnelStatus::Connected)) => {
                    if self.pending_user_disconnect.remove(&t.id) {
                        // User-initiated, no notification.
                    } else {
                        notifications::tunnel_disconnected(&t.name);
                    }
                }
                _ => {}
            }
        }

        let current_ids: HashSet<&str> = new.iter().map(|t| t.id.as_str()).collect();
        self.pending_user_disconnect
            .retain(|id| current_ids.contains(id.as_str()));
    }
}

// -- Log export --

fn export_logs(logs: &[LogEvent]) {
    let log_dir = crate::common::logging::log_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir);

    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let path = log_dir.join(format!("burrow-export-{epoch}.log"));

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut content = String::new();
    for log in logs {
        content.push_str(&format!(
            "{} [{}] [{}] {}\n",
            log.timestamp, log.level, log.target, log.message
        ));
    }

    match std::fs::write(&path, &content) {
        Ok(()) => tracing::info!(path = %path.display(), lines = logs.len(), "logs exported"),
        Err(e) => tracing::error!(error = %e, "failed to export logs"),
    }
}

/// Compare only the fields that affect the tray menu (id, name, status, enabled).
/// Ignores stats/uptime which change every poll and would cause menu rebuilds.
fn tray_relevant_eq(a: &[TunnelInfo], b: &[TunnelInfo]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| {
            x.id == y.id && x.name == y.name && x.status == y.status && x.enabled == y.enabled
        })
}

// -- IPC subscription --

fn ipc_subscription() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(100, |mut output| async move {
        let (client, mut event_rx) = GuiIpcClient::spawn();

        if output.send(Message::IpcReady(client)).await.is_err() {
            return;
        }

        while let Some(event) = event_rx.recv().await {
            let msg = match event {
                DaemonEvent::Connected => Message::DaemonConnected,
                DaemonEvent::Disconnected(r) => Message::DaemonDisconnected(r),
                DaemonEvent::LogLine(ev) => Message::LogReceived(ev),
                DaemonEvent::TunnelsChanged(tunnels) => Message::TunnelStatusUpdate(tunnels),
            };
            if output.send(msg).await.is_err() {
                break;
            }
        }
    })
}

// -- Tray event subscription --

fn tray_event_subscription() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(32, |mut output| async move {
        let rx = tray::menu_event_receiver();

        // Bridge crossbeam blocking recv to async via a dedicated thread.
        // Using std::thread (not spawn_blocking) because this blocks
        // indefinitely and would permanently consume a threadpool slot.
        let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<String>(32);
        std::thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                if async_tx.blocking_send(event.id.as_ref().to_string()).is_err() {
                    break;
                }
            }
        });

        while let Some(id) = async_rx.recv().await {
            if output.send(Message::TrayMenuEvent(id)).await.is_err() {
                break;
            }
        }
    })
}
