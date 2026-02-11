mod app;
mod ipc_client;
mod notifications;
mod style;
mod tray;
mod views;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use app::BurrowApp;

/// Entry point for the GUI. Runs iced daemon (no window on start -- opened
/// on demand via tray menu). Must be called from the main thread.
pub fn launch() {
    ensure_daemon();

    // Tray and hide_from_dock happen inside new(), after iced has
    // initialized NSApplication. Creating NSApplication ourselves first
    // (via raw objc_msgSend) conflicts with winit/objc2's initialization.
    iced::daemon(BurrowApp::title, BurrowApp::update, BurrowApp::view)
        .subscription(BurrowApp::subscription)
        .run_with(BurrowApp::new)
        .expect("iced daemon failed");
}

/// Start the daemon if it isn't already running.
fn ensure_daemon() {
    let socket = crate::daemon::socket_path();
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        return;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "cannot determine executable path, skipping daemon start");
            return;
        }
    };

    match Command::new(&exe)
        .arg("daemon-foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => {
            let pid = child.id();
            let started = wait_for_socket(&socket, Duration::from_secs(3));
            if started {
                tracing::info!(pid, "started daemon");
            } else {
                tracing::warn!(pid, "daemon spawned but socket not ready after 3s");
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn daemon");
        }
    }
}

fn wait_for_socket(socket: &std::path::Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

// -- macOS helpers (raw objc calls to avoid objc2 dep) --

#[cfg(target_os = "macos")]
mod macos {
    type Obj = *mut std::ffi::c_void;
    type Sel = *mut std::ffi::c_void;

    unsafe extern "C" {
        fn objc_getClass(name: *const i8) -> Obj;
        fn sel_registerName(name: *const i8) -> Sel;
        fn objc_msgSend();
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct NSRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    type SendNoArgs = unsafe extern "C" fn(Obj, Sel) -> Obj;
    type SendI64 = unsafe extern "C" fn(Obj, Sel, i64) -> Obj;
    type SendBool = unsafe extern "C" fn(Obj, Sel, bool) -> ();
    type SendNoArgsRect = unsafe extern "C" fn(Obj, Sel) -> NSRect;

    fn shared_app() -> Obj {
        unsafe {
            let cls = objc_getClass(c"NSApplication".as_ptr());
            let sel = sel_registerName(c"sharedApplication".as_ptr());
            let send: SendNoArgs = std::mem::transmute(objc_msgSend as *const ());
            send(cls, sel)
        }
    }

    /// Set activation policy to Accessory (menu bar only, no Dock/Cmd-Tab).
    pub fn hide_from_dock() {
        unsafe {
            let sel = sel_registerName(c"setActivationPolicy:".as_ptr());
            let send: SendI64 = std::mem::transmute(objc_msgSend as *const ());
            // NSApplicationActivationPolicyAccessory = 1
            send(shared_app(), sel, 1);
        }
    }

    /// Bring the app to the foreground.
    pub fn activate_app() {
        unsafe {
            let sel = sel_registerName(c"activateIgnoringOtherApps:".as_ptr());
            let send: SendBool = std::mem::transmute(objc_msgSend as *const ());
            send(shared_app(), sel, true);
        }
    }

    /// Get the main screen dimensions (width, height).
    pub fn main_screen_size() -> (f64, f64) {
        unsafe {
            let cls = objc_getClass(c"NSScreen".as_ptr());
            let sel_main = sel_registerName(c"mainScreen".as_ptr());
            let sel_frame = sel_registerName(c"frame".as_ptr());

            let send_obj: SendNoArgs = std::mem::transmute(objc_msgSend as *const ());
            let send_rect: SendNoArgsRect = std::mem::transmute(objc_msgSend as *const ());

            let screen = send_obj(cls, sel_main);
            if screen.is_null() {
                return (1920.0, 1080.0); // fallback
            }
            let frame = send_rect(screen, sel_frame);
            (frame.width, frame.height)
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) use macos::{activate_app, hide_from_dock, main_screen_size};

#[cfg(not(target_os = "macos"))]
pub(super) fn main_screen_size() -> (f64, f64) {
    (1920.0, 1080.0) // fallback for non-macOS
}
