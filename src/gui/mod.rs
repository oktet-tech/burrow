mod app;
mod ipc_client;
mod style;
mod tray;
mod views;

/// Entry point for the GUI. Hides from Dock, shows tray icon.
/// Must be called from the main thread.
pub fn launch() {
    #[cfg(target_os = "macos")]
    hide_from_dock();

    tray::run();
}

/// Set macOS activation policy to Accessory so the app appears only
/// in the menu bar, not the Dock or Cmd-Tab switcher.
#[cfg(target_os = "macos")]
fn hide_from_dock() {
    type Obj = *mut std::ffi::c_void;
    type Sel = *mut std::ffi::c_void;

    unsafe extern "C" {
        fn objc_getClass(name: *const i8) -> Obj;
        fn sel_registerName(name: *const i8) -> Sel;
        fn objc_msgSend();
    }

    // objc_msgSend has a variadic ABI; cast to the exact signature needed.
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
