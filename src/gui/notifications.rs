use notify_rust::Notification;

fn send(summary: &str, body: &str) {
    let mut n = Notification::new();
    n.appname("Burrow").summary(summary);
    if !body.is_empty() {
        n.body(body);
    }
    // On Linux, set a freedesktop icon hint (ignored on macOS).
    // On macOS, Notification Center uses the sending application's bundle icon.
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
