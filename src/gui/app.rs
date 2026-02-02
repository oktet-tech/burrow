use std::collections::{HashMap, HashSet};
use std::time::Duration;

use iced::futures::SinkExt;
use iced::widget::{column, container, text};
use iced::window;
use iced::{Element, Length, Size, Subscription, Task};
use serde_json::json;
use tray_icon::TrayIcon;

use crate::ipc::protocol::{TunnelInfo, TunnelStatus};

use super::ipc_client::{DaemonEvent, GuiIpcClient, LogEvent};
use super::notifications;
use super::tray::{self, AggregateStatus, TUNNEL_ID_PREFIX};

const MAX_LOG_LINES: usize = 1000;
const STATUS_POLL_SECS: u64 = 2;

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
    Tick,

    // Tray menu
    TrayMenuEvent(String),

    // Window lifecycle
    WindowOpened(window::Id),
    WindowCloseRequested(window::Id),
    Quit,

    // User actions -- tunnels
    Connect(String),
    Disconnect(String),
    ConnectAll,
    DisconnectAll,
    RestartAll,
    ReloadConfig,

    // User actions -- logs
    ClearLogs,
    ExportLogs,

    // Action completions
    ActionDone,
    ConfigReloaded,
}

// -- Application state --

pub struct BurrowApp {
    tray: TrayIcon,
    current_tray_status: AggregateStatus,
    window_id: Option<window::Id>,

    tunnels: Vec<TunnelInfo>,
    logs: Vec<LogEvent>,
    daemon_connected: bool,
    client: Option<GuiIpcClient>,
    initial_fetch_done: bool,
    pending_user_disconnect: HashSet<String>,
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
                window_id: None,

                tunnels: Vec::new(),
                logs: Vec::new(),
                daemon_connected: false,
                client: None,
                initial_fetch_done: false,
                pending_user_disconnect: HashSet::new(),
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
                Task::none()
            }
            Message::Tick => {
                if self.daemon_connected {
                    self.fetch_tunnels()
                } else {
                    Task::none()
                }
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
            Message::ClearLogs => {
                self.logs.clear();
                Task::none()
            }
            Message::ExportLogs => {
                export_logs(&self.logs);
                Task::none()
            }
            Message::ActionDone => self.fetch_tunnels(),
            Message::ConfigReloaded => {
                notifications::config_reloaded();
                self.fetch_tunnels()
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

        column![
            super::views::tunnel_list::view(&self.tunnels),
            super::views::logs::view(&self.logs),
        ]
        .height(Length::Fill)
        .into()
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
        self.tray.set_menu(Some(Box::new(tray::build_menu(
            &self.tunnels,
            self.daemon_connected,
        ))));

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

// -- IPC subscription --

fn ipc_subscription() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(100, |mut output| async move {
        let (client, mut event_rx) = GuiIpcClient::spawn();

        if output.send(Message::IpcReady(client)).await.is_err() {
            return;
        }

        let mut poll = tokio::time::interval(Duration::from_secs(STATUS_POLL_SECS));

        loop {
            tokio::select! {
                event = event_rx.recv() => {
                    let msg = match event {
                        Some(DaemonEvent::Connected) => Message::DaemonConnected,
                        Some(DaemonEvent::Disconnected(r)) => {
                            Message::DaemonDisconnected(r)
                        }
                        Some(DaemonEvent::LogLine(ev)) => Message::LogReceived(ev),
                        None => break,
                    };
                    if output.send(msg).await.is_err() {
                        break;
                    }
                }
                _ = poll.tick() => {
                    if output.send(Message::Tick).await.is_err() {
                        break;
                    }
                }
            }
        }
    })
}

// -- Tray event subscription --

fn tray_event_subscription() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(32, |mut output| async move {
        let rx = tray::menu_event_receiver();
        loop {
            // Poll the tray menu event receiver periodically.
            // MenuEvent::receiver() is a crossbeam channel, not async.
            match rx.try_recv() {
                Ok(event) => {
                    let id = event.id.as_ref().to_string();
                    if output.send(Message::TrayMenuEvent(id)).await.is_err() {
                        break;
                    }
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    })
}
