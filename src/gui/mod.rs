mod app;
mod ipc_client;
mod notifications;
mod style;
mod tray;
mod views;

use app::BurrowApp;

/// Entry point for the GUI. Runs iced daemon (no window on start -- opened
/// on demand via tray menu). Must be called from the main thread.
pub fn launch() {
    // Tray and hide_from_dock happen inside new(), after iced has
    // initialized NSApplication. Creating NSApplication ourselves first
    // (via raw objc_msgSend) conflicts with winit/objc2's initialization.
    iced::daemon(BurrowApp::title, BurrowApp::update, BurrowApp::view)
        .subscription(BurrowApp::subscription)
        .run_with(BurrowApp::new)
        .expect("iced daemon failed");
}

/// Set macOS activation policy to Accessory so the app appears only
/// in the menu bar, not the Dock or Cmd-Tab switcher.
#[cfg(target_os = "macos")]
pub(super) fn hide_from_dock() {
    type Obj = *mut std::ffi::c_void;
    type Sel = *mut std::ffi::c_void;

    unsafe extern "C" {
        fn objc_getClass(name: *const i8) -> Obj;
        fn sel_registerName(name: *const i8) -> Sel;
        fn objc_msgSend();
    }

    type SendNoArgs = unsafe extern "C" fn(Obj, Sel) -> Obj;
    type SendI64 = unsafe extern "C" fn(Obj, Sel, i64) -> Obj;

    unsafe {
        let cls = objc_getClass(c"NSApplication".as_ptr());
        let sel_shared = sel_registerName(c"sharedApplication".as_ptr());
        let sel_policy = sel_registerName(c"setActivationPolicy:".as_ptr());

        let send0: SendNoArgs = std::mem::transmute(objc_msgSend as *const ());
        let send1: SendI64 = std::mem::transmute(objc_msgSend as *const ());

        let app = send0(cls, sel_shared);
        // NSApplicationActivationPolicyAccessory = 1
        send1(app, sel_policy, 1);
    }
}
