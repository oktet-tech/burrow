use std::io::Write;
use std::time::Duration;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

const ICON_SIZE: u32 = 22;

/// Build the system tray icon and enter the main event loop.
/// Must be called from the main thread (macOS requirement).
pub fn run() {
    let open_item = MenuItem::with_id("open-window", "Open Window", true, None);
    let quit_item = MenuItem::with_id("quit", "Quit Burrow", true, None);

    let menu = Menu::with_items(&[
        &open_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ])
    .expect("failed to create tray menu");

    let icon = create_icon();

    // _tray must stay alive -- dropping it removes the icon from the menu bar.
    let _tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Burrow - SSH Tunnel Manager")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true)
        .with_icon_as_template(true) // macOS: adapts to light/dark menu bar
        .build()
        .expect("failed to create tray icon");

    let menu_rx = MenuEvent::receiver();

    loop {
        // macOS: process pending events so the tray menu works.
        #[cfg(target_os = "macos")]
        pump_macos_events();

        #[cfg(not(target_os = "macos"))]
        std::thread::sleep(Duration::from_millis(50));

        if let Ok(event) = menu_rx.try_recv() {
            if event.id == "open-window" {
                println!("Open Window (not yet implemented)");
            } else if event.id == "quit" {
                shutdown_and_exit();
            }
        }
    }
}

/// Generate a small filled circle as the tray icon.
/// Black-on-transparent so macOS template rendering adapts to dark/light mode.
fn create_icon() -> Icon {
    let mut rgba = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
    let center = ICON_SIZE as f32 / 2.0;
    let radius = 5.0f32;

    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x as f32 - center + 0.5;
            let dy = y as f32 - center + 0.5;
            if dx * dx + dy * dy <= radius * radius {
                let i = ((y * ICON_SIZE + x) * 4) as usize;
                // Black pixel, fully opaque (template icon)
                rgba[i + 3] = 255;
            }
        }
    }

    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).expect("failed to create icon")
}

/// Run the CoreFoundation run loop briefly so macOS delivers tray/menu events.
#[cfg(target_os = "macos")]
fn pump_macos_events() {
    use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
    CFRunLoop::run_in_mode(
        unsafe { kCFRunLoopDefaultMode },
        Duration::from_millis(50),
        false,
    );
}

/// Best-effort daemon shutdown, then exit.
fn shutdown_and_exit() -> ! {
    let socket = crate::daemon::socket_path();
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket) {
        let req =
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"daemon.shutdown\",\"params\":{}}\n";
        let _ = stream.write_all(req.as_bytes());
    }
    std::process::exit(0);
}
