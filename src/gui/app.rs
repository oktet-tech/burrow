use std::time::Duration;

use iced::futures::SinkExt;
use iced::widget::{container, text};
use iced::{Element, Length, Size, Subscription, Task};
use serde_json::json;

use crate::ipc::protocol::TunnelInfo;

use super::ipc_client::{DaemonEvent, GuiIpcClient, LogEvent};

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

    // User actions
    Connect(String),
    Disconnect(String),
    ConnectAll,
    DisconnectAll,
    RestartAll,
    ReloadConfig,

    // IPC action completed, triggers tunnel list refresh
    ActionDone,
}

// -- Application state --

pub struct BurrowApp {
    tunnels: Vec<TunnelInfo>,
    logs: Vec<LogEvent>,
    daemon_connected: bool,
    client: Option<GuiIpcClient>,
}

impl BurrowApp {
    fn new() -> (Self, Task<Message>) {
        (
            Self {
                tunnels: Vec::new(),
                logs: Vec::new(),
                daemon_connected: false,
                client: None,
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
                self.fetch_tunnels()
            }
            Message::DaemonDisconnected(_) => {
                self.daemon_connected = false;
                self.tunnels.clear();
                Task::none()
            }
            Message::TunnelStatusUpdate(tunnels) => {
                self.tunnels = tunnels;
                Task::none()
            }
            Message::LogReceived(event) => {
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
                self.send_action("tunnel.disconnect", json!({ "id": id }))
            }
            Message::ConnectAll => self.send_action("tunnel.connect_all", json!({})),
            Message::DisconnectAll => self.send_action("tunnel.disconnect_all", json!({})),
            Message::RestartAll => self.send_action("tunnel.restart_all", json!({})),
            Message::ReloadConfig => self.send_action("config.reload", json!({})),
            Message::ActionDone => self.fetch_tunnels(),
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

        super::views::tunnel_list::view(&self.tunnels)
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
