use notify_rust::Notification;

/// Register a known bundle ID so mac-notification-sys doesn't try to
/// resolve the bogus "use_default" app name via AppleScript, which
/// pops up the macOS application picker dialog.
#[cfg(target_os = "macos")]
pub fn init() {
    let _ = notify_rust::set_application("com.apple.Terminal");
}

#[cfg(not(target_os = "macos"))]
pub fn init() {}

fn send(summary: &str, body: &str) {
    let mut n = Notification::new();
    n.appname("Burrow").summary(summary);
    if !body.is_empty() {
        n.body(body);
    }
    #[cfg(not(target_os = "macos"))]
    n.icon("network-server");

    if let Err(e) = n.show() {
        tracing::debug!(error = %e, "failed to show desktop notification");
    }
}

pub fn tunnel_connected(name: &str) {
    send(&format!("\u{2713} {name} connected"), "");
}

pub fn tunnel_error(name: &str, error: &str) {
    send(&format!("\u{2717} {name} failed"), error);
}

pub fn tunnel_disconnected(name: &str) {
    send(
        &format!("\u{26A0} {name} disconnected"),
        "Retrying\u{2026}",
    );
}

pub fn network_changed() {
    send("Network changed", "Reconnecting tunnels");
}

pub fn config_reloaded() {
    send("Configuration reloaded", "");
}
