use std::collections::{HashMap, HashSet};
use std::time::Duration;

use iced::futures::SinkExt;
use iced::widget::{column, container, text};
use iced::{Element, Length, Size, Subscription, Task};
use serde_json::json;

use crate::ipc::protocol::{TunnelInfo, TunnelStatus};

use super::ipc_client::{DaemonEvent, GuiIpcClient, LogEvent};
use super::notifications;

const MAX_LOG_LINES: usize = 1000;
const STATUS_POLL_SECS: u64 = 2;

// -- Messages --

#[derive(Debug, Clone)]
pub enum Message {
    // IPC subscription events
    IpcReady(GuiIpcClient),
    DaemonConnected,
    DaemonDisconnected(String),
    TunnelStatusUpdate(Vec<TunnelInfo>),
    LogReceived(LogEvent),
    Tick,

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
    tunnels: Vec<TunnelInfo>,
    logs: Vec<LogEvent>,
    daemon_connected: bool,
    client: Option<GuiIpcClient>,
    /// Skip notifications on the first tunnel poll after connecting to daemon.
    initial_fetch_done: bool,
    /// Tunnel IDs the user explicitly disconnected; suppresses the "unexpected
    /// disconnect" notification for those tunnels.
    pending_user_disconnect: HashSet<String>,
}

impl BurrowApp {
    fn new() -> (Self, Task<Message>) {
        (
            Self {
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

    fn title(&self) -> String {
        "Burrow".into()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::IpcReady(client) => {
                self.client = Some(client);
                Task::none()
            }
            Message::DaemonConnected => {
                self.daemon_connected = true;
                self.initial_fetch_done = false;
                self.pending_user_disconnect.clear();
                self.fetch_tunnels()
            }
            Message::DaemonDisconnected(_) => {
                self.daemon_connected = false;
                self.initial_fetch_done = false;
                self.tunnels.clear();
                Task::none()
            }
            Message::TunnelStatusUpdate(tunnels) => {
                if self.initial_fetch_done {
                    self.emit_tunnel_notifications(&tunnels);
                } else {
                    self.initial_fetch_done = true;
                }
                self.tunnels = tunnels;
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
            Message::ReloadConfig => {
                self.send_config_reload()
            }
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

    fn view(&self) -> Element<'_, Message> {
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

    fn subscription(&self) -> Subscription<Message> {
        Subscription::run(ipc_subscription)
    }

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

    /// Compare old and new tunnel lists, fire desktop notifications for
    /// status transitions.
    fn emit_tunnel_notifications(&mut self, new: &[TunnelInfo]) {
        let old: HashMap<&str, &TunnelStatus> = self
            .tunnels
            .iter()
            .map(|t| (t.id.as_str(), &t.status))
            .collect();

        for t in new {
            let prev = old.get(t.id.as_str()).copied();
            match (&t.status, prev) {
                // Newly connected (was anything other than Connected before).
                (TunnelStatus::Connected, Some(s)) if *s != TunnelStatus::Connected => {
                    notifications::tunnel_connected(&t.name);
                }
                // Transitioned to Error.
                (TunnelStatus::Error, Some(s)) if *s != TunnelStatus::Error => {
                    let error = t.last_error.as_deref().unwrap_or("unknown error");
                    notifications::tunnel_error(&t.name, error);
                }
                // Was Connected, now Disconnected -- unexpected unless user did it.
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

        // Clean up stale entries from pending set (tunnels that no longer exist).
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

/// Long-lived subscription that maintains the daemon connection, streams
/// events (connect/disconnect, log lines), and sends periodic ticks for
/// tunnel status polling.
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

// -- Launch --

/// Launch the iced application window.
///
/// Currently runs as a standalone window. When tray-iced integration is
/// wired up, this will switch to daemon mode (no default window) with
/// windows opened on demand via the tray "Open Window" action.
pub fn run() -> iced::Result {
    iced::application(BurrowApp::title, BurrowApp::update, BurrowApp::view)
        .subscription(BurrowApp::subscription)
        .window_size(Size::new(800.0, 600.0))
        .run_with(BurrowApp::new)
}
