use std::time::Duration;

use super::manager::TunnelManager;

const DEBOUNCE_DELAY: Duration = Duration::from_secs(2);

/// Start monitoring for network interface changes.
///
/// On macOS: uses SCNetworkReachability callback on a dedicated CFRunLoop thread.
/// On Linux: stub that logs "not implemented".
///
/// On network change, waits for stabilization then reconnects errored tunnels.
pub fn spawn_network_monitor(mgr: TunnelManager) {
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        macos::spawn_reachability_thread(tx);
        spawn_debounced_handler(mgr, rx);
        tracing::info!("network change detection enabled (macOS SCNetworkReachability)");
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = mgr;
        tracing::info!("network change detection not implemented on this platform");
    }
}

/// Receive raw network-change signals, debounce, then reconnect.
fn spawn_debounced_handler(
    mgr: TunnelManager,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Wait for network to stabilize (coalesce rapid events)
            tokio::time::sleep(DEBOUNCE_DELAY).await;
            while rx.try_recv().is_ok() {}

            tracing::info!("network change detected, reconnecting tunnels...");
            mgr.reconnect_errored().await;
        }
    });
}

// -- macOS implementation using SCNetworkReachability --

#[cfg(target_os = "macos")]
mod macos {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

    use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
    use system_configuration::network_reachability::SCNetworkReachability;

    /// Spawn a dedicated OS thread running a CFRunLoop that listens for
    /// reachability changes. Sends a unit signal through `tx` on every change.
    pub fn spawn_reachability_thread(tx: tokio::sync::mpsc::UnboundedSender<()>) {
        std::thread::Builder::new()
            .name("network-monitor".into())
            .spawn(move || {
                run_reachability_loop(tx);
            })
            .expect("failed to spawn network monitor thread");
    }

    fn run_reachability_loop(tx: tokio::sync::mpsc::UnboundedSender<()>) {
        // Monitor general internet reachability (0.0.0.0 = any route)
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
        let mut reachability = SCNetworkReachability::from(addr);

        if let Err(e) = reachability.set_callback(move |_flags| {
            let _ = tx.send(());
        }) {
            tracing::error!(error = ?e, "failed to set reachability callback");
            return;
        }

        let run_loop = CFRunLoop::get_current();
        // Safety: kCFRunLoopCommonModes is a valid, non-null CFStringRef
        // provided by Core Foundation.
        unsafe {
            if let Err(e) = reachability.schedule_with_runloop(&run_loop, kCFRunLoopCommonModes) {
                tracing::error!(error = ?e, "failed to schedule reachability on run loop");
                return;
            }
        }

        tracing::debug!("CFRunLoop running for network reachability");
        CFRunLoop::run_current();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_delay_is_reasonable() {
        assert_eq!(DEBOUNCE_DELAY, Duration::from_secs(2));
    }
}
