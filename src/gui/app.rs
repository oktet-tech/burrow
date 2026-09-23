use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use iced::futures::SinkExt;
use iced::widget::{column, container, rule, text, text_editor};
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
const ERROR_NOTIFICATION_COOLDOWN_SECS: u64 = 60;
/// Coalesces log bursts (e.g. the 512-line backlog on connect) into one
/// re-layout of the log editor.
const LOG_FLUSH_INTERVAL: Duration = Duration::from_millis(250);
/// Redraw cadence for a visible retry countdown.
const COUNTDOWN_TICK: Duration = Duration::from_secs(1);
/// Redraw cadence for session uptimes, which show minutes at most.
const UPTIME_TICK: Duration = Duration::from_secs(30);

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
    WindowClosed(window::Id),
    NotificationClicked,
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
    FlushLogs,
    /// Open the log drawer, for one tunnel or (None) for all.
    ShowLogs(Option<String>),
    HideLogs,
    ToggleDisabledSection,
    /// Redraw so countdowns and uptimes stay current.
    Tick,
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
    logs: VecDeque<LogEvent>,
    log_content: text_editor::Content,
    /// Logs arrived since log_content was last rebuilt.
    logs_dirty: bool,
    logs_open: bool,
    /// Tunnel whose log lines the drawer shows; None shows all.
    log_scope: Option<String>,
    show_disabled: bool,
    daemon_connected: bool,
    client: Option<GuiIpcClient>,
    initial_fetch_done: bool,
    pending_user_disconnect: HashSet<String>,

    show_new_tunnel_form: bool,
    form_state: TunnelFormState,
    form_error: Option<String>,

    /// Cooldown for error notifications to avoid flooding user during repeated failures.
    error_notification_cooldown: HashMap<String, Instant>,
}

impl BurrowApp {
    /// Called inside the iced event loop, after NSApplication is initialized.
    pub fn new() -> (Self, Task<Message>) {
        notifications::init();
        #[cfg(target_os = "macos")]
        super::install_dock_reopen_handler();

        let tray = tray::create_tray();

        (
            Self {
                tray,
                current_tray_status: AggregateStatus::NoneConnected,
                last_menu_tunnels: Vec::new(),
                last_menu_daemon_connected: false,
                window_id: None,

                tunnels: Vec::new(),
                logs: VecDeque::new(),
                log_content: text_editor::Content::new(),
                logs_dirty: false,
                logs_open: false,
                log_scope: None,
                show_disabled: false,
                daemon_connected: false,
                client: None,
                initial_fetch_done: false,
                pending_user_disconnect: HashSet::new(),

                show_new_tunnel_form: false,
                form_state: TunnelFormState::default(),
                form_error: None,

                error_notification_cooldown: HashMap::new(),
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
                    self.logs.pop_front();
                }
                self.logs.push_back(event);
                // Rebuilt lazily by FlushLogs, and only while the window is open.
                self.logs_dirty = true;
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
                tracing::debug!(?id, "window close requested");
                if self.window_id == Some(id) {
                    window::close(id)
                } else {
                    Task::none()
                }
            }
            Message::WindowClosed(id) => {
                tracing::debug!(?id, "window closed");
                if self.window_id == Some(id) {
                    self.window_id = None;
                }
                Task::none()
            }
            Message::NotificationClicked => self.open_window(),
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
                // Scoped to one tunnel, clear only its lines
                let scope = self.log_scope.clone();
                self.logs
                    .retain(|e| !super::views::logs::matches_scope(e, scope.as_deref()));
                self.rebuild_log_content();
                Task::none()
            }
            Message::FlushLogs => {
                // Rebuilding drops the selection; wait until the user is done copying.
                if self.log_content.selection().is_none() {
                    self.rebuild_log_content();
                }
                Task::none()
            }
            Message::ExportLogs => {
                export_logs(self.scoped_logs());
                Task::none()
            }
            Message::ShowLogs(scope) => {
                self.logs_open = true;
                self.log_scope = scope;
                self.rebuild_log_content();
                Task::none()
            }
            Message::HideLogs => {
                self.logs_open = false;
                Task::none()
            }
            Message::ToggleDisabledSection => {
                self.show_disabled = !self.show_disabled;
                Task::none()
            }
            Message::Tick => Task::none(),
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
            tracing::trace!(?id, window_id = ?self.window_id, "view called for unknown window");
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
            let enabled = self
                .form_state
                .editing_id
                .as_ref()
                .and_then(|id| self.tunnels.iter().find(|t| &t.id == id))
                .map(|t| t.enabled);
            return container(tunnel_form::view(
                &self.form_state,
                &self.form_error,
                enabled,
            ))
            .style(super::style::window)
            .into();
        }

        let list = container(super::views::tunnel_list::view(
            &self.tunnels,
            self.show_disabled,
        ))
        .height(Length::FillPortion(3));

        let drawer: Element<'_, Message> = if self.logs_open {
            let scope_name = self.log_scope.as_ref().map(|id| {
                self.tunnels
                    .iter()
                    .find(|t| &t.id == id)
                    .map_or(id.as_str(), |t| t.name.as_str())
            });
            container(super::views::logs::panel(
                &self.log_content,
                self.scoped_logs().next().is_some(),
                scope_name,
            ))
            .height(Length::FillPortion(2))
            .into()
        } else {
            super::views::logs::collapsed_bar(self.logs.back())
        };

        container(column![list, rule::horizontal(1), drawer].height(Length::Fill))
            .style(super::style::window)
            .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        // Timer exists only while there is something to flush, so an idle
        // or hidden app gets no periodic wakeups.
        let visible = self.window_id.is_some();
        let log_flush = if self.logs_dirty && visible && self.logs_open {
            iced::time::every(LOG_FLUSH_INTERVAL).map(|_| Message::FlushLogs)
        } else {
            Subscription::none()
        };

        // A past retry time with no update from the daemon must not keep a
        // 1s timer alive; allow a few seconds for the retry to report back.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let countdown = self
            .tunnels
            .iter()
            .any(|t| t.next_retry_at.is_some_and(|at| at + 5 > now));
        let tick = if !visible {
            Subscription::none()
        } else if countdown {
            iced::time::every(COUNTDOWN_TICK).map(|_| Message::Tick)
        } else if self.tunnels.iter().any(|t| t.status == TunnelStatus::Connected) {
            iced::time::every(UPTIME_TICK).map(|_| Message::Tick)
        } else {
            Subscription::none()
        };

        Subscription::batch([
            Subscription::run(ipc_subscription),
            Subscription::run(tray_event_subscription),
            Subscription::run(notification_click_subscription),
            window::close_requests().map(Message::WindowCloseRequested),
            window::close_events().map(Message::WindowClosed),
            log_flush,
            tick,
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
        tracing::debug!(%id, window_id = ?self.window_id, "tray event");
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

    /// Show the main window: focus the existing one in place, or open a new
    /// one if it was closed.
    fn open_window(&mut self) -> Task<Message> {
        #[cfg(target_os = "macos")]
        super::activate_app();

        if self.logs_dirty {
            self.rebuild_log_content();
        }

        if let Some(id) = self.window_id {
            // Reuse it: recreating would reset the user's position and size.
            return window::minimize(id, false).chain(window::gain_focus(id));
        }

        let (_, screen_height) = super::main_screen_size();
        let win_height = (screen_height * 0.75) as f32;
        let (id, open) = window::open(window::Settings {
            size: Size::new(800.0, win_height),
            ..Default::default()
        });
        self.window_id = Some(id);
        tracing::debug!(?id, "opening window");

        open.discard().chain(window::gain_focus(id))
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

    /// Buffered log lines within the drawer's current scope.
    fn scoped_logs(&self) -> impl Iterator<Item = &LogEvent> {
        let scope = self.log_scope.as_deref();
        self.logs
            .iter()
            .filter(move |e| super::views::logs::matches_scope(e, scope))
    }

    fn rebuild_log_content(&mut self) {
        self.logs_dirty = false;
        let text = super::views::logs::build_log_text(self.scoped_logs());
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
        let now = Instant::now();
        let cooldown = std::time::Duration::from_secs(ERROR_NOTIFICATION_COOLDOWN_SECS);

        for t in new {
            let prev = old.get(t.id.as_str()).copied();
            match (&t.status, prev) {
                (TunnelStatus::Connected, Some(s)) if *s != TunnelStatus::Connected => {
                    notifications::tunnel_connected(&t.name);
                }
                (TunnelStatus::Error, Some(s)) if *s != TunnelStatus::Error => {
                    // Rate-limit error notifications to avoid flooding during repeated failures
                    let should_notify = self
                        .error_notification_cooldown
                        .get(&t.id)
                        .map(|last| now.duration_since(*last) >= cooldown)
                        .unwrap_or(true);

                    if should_notify {
                        let error = t.last_error.as_deref().unwrap_or("unknown error");
                        notifications::tunnel_error(&t.name, error);
                        self.error_notification_cooldown.insert(t.id.clone(), now);
                    }
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

        // Clean up cooldown entries for removed tunnels
        let current_ids: HashSet<&str> = new.iter().map(|t| t.id.as_str()).collect();
        self.pending_user_disconnect
            .retain(|id| current_ids.contains(id.as_str()));
        self.error_notification_cooldown
            .retain(|id, _| current_ids.contains(id.as_str()));
    }
}

// -- Log export --

fn export_logs<'a>(logs: impl Iterator<Item = &'a LogEvent>) {
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
    let mut lines = 0;
    for log in logs {
        lines += 1;
        content.push_str(&format!(
            "{} [{}] [{}] {}\n",
            log.timestamp, log.level, log.target, log.message
        ));
    }

    match std::fs::write(&path, &content) {
        Ok(()) => tracing::info!(path = %path.display(), lines, "logs exported"),
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
    iced::stream::channel(100, async |mut output| {
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

// -- Notification click subscription --

fn notification_click_subscription() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(8, async |mut output| {
        let Some(rx) = notifications::take_click_receiver() else {
            // Already consumed or init() not called; park forever.
            std::future::pending::<()>().await;
            return;
        };

        let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<()>(8);
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                if async_tx.blocking_send(()).is_err() {
                    break;
                }
            }
        });

        while async_rx.recv().await.is_some() {
            if output.send(Message::NotificationClicked).await.is_err() {
                break;
            }
        }
    })
}

// -- Tray event subscription --

fn tray_event_subscription() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(32, async |mut output| {
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
