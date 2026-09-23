//! Supervises one SSH child process: drains stderr while it runs, reports
//! when forwarding is up, and reports its exit.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Enough stderr to explain a failure without keeping a chatty session's history.
const STDERR_TAIL_LINES: usize = 20;
/// A jump-host child can inherit stderr and hold the pipe open after SSH exits.
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const READY_POLL_MAX: Duration = Duration::from_secs(1);

pub(super) enum MonitorEvent {
    Ready {
        id: String,
        generation: u64,
    },
    Exited {
        id: String,
        generation: u64,
        code: Option<i32>,
        stderr: Option<String>,
    },
}

impl MonitorEvent {
    pub fn id(&self) -> &str {
        match self {
            Self::Ready { id, .. } | Self::Exited { id, .. } => id,
        }
    }

    pub fn generation(&self) -> u64 {
        match self {
            Self::Ready { generation, .. } | Self::Exited { generation, .. } => *generation,
        }
    }
}

/// How to tell that the SSH session is forwarding traffic.
pub(super) enum Readiness {
    /// SSH has bound this port on 127.0.0.1 (local and SOCKS forwards).
    LocalPort(u16),
    /// No local signal exists (reverse forwards); assume ready once SSH has
    /// survived this long, since ExitOnForwardFailure kills it otherwise.
    After(Duration),
}

/// Handle to a running monitor. Dropping it also kills SSH.
pub(super) struct Monitor {
    cancel: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

impl Monitor {
    pub fn spawn(
        id: String,
        generation: u64,
        child: Child,
        readiness: Readiness,
        tx: mpsc::UnboundedSender<MonitorEvent>,
    ) -> Self {
        let (cancel, cancel_rx) = oneshot::channel();
        let handle = tokio::spawn(supervise(id, generation, child, readiness, cancel_rx, tx));
        Self { cancel, handle }
    }

    /// Kill SSH and wait until it is reaped, so its port is free on return.
    /// No exit event is sent for a stopped process.
    pub async fn stop(self) {
        let _ = self.cancel.send(());
        let _ = self.handle.await;
    }
}

async fn supervise(
    id: String,
    generation: u64,
    mut child: Child,
    readiness: Readiness,
    mut cancel_rx: oneshot::Receiver<()>,
    tx: mpsc::UnboundedSender<MonitorEvent>,
) {
    let stderr_task = child
        .stderr
        .take()
        .map(|s| tokio::spawn(collect_stderr_tail(s, id.clone())));
    let ready = wait_ready(readiness);
    tokio::pin!(ready);
    let mut ready_sent = false;

    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            // Also fires when the Monitor is dropped without stop().
            _ = &mut cancel_rx => {
                let _ = child.kill().await;
                if let Some(task) = stderr_task {
                    task.abort();
                }
                return;
            }
            () = &mut ready, if !ready_sent => {
                ready_sent = true;
                let _ = tx.send(MonitorEvent::Ready { id: id.clone(), generation });
            }
        }
    };

    let stderr = match stderr_task {
        Some(task) => drain(task).await,
        None => None,
    };
    let _ = tx.send(MonitorEvent::Exited {
        id,
        generation,
        code: status.ok().and_then(|s| s.code()),
        stderr,
    });
}

async fn drain(mut task: JoinHandle<Option<String>>) -> Option<String> {
    match tokio::time::timeout(STDERR_DRAIN_TIMEOUT, &mut task).await {
        Ok(Ok(tail)) => tail,
        _ => {
            task.abort();
            None
        }
    }
}

/// Read stderr until EOF, keeping the last lines. Reading continuously
/// matters: a full pipe would block SSH mid-session.
async fn collect_stderr_tail(stderr: ChildStderr, id: String) -> Option<String> {
    let mut reader = BufReader::new(stderr);
    let mut tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_LINES);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf).trim().to_string();
        if line.is_empty() {
            continue;
        }
        tracing::debug!(tunnel_id = %id, "ssh: {line}");
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    if tail.is_empty() {
        None
    } else {
        Some(Vec::from(tail).join("\n"))
    }
}

async fn wait_ready(readiness: Readiness) {
    match readiness {
        Readiness::After(delay) => tokio::time::sleep(delay).await,
        Readiness::LocalPort(port) => {
            let mut delay = Duration::from_millis(100);
            while !port_is_bound(port) {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(READY_POLL_MAX);
            }
        }
    }
}

/// True once something listens on 127.0.0.1:port. Probes by binding rather
/// than connecting so no forwarded connection is opened to the remote side.
fn port_is_bound(port: u16) -> bool {
    matches!(
        std::net::TcpListener::bind(("127.0.0.1", port)),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    fn spawn_sh(script: &str) -> Child {
        tokio::process::Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    fn pid_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn port_probe_detects_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port_is_bound(port));
        drop(listener);
    }

    #[tokio::test]
    async fn reports_exit_with_stderr_tail() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let child = spawn_sh("echo first >&2; echo last >&2; exit 3");
        let _m = Monitor::spawn(
            "t".into(),
            7,
            child,
            Readiness::After(Duration::from_secs(60)),
            tx,
        );

        match rx.recv().await.unwrap() {
            MonitorEvent::Exited {
                generation,
                code,
                stderr,
                ..
            } => {
                assert_eq!(generation, 7);
                assert_eq!(code, Some(3));
                assert_eq!(stderr.as_deref(), Some("first\nlast"));
            }
            MonitorEvent::Ready { .. } => panic!("unexpected ready"),
        }
    }

    #[tokio::test]
    async fn drains_stderr_while_running() {
        // 200KB of stderr exceeds the pipe buffer; without continuous
        // draining the child blocks and never exits.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let child = spawn_sh(
            "i=0; while [ $i -lt 4000 ]; do echo 'channel 3: open failed: connect failed: xxxxxxxxxxxx' >&2; i=$((i+1)); done; exit 1",
        );
        let _m = Monitor::spawn(
            "t".into(),
            1,
            child,
            Readiness::After(Duration::from_secs(60)),
            tx,
        );

        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap();
        assert!(matches!(
            event,
            Some(MonitorEvent::Exited { code: Some(1), .. })
        ));
    }

    #[tokio::test]
    async fn reports_ready_when_port_bound() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let m = Monitor::spawn(
            "t".into(),
            1,
            spawn_sh("exec sleep 60"),
            Readiness::LocalPort(port),
            tx,
        );

        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap();
        assert!(matches!(event, Some(MonitorEvent::Ready { .. })));
        m.stop().await;
    }

    #[tokio::test]
    async fn stop_kills_and_reaps_without_exit_event() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let child = spawn_sh("exec sleep 60");
        let pid = child.id().unwrap();
        let m = Monitor::spawn(
            "t".into(),
            1,
            child,
            Readiness::After(Duration::from_secs(60)),
            tx,
        );

        m.stop().await;
        assert!(!pid_alive(pid));
        assert!(rx.try_recv().is_err());
    }
}
