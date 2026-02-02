use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::ipc::protocol::LogLine;

// -- LogBroadcast --

struct Inner {
    ring: Mutex<VecDeque<LogLine>>,
    capacity: usize,
    tx: broadcast::Sender<LogLine>,
}

/// Collects log lines into a ring buffer and broadcasts them to subscribers.
/// Thread-safe and cheaply cloneable.
#[derive(Clone)]
pub struct LogBroadcast {
    inner: Arc<Inner>,
}

impl LogBroadcast {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            inner: Arc::new(Inner {
                ring: Mutex::new(VecDeque::with_capacity(capacity)),
                capacity,
                tx,
            }),
        }
    }

    /// Append a log line to the ring buffer and broadcast it.
    pub fn push(&self, line: LogLine) {
        {
            let mut ring = self.inner.ring.lock().expect("ring lock poisoned");
            if ring.len() >= self.inner.capacity {
                ring.pop_front();
            }
            ring.push_back(line.clone());
        }
        // Ignore send error -- just means no active receivers
        let _ = self.inner.tx.send(line);
    }

    /// Get a new broadcast receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.inner.tx.subscribe()
    }

    /// Snapshot the most recent `n` lines from the ring buffer.
    pub fn recent(&self, n: usize) -> Vec<LogLine> {
        let ring = self.inner.ring.lock().expect("ring lock poisoned");
        let skip = ring.len().saturating_sub(n);
        ring.iter().skip(skip).cloned().collect()
    }

    /// Create a tracing layer that feeds log events into this broadcast.
    pub fn layer(&self) -> BroadcastLayer {
        BroadcastLayer {
            broadcast: self.clone(),
        }
    }
}

// -- Tracing Layer --

pub struct BroadcastLayer {
    broadcast: LogBroadcast,
}

impl<S> Layer<S> for BroadcastLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let timestamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f").to_string();
        let level = meta.level().to_string();
        let target = meta.target().to_string();
        let message = visitor.message.unwrap_or_default();

        self.broadcast.push(LogLine {
            timestamp,
            level,
            target,
            message,
        });
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{:?}", value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_recent() {
        let bc = LogBroadcast::new(4);

        for i in 0..6 {
            bc.push(LogLine {
                timestamp: format!("t{i}"),
                level: "INFO".into(),
                target: "test".into(),
                message: format!("msg{i}"),
            });
        }

        // Ring capacity is 4, pushed 6 -> oldest 2 evicted
        let recent = bc.recent(10);
        assert_eq!(recent.len(), 4);
        assert_eq!(recent[0].message, "msg2");
        assert_eq!(recent[3].message, "msg5");
    }

    #[test]
    fn recent_fewer_than_available() {
        let bc = LogBroadcast::new(8);
        for i in 0..5 {
            bc.push(LogLine {
                timestamp: String::new(),
                level: "DEBUG".into(),
                target: "t".into(),
                message: format!("m{i}"),
            });
        }
        let recent = bc.recent(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].message, "m3");
        assert_eq!(recent[1].message, "m4");
    }

    #[tokio::test]
    async fn broadcast_receiver_gets_lines() {
        let bc = LogBroadcast::new(16);
        let mut rx = bc.subscribe();

        bc.push(LogLine {
            timestamp: "t0".into(),
            level: "INFO".into(),
            target: "test".into(),
            message: "hello".into(),
        });

        let line = rx.recv().await.unwrap();
        assert_eq!(line.message, "hello");
    }
}
