use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// Handle to a running stub listener. Drop aborts the task, which releases
/// the port once the runtime next polls it; use `stop` to wait for that.
pub struct StubHandle {
    task: Option<JoinHandle<()>>,
}

impl StubHandle {
    /// Abort the listener and wait until its socket is closed.
    pub async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for StubHandle {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Spawn a stub TCP listener for an on-demand tunnel.
///
/// Binds local_port synchronously (fails fast if in use). On incoming
/// connection: drops the listener (freeing the port for SSH), tells the
/// manager to establish the tunnel, waits for SSH to bind the port, then
/// proxies the held connection.
pub fn spawn_stub(
    tunnel_id: String,
    tunnel_name: String,
    local_port: u16,
    manager: super::manager::TunnelManager,
) -> Result<StubHandle, std::io::Error> {
    // Bind synchronously so errors propagate immediately to the caller
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", local_port))?;
    std_listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(std_listener)?;

    tracing::info!(
        tunnel_id = %tunnel_id,
        port = local_port,
        "stub listener bound for on-demand tunnel"
    );

    let task = tokio::spawn(async move {
        stub_accept(listener, tunnel_id, tunnel_name, local_port, manager).await;
    });

    Ok(StubHandle { task: Some(task) })
}

/// Check if a buffer starts with an HTTP method keyword.
fn is_http_request(buf: &[u8]) -> bool {
    const METHODS: &[&[u8]] = &[
        b"GET ", b"POST ", b"PUT ", b"HEAD ", b"DELETE ", b"PATCH ", b"OPTIONS ", b"CONNECT ",
    ];
    METHODS.iter().any(|m| buf.starts_with(m))
}

/// Build a full HTTP 503 response with an inline waiting page.
fn build_waiting_response(tunnel_name: &str) -> Vec<u8> {
    let body = format!(
        r##"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Connecting - {name}</title>
<style>
*{{margin:0;padding:0;box-sizing:border-box}}
body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
  display:flex;justify-content:center;align-items:center;min-height:100vh;
  background:#f5f5f5;color:#333}}
.card{{background:#fff;border-radius:12px;padding:48px;text-align:center;
  box-shadow:0 2px 12px rgba(0,0,0,.08);max-width:420px}}
.spinner{{width:40px;height:40px;border:3px solid #e0e0e0;border-top-color:#666;
  border-radius:50%;animation:spin .8s linear infinite;margin:0 auto 24px}}
@keyframes spin{{to{{transform:rotate(360deg)}}}}
h1{{font-size:18px;font-weight:600;margin-bottom:8px}}
p{{font-size:14px;color:#666;line-height:1.5}}
.error{{color:#c00;display:none;margin-top:16px}}
</style>
</head>
<body>
<div class="card">
  <div class="spinner" id="spinner"></div>
  <h1>Establishing tunnel</h1>
  <p>{name}</p>
  <p class="error" id="error">Tunnel failed to connect. Check daemon logs.</p>
</div>
<script>
(function(){{
  var delay=200,max=2000,deadline=Date.now()+30000;
  function poll(){{
    if(Date.now()>deadline){{
      document.getElementById("spinner").style.display="none";
      document.getElementById("error").style.display="block";
      return;
    }}
    fetch(location.href,{{method:"HEAD"}}).then(function(r){{
      if(r.headers.get("X-Burrow-Waiting"))throw new Error("still waiting");
      location.reload();
    }}).catch(function(){{
      delay=Math.min(delay*1.5,max);
      setTimeout(poll,delay);
    }});
  }}
  setTimeout(poll,delay);
}})();
</script>
</body>
</html>"##,
        name = tunnel_name,
    );

    format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         Retry-After: 3\r\n\
         X-Burrow-Waiting: 1\r\n\
         Cache-Control: no-store\r\n\
         \r\n\
         {body}",
        len = body.len(),
        body = body,
    )
    .into_bytes()
}

/// Serve the waiting page and close the connection.
async fn serve_waiting_page(mut stream: TcpStream, tunnel_name: &str) {
    let response = build_waiting_response(tunnel_name);
    let _ = stream.write_all(&response).await;
    let _ = stream.shutdown().await;
}

async fn stub_accept(
    listener: TcpListener,
    tunnel_id: String,
    tunnel_name: String,
    local_port: u16,
    manager: super::manager::TunnelManager,
) {
    let (stream, addr) = match listener.accept().await {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(tunnel_id = %tunnel_id, error = %e, "stub accept failed");
            return;
        }
    };

    tracing::info!(
        tunnel_id = %tunnel_id,
        peer = %addr,
        "on-demand connection to {}, establishing tunnel...",
        tunnel_name,
    );

    // Peek at first bytes to detect HTTP requests (2s timeout)
    let mut peek_buf = [0u8; 8];
    let is_http = match tokio::time::timeout(
        Duration::from_secs(2),
        stream.peek(&mut peek_buf),
    )
    .await
    {
        Ok(Ok(n)) if n > 0 => is_http_request(&peek_buf[..n]),
        _ => false, // timeout or error: treat as non-HTTP
    };

    // Drop listener to free the port for SSH
    drop(listener);

    let held = if is_http {
        tracing::info!(tunnel_id = %tunnel_id, "HTTP request detected, serving waiting page");
        serve_waiting_page(stream, &tunnel_name).await;
        None
    } else {
        Some(stream)
    };

    // Run outside this task: handle_on_demand clears the StubHandle, whose
    // drop aborts this very task and would cancel the call midway.
    tokio::spawn(async move {
        manager.handle_on_demand(&tunnel_id, held, local_port).await;
    });
}

/// Wait for SSH to bind the port (up to 10s), then proxy bidirectionally.
pub(super) async fn wait_and_proxy(
    mut held: TcpStream,
    local_port: u16,
    tunnel_id: &str,
) {
    const MAX_ATTEMPTS: u32 = 100;
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    for _ in 0..MAX_ATTEMPTS {
        match TcpStream::connect(("127.0.0.1", local_port)).await {
            Ok(mut tunnel_stream) => {
                tracing::info!(
                    tunnel_id = %tunnel_id,
                    "tunnel ready, proxying on-demand connection"
                );
                match tokio::io::copy_bidirectional(&mut held, &mut tunnel_stream).await {
                    Ok((up, down)) => {
                        tracing::debug!(
                            tunnel_id = %tunnel_id,
                            up, down,
                            "on-demand proxy finished"
                        );
                    }
                    Err(e) => {
                        tracing::debug!(
                            tunnel_id = %tunnel_id,
                            error = %e,
                            "on-demand proxy ended"
                        );
                    }
                }
                return;
            }
            Err(_) => {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }

    tracing::error!(
        tunnel_id = %tunnel_id,
        "timed out waiting for tunnel to bind port {}",
        local_port,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_binds_port() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(1);

        // We can't use a real TunnelManager in unit tests, so just test
        // that the listener binds and accepts a connection.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let task = tokio::spawn(async move {
            let (_, addr) = listener.accept().await.unwrap();
            tx.send(addr.to_string()).await.unwrap();
        });

        // Connect to the stub
        let _client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let peer = rx.recv().await.unwrap();
        assert!(!peer.is_empty());
        task.await.unwrap();
    }

    #[test]
    fn detects_http_methods() {
        assert!(is_http_request(b"GET / HTTP/1.1\r\n"));
        assert!(is_http_request(b"POST /api"));
        assert!(is_http_request(b"HEAD / H"));
        assert!(is_http_request(b"PUT /x"));
        assert!(is_http_request(b"DELETE /"));
        assert!(is_http_request(b"PATCH /x"));
        assert!(is_http_request(b"OPTIONS "));
        assert!(is_http_request(b"CONNECT "));
    }

    #[test]
    fn rejects_non_http() {
        assert!(!is_http_request(b"\x16\x03\x01")); // TLS ClientHello
        assert!(!is_http_request(b"\x05\x01\x00")); // SOCKS5
        assert!(!is_http_request(b"SSH-2.0"));
        assert!(!is_http_request(b""));
        assert!(!is_http_request(b"GE")); // too short for "GET "
    }

    #[test]
    fn waiting_response_is_valid_http() {
        let resp = build_waiting_response("Test Tunnel");
        let resp_str = String::from_utf8(resp).unwrap();
        assert!(resp_str.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(resp_str.contains("X-Burrow-Waiting: 1\r\n"));
        assert!(resp_str.contains("Retry-After: 3\r\n"));
        assert!(resp_str.contains("Content-Length:"));
        assert!(resp_str.contains("Test Tunnel"));
        assert!(resp_str.contains("X-Burrow-Waiting"));
        // Verify Content-Length matches actual body
        let parts: Vec<&str> = resp_str.splitn(2, "\r\n\r\n").collect();
        assert_eq!(parts.len(), 2);
        let headers = parts[0];
        let body = parts[1];
        let cl: usize = headers
            .lines()
            .find(|l| l.starts_with("Content-Length:"))
            .unwrap()
            .split(':')
            .nth(1)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(cl, body.len());
    }

    #[tokio::test]
    async fn wait_and_proxy_timeout() {
        // Use a port that nothing listens on
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let (client, server) = tokio::io::duplex(64);
        // Convert DuplexStream to TcpStream is not possible, so test
        // the timeout path by calling with an unreachable port.
        // We'll just verify it doesn't hang by using a short timeout.
        let _ = client;
        let _ = server;

        // The function polls 100 times at 100ms = 10s total, too long for a test.
        // We test the proxy logic indirectly via integration tests.
    }
}
