// On macOS, notify-rust swizzles NSBundle.bundleIdentifier which corrupts
// winit/iced, and osascript intermittently opens Script Editor.
// Use NSUserNotificationCenter via raw objc FFI (same pattern as hide_from_dock).

use std::sync::{Mutex, OnceLock};

// -- Notification click channel --
//
// Platform-specific callbacks send () through this channel when the user
// clicks a notification. The GUI picks it up via take_click_receiver()
// and opens the main window.

static CLICK_CHANNEL: OnceLock<(
    std::sync::mpsc::Sender<()>,
    Mutex<Option<std::sync::mpsc::Receiver<()>>>,
)> = OnceLock::new();

/// Set up notification click handling. Call once during GUI startup.
pub fn init() {
    CLICK_CHANNEL.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        (tx, Mutex::new(Some(rx)))
    });

    #[cfg(target_os = "macos")]
    install_macos_delegate();
}

/// Take the click event receiver (one-shot). Used by the iced subscription.
pub fn take_click_receiver() -> Option<std::sync::mpsc::Receiver<()>> {
    CLICK_CHANNEL.get()?.1.lock().ok()?.take()
}

fn signal_click() {
    if let Some((tx, _)) = CLICK_CHANNEL.get() {
        let _ = tx.send(());
    }
}

// -- macOS: NSUserNotificationCenter delegate via raw objc FFI --

#[cfg(target_os = "macos")]
fn install_macos_delegate() {
    type Obj = *mut std::ffi::c_void;
    type Sel = *mut std::ffi::c_void;

    unsafe extern "C" {
        fn objc_getClass(name: *const i8) -> Obj;
        fn objc_allocateClassPair(superclass: Obj, name: *const i8, extra_bytes: usize) -> Obj;
        fn objc_registerClassPair(cls: Obj);
        fn class_addMethod(cls: Obj, sel: Sel, imp: *const (), types: *const i8) -> i8;
        fn sel_registerName(name: *const i8) -> Sel;
        fn objc_msgSend();
    }

    unsafe extern "C" fn did_activate(
        _this: *mut std::ffi::c_void,
        _sel: *mut std::ffi::c_void,
        _center: *mut std::ffi::c_void,
        _notification: *mut std::ffi::c_void,
    ) {
        signal_click();
    }

    unsafe {
        let nsobject = objc_getClass(c"NSObject".as_ptr());
        let cls = objc_allocateClassPair(nsobject, c"BurrowNotifDelegate".as_ptr(), 0);
        if cls.is_null() {
            return;
        }

        let sel_activate = sel_registerName(
            c"userNotificationCenter:didActivateNotification:".as_ptr(),
        );
        class_addMethod(
            cls,
            sel_activate,
            did_activate as *const (),
            c"v@:@@".as_ptr(),
        );
        objc_registerClassPair(cls);

        // [[BurrowNotifDelegate alloc] init]
        type SendNoArgs = unsafe extern "C" fn(Obj, Sel) -> Obj;
        type SendObj = unsafe extern "C" fn(Obj, Sel, Obj) -> Obj;
        let send0: SendNoArgs = std::mem::transmute(objc_msgSend as *const ());
        let send_obj: SendObj = std::mem::transmute(objc_msgSend as *const ());

        let sel_alloc = sel_registerName(c"alloc".as_ptr());
        let sel_init = sel_registerName(c"init".as_ptr());
        let delegate = send0(send0(cls, sel_alloc), sel_init);
        if delegate.is_null() {
            return;
        }

        // [defaultUserNotificationCenter setDelegate:delegate]
        let cls_center = objc_getClass(c"NSUserNotificationCenter".as_ptr());
        let sel_default = sel_registerName(c"defaultUserNotificationCenter".as_ptr());
        let center = send0(cls_center, sel_default);
        if center.is_null() {
            return;
        }
        let sel_set_delegate = sel_registerName(c"setDelegate:".as_ptr());
        send_obj(center, sel_set_delegate, delegate);
    }
}

// -- macOS send --

#[cfg(target_os = "macos")]
fn send(title: &str, body: &str) {
    let title = title.to_owned();
    let body = body.to_owned();

    std::thread::spawn(move || {
        send_native(&title, &body);
    });
}

#[cfg(target_os = "macos")]
fn send_native(title: &str, body: &str) {
    type Obj = *mut std::ffi::c_void;
    type Sel = *mut std::ffi::c_void;

    unsafe extern "C" {
        fn objc_getClass(name: *const i8) -> Obj;
        fn sel_registerName(name: *const i8) -> Sel;
        fn objc_msgSend();
    }

    type SendNoArgs = unsafe extern "C" fn(Obj, Sel) -> Obj;
    type SendObj = unsafe extern "C" fn(Obj, Sel, Obj) -> Obj;
    type SendPtr = unsafe extern "C" fn(Obj, Sel, *const i8) -> Obj;

    unsafe {
        let send0: SendNoArgs = std::mem::transmute(objc_msgSend as *const ());
        let send_obj: SendObj = std::mem::transmute(objc_msgSend as *const ());
        let send_ptr: SendPtr = std::mem::transmute(objc_msgSend as *const ());

        let cls_str = objc_getClass(c"NSString".as_ptr());
        let sel_utf8 = sel_registerName(c"stringWithUTF8String:".as_ptr());
        let make_nsstring = |s: &str| -> Obj {
            let cstr = std::ffi::CString::new(s).unwrap_or_default();
            send_ptr(cls_str, sel_utf8, cstr.as_ptr())
        };

        // [[NSUserNotification alloc] init]
        let cls_notif = objc_getClass(c"NSUserNotification".as_ptr());
        let sel_alloc = sel_registerName(c"alloc".as_ptr());
        let sel_init = sel_registerName(c"init".as_ptr());
        let notif = send0(send0(cls_notif, sel_alloc), sel_init);
        if notif.is_null() {
            return;
        }

        // setTitle:
        let sel_set_title = sel_registerName(c"setTitle:".as_ptr());
        send_obj(notif, sel_set_title, make_nsstring(title));

        // setInformativeText:
        if !body.is_empty() {
            let sel_set_text = sel_registerName(c"setInformativeText:".as_ptr());
            send_obj(notif, sel_set_text, make_nsstring(body));
        }

        // [[NSUserNotificationCenter defaultUserNotificationCenter] deliverNotification:]
        let cls_center = objc_getClass(c"NSUserNotificationCenter".as_ptr());
        let sel_default = sel_registerName(c"defaultUserNotificationCenter".as_ptr());
        let center = send0(cls_center, sel_default);
        if center.is_null() {
            return;
        }
        let sel_deliver = sel_registerName(c"deliverNotification:".as_ptr());
        send_obj(center, sel_deliver, notif);
    }
}

// -- Linux send (with click-to-open via wait_for_action) --

#[cfg(not(target_os = "macos"))]
fn send(title: &str, body: &str) {
    use notify_rust::Notification;

    let title = title.to_owned();
    let body = body.to_owned();

    std::thread::spawn(move || {
        let mut n = Notification::new();
        n.appname("Burrow")
            .summary(&title)
            .icon("network-server")
            .action("default", "Open");
        if !body.is_empty() {
            n.body(&body);
        }
        match n.show() {
            Ok(handle) => {
                handle.wait_for_action(|action| {
                    if action == "default" {
                        signal_click();
                    }
                });
            }
            Err(e) => {
                tracing::debug!(error = %e, "failed to show desktop notification");
            }
        }
    });
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
