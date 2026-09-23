//! Supervises one SSH child process: drains stderr while it runs, reports
//! when forwarding is up, and reports its exit.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Kept for the error message (and port-conflict detection, which needs the
/// "Address already in use" line that precedes ~3 follow-ups). The full
/// stream is logged at debug level.
const STDERR_TAIL_LINES: usize = 5;
/// VERBOSE-level chatter that never explains a failure.
const INFORMATIONAL_PREFIXES: &[&str] = &[
    "Authenticated to ",
    "Authenticated using ",
    "Authentication succeeded",
    "OpenSSH_",
    "Transferred: ",
    "Bytes per second",
    "Server accepts key",
    "Will attempt key",
    "Offering public key",
    "Connection established",
    "Warning: Permanently added",
];
/// A jump-host child can inherit stderr and hold the pipe open after SSH exits.
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// Logged by OpenSSH at VERBOSE level once auth succeeds; forwards are set
/// up right after, and ExitOnForwardFailure exits if they can't be.
const AUTHENTICATED_MARKER: &str = "Authenticated to ";
/// Fallback when the marker never shows (non-OpenSSH client): past
/// ConnectTimeout with BatchMode, a live SSH must have authenticated.
const READY_FALLBACK: Duration = Duration::from_secs(20);
/// ExitOnForwardFailure exits right after auth if a forward can't bind;
/// waiting a moment avoids a Connected blip (and notification) for that case.
const READY_SETTLE: Duration = Duration::from_millis(500);

type Tail = Arc<Mutex<VecDeque<String>>>;

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
        tx: mpsc::UnboundedSender<MonitorEvent>,
    ) -> Self {
        let (cancel, cancel_rx) = oneshot::channel();
        let handle = tokio::spawn(supervise(id, generation, child, cancel_rx, tx));
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
    mut cancel_rx: oneshot::Receiver<()>,
    tx: mpsc::UnboundedSender<MonitorEvent>,
) {
    let (auth_tx, auth_rx) = oneshot::channel();
    let tail: Tail = Arc::default();
    let stderr_task = child
        .stderr
        .take()
        .map(|s| tokio::spawn(collect_stderr(s, id.clone(), auth_tx, Arc::clone(&tail))));
    let ready = async {
        // An Err means the collector ended without the marker; fall back to time.
        if auth_rx.await.is_err() {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(READY_SETTLE).await;
    };
    let ready = tokio::time::timeout(READY_FALLBACK, ready);
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
            _ = &mut ready, if !ready_sent => {
                ready_sent = true;
                let _ = tx.send(MonitorEvent::Ready { id: id.clone(), generation });
            }
        }
    };

    if let Some(task) = stderr_task {
        drain(task).await;
    }
    let stderr = take_tail(&tail);
    let _ = tx.send(MonitorEvent::Exited {
        id,
        generation,
        code: status.ok().and_then(|s| s.code()),
        stderr,
    });
}

/// Give the collector a moment to read the final lines. Whatever it has
/// gathered stays in the shared tail even if this times out.
async fn drain(mut task: JoinHandle<()>) {
    if tokio::time::timeout(STDERR_DRAIN_TIMEOUT, &mut task)
        .await
        .is_err()
    {
        task.abort();
    }
}

fn take_tail(tail: &Tail) -> Option<String> {
    let lines = std::mem::take(&mut *tail.lock().unwrap_or_else(|e| e.into_inner()));
    if lines.is_empty() {
        None
    } else {
        Some(Vec::from(lines).join("\n"))
    }
}

/// Read stderr until EOF, keeping the last meaningful lines in `tail` and
/// signalling `auth_tx` when SSH reports successful authentication. Reading
/// continuously matters: a full pipe would block SSH mid-session.
async fn collect_stderr(stderr: ChildStderr, id: String, auth_tx: oneshot::Sender<()>, tail: Tail) {
    let mut auth_tx = Some(auth_tx);
    let mut reader = BufReader::new(stderr);
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
        if line.contains(AUTHENTICATED_MARKER)
            && let Some(tx) = auth_tx.take()
        {
            let _ = tx.send(());
        }
        if INFORMATIONAL_PREFIXES.iter().any(|p| line.starts_with(p)) {
            continue;
        }
        let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
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

    #[tokio::test]
    async fn reports_exit_with_stderr_tail() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let child = spawn_sh(
            "echo OpenSSH_10.3p1 >&2; echo zero >&2; echo first >&2; echo last >&2; \
             echo 'Transferred: sent 1, received 2 bytes' >&2; exit 3",
        );
        let _m = Monitor::spawn("t".into(), 7, child, tx);

        match rx.recv().await.unwrap() {
            MonitorEvent::Exited {
                generation,
                code,
                stderr,
                ..
            } => {
                assert_eq!(generation, 7);
                assert_eq!(code, Some(3));
                assert_eq!(stderr.as_deref(), Some("zero\nfirst\nlast"));
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
        let _m = Monitor::spawn("t".into(), 1, child, tx);

        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap();
        assert!(matches!(
            event,
            Some(MonitorEvent::Exited { code: Some(1), .. })
        ));
    }

    #[tokio::test]
    async fn reports_ready_on_authenticated_line() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let child = spawn_sh(
            "echo 'Authenticated to bastion ([10.0.0.1]:22) using \"publickey\".' >&2; exec sleep 60",
        );
        let m = Monitor::spawn("t".into(), 1, child, tx);

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
        let m = Monitor::spawn("t".into(), 1, child, tx);

        m.stop().await;
        assert!(!pid_alive(pid));
        assert!(rx.try_recv().is_err());
    }
}
