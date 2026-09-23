use std::path::PathBuf;
use std::process::Command;

/// Install burrow as a system service that starts on login.
pub fn service_install() {
    let exe = current_exe_or_exit();

    #[cfg(target_os = "macos")]
    macos_install(&exe);

    #[cfg(target_os = "linux")]
    linux_install(&exe);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = exe;
        eprintln!("service install is not supported on this platform");
        std::process::exit(1);
    }
}

/// Uninstall the burrow system service.
pub fn service_uninstall() {
    #[cfg(target_os = "macos")]
    macos_uninstall();

    #[cfg(target_os = "linux")]
    linux_uninstall();

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        eprintln!("service uninstall is not supported on this platform");
        std::process::exit(1);
    }
}

fn current_exe_or_exit() -> PathBuf {
    match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot determine executable path: {e}");
            std::process::exit(1);
        }
    }
}

// -- macOS: launchd --

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    directories::BaseDirs::new()
        .expect("cannot determine home directory")
        .home_dir()
        .join("Library/LaunchAgents/com.burrow.daemon.plist")
}

#[cfg(target_os = "macos")]
fn generate_plist(exe: &std::path::Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.burrow.daemon</string>

    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon-foreground</string>
    </array>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>

    <key>ThrottleInterval</key>
    <integer>30</integer>

    <key>StandardOutPath</key>
    <string>/dev/null</string>

    <key>StandardErrorPath</key>
    <string>/dev/null</string>

    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        exe = exe.display()
    )
}

#[cfg(target_os = "macos")]
fn macos_install(exe: &std::path::Path) {
    let plist = plist_path();

    if plist.exists() {
        eprintln!("Service already installed at: {}", plist.display());
        eprintln!("Run 'burrow service uninstall' first to reinstall.");
        std::process::exit(1);
    }

    if let Some(parent) = plist.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("failed to create {}: {e}", parent.display());
            std::process::exit(1);
        }
    }

    let content = generate_plist(exe);
    if let Err(e) = std::fs::write(&plist, &content) {
        eprintln!("failed to write plist: {e}");
        std::process::exit(1);
    }

    let status = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("Service installed and started.");
            println!("  plist: {}", plist.display());
            println!("  binary: {}", exe.display());
            println!();
            println!("The daemon will start automatically on login.");
        }
        Ok(s) => {
            eprintln!("launchctl load failed (exit {})", s.code().unwrap_or(-1));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("failed to run launchctl: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_uninstall() {
    let plist = plist_path();

    if !plist.exists() {
        eprintln!("Service not installed (no plist at {})", plist.display());
        std::process::exit(1);
    }

    // Unload stops the service if running
    let status = Command::new("launchctl")
        .args(["unload", "-w"])
        .arg(&plist)
        .status();

    if let Ok(s) = status {
        if !s.success() {
            // Non-fatal: service may not be loaded
            eprintln!("warning: launchctl unload exited with {}", s.code().unwrap_or(-1));
        }
    }

    if let Err(e) = std::fs::remove_file(&plist) {
        eprintln!("failed to remove plist: {e}");
        std::process::exit(1);
    }

    println!("Service uninstalled.");
    println!("  removed: {}", plist.display());
}

// -- Linux: systemd user units --

#[cfg(target_os = "linux")]
fn unit_path() -> PathBuf {
    directories::BaseDirs::new()
        .expect("cannot determine home directory")
        .home_dir()
        .join(".config/systemd/user/burrow.service")
}

#[cfg(target_os = "linux")]
fn generate_unit(exe: &std::path::Path) -> String {
    format!(
        "[Unit]\n\
         Description=Burrow SSH Tunnel Manager\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} daemon-foreground\n\
         Restart=on-failure\n\
         RestartSec=30\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display()
    )
}

#[cfg(target_os = "linux")]
fn linux_install(exe: &std::path::Path) {
    let unit = unit_path();

    if unit.exists() {
        eprintln!("Service already installed at: {}", unit.display());
        eprintln!("Run 'burrow service uninstall' first to reinstall.");
        std::process::exit(1);
    }

    if let Some(parent) = unit.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("failed to create {}: {e}", parent.display());
            std::process::exit(1);
        }
    }

    let content = generate_unit(exe);
    if let Err(e) = std::fs::write(&unit, &content) {
        eprintln!("failed to write unit file: {e}");
        std::process::exit(1);
    }

    // Reload systemd to pick up the new unit
    run_systemctl(&["daemon-reload"]);

    // Enable and start
    let status = Command::new("systemctl")
        .args(["--user", "enable", "--now", "burrow.service"])
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("Service installed and started.");
            println!("  unit: {}", unit.display());
            println!("  binary: {}", exe.display());
            println!();
            println!("The daemon will start automatically on login.");
        }
        Ok(s) => {
            eprintln!("systemctl enable failed (exit {})", s.code().unwrap_or(-1));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("failed to run systemctl: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_uninstall() {
    let unit = unit_path();

    if !unit.exists() {
        eprintln!("Service not installed (no unit at {})", unit.display());
        std::process::exit(1);
    }

    // Disable and stop
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", "burrow.service"])
        .status();

    if let Err(e) = std::fs::remove_file(&unit) {
        eprintln!("failed to remove unit file: {e}");
        std::process::exit(1);
    }

    // Reload so systemd forgets the unit
    run_systemctl(&["daemon-reload"]);

    println!("Service uninstalled.");
    println!("  removed: {}", unit.display());
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) {
    let _ = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status();
}
