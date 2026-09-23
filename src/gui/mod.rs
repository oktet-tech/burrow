mod app;
mod ipc_client;
mod notifications;
mod style;
mod tray;
mod views;

use std::time::Duration;

use crate::common::daemon_spawn::{spawn_daemon, wait_for_socket};

use app::BurrowApp;

/// Entry point for the GUI. Runs iced daemon (no window on start -- opened
/// on demand via tray menu). Must be called from the main thread.
pub fn launch() {
    ensure_daemon();

    // Tray and the Dock reopen hook are set up inside new(), after iced has
    // initialized NSApplication. Creating NSApplication ourselves first
    // (via raw objc_msgSend) conflicts with winit/objc2's initialization.
    iced::daemon(BurrowApp::new, BurrowApp::update, BurrowApp::view)
        .title(BurrowApp::title)
        .subscription(BurrowApp::subscription)
        .run()
        .expect("iced daemon failed");
}

/// Start the daemon if it isn't already running.
fn ensure_daemon() {
    let socket = crate::daemon::socket_path();
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        return;
    }

    match spawn_daemon() {
        Ok(pid) => {
            if wait_for_socket(&socket, Duration::from_secs(3)) {
                tracing::info!(pid, "started daemon");
            } else {
                tracing::warn!(pid, "daemon spawned but socket not ready after 3s");
            }
        }
        Err(e) => tracing::error!(error = %e, "failed to spawn daemon"),
    }
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
    type SendObj = unsafe extern "C" fn(Obj, Sel, Obj);
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

    /// Open the main window when the Dock icon is clicked with no visible
    /// windows. winit's app delegate lacks `applicationShouldHandleReopen:`,
    /// so we graft it onto the delegate class at runtime.
    pub fn install_dock_reopen_handler() {
        unsafe extern "C" {
            fn object_getClass(obj: Obj) -> Obj;
            fn class_addMethod(cls: Obj, sel: Sel, imp: *const (), types: *const i8) -> i8;
        }

        unsafe extern "C" fn should_handle_reopen(
            _this: Obj,
            _sel: Sel,
            _sender: Obj,
            has_visible_windows: i8,
        ) -> i8 {
            if has_visible_windows != 0 {
                return 1; // let AppKit bring existing windows forward
            }
            super::notifications::request_open_window();
            0
        }

        // BOOL is `bool` ("B") on arm64 and `signed char` ("c") on x86_64.
        #[cfg(target_arch = "aarch64")]
        const REOPEN_TYPES: &std::ffi::CStr = c"B@:@B";
        #[cfg(not(target_arch = "aarch64"))]
        const REOPEN_TYPES: &std::ffi::CStr = c"c@:@c";

        unsafe {
            let send: SendNoArgs = std::mem::transmute(objc_msgSend as *const ());
            let set_obj: SendObj = std::mem::transmute(objc_msgSend as *const ());
            let app = shared_app();
            let sel_delegate = sel_registerName(c"delegate".as_ptr());
            let delegate = send(app, sel_delegate);
            if delegate.is_null() {
                tracing::warn!("no NSApplication delegate, Dock click won't open window");
                return;
            }

            let sel_reopen =
                sel_registerName(c"applicationShouldHandleReopen:hasVisibleWindows:".as_ptr());
            let added = class_addMethod(
                object_getClass(delegate),
                sel_reopen,
                should_handle_reopen as *const (),
                REOPEN_TYPES.as_ptr(),
            );
            if added == 0 {
                tracing::warn!("delegate already handles reopen, Dock hook not installed");
                return;
            }

            // NSApplication caches which optional delegate methods exist at
            // setDelegate: time; re-assign so it notices the new one.
            let sel_set_delegate = sel_registerName(c"setDelegate:".as_ptr());
            set_obj(app, sel_set_delegate, delegate);
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
pub(super) use macos::{activate_app, install_dock_reopen_handler, main_screen_size};

#[cfg(not(target_os = "macos"))]
pub(super) fn main_screen_size() -> (f64, f64) {
    (1920.0, 1080.0) // fallback for non-macOS
}
