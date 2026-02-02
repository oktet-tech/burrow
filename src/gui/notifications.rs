// On macOS, notify-rust uses mac-notification-sys which swizzles
// NSBundle.bundleIdentifier globally. This corrupts winit/iced's view
// of the process identity and causes crashes when opening windows.
// Use osascript instead -- no swizzle, no FFI conflicts.

#[cfg(target_os = "macos")]
fn send(title: &str, body: &str) {
    use std::process::Command;

    // AppleScript: display notification "body" with title "title"
    let script = if body.is_empty() {
        format!("display notification \"\" with title \"{}\"", escape(title))
    } else {
        format!(
            "display notification \"{}\" with title \"{}\"",
            escape(body),
            escape(title),
        )
    };

    std::thread::spawn(move || {
        let _ = Command::new("osascript").arg("-e").arg(&script).output();
    });
}

#[cfg(target_os = "macos")]
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(not(target_os = "macos"))]
fn send(title: &str, body: &str) {
    use notify_rust::Notification;

    let mut n = Notification::new();
    n.appname("Burrow").summary(title).icon("network-server");
    if !body.is_empty() {
        n.body(body);
    }
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
