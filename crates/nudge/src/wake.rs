//! Best-effort detection of resume from suspend and network changes.
//!
//! After either event the existing WebSocket is likely dead, but it can take
//! until the keep-alive timeout to notice. A wake signal lets the daemon
//! reconnect and fetch immediately instead.

use futures_util::StreamExt;
use tokio::sync::watch;
use tracing::{debug, info};

/// NetworkManager `NM_STATE_CONNECTED_GLOBAL`: full internet access.
const NM_CONNECTED_GLOBAL: u32 = 70;

/// Receiving side; each event bumps the counter, so bursts coalesce.
pub type WakeReceiver = watch::Receiver<u64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeEvent {
    Resumed,
    NetworkUp,
}

/// Starts the watchers on the system bus. Without a system bus, or without
/// logind or NetworkManager, the receiver simply never fires.
pub fn spawn() -> WakeReceiver {
    let (tx, rx) = watch::channel(0u64);
    tokio::spawn(async move {
        if let Err(e) = watch_system_bus(tx).await {
            debug!("suspend/network detection unavailable: {e}");
        }
    });
    rx
}

async fn watch_system_bus(tx: watch::Sender<u64>) -> zbus::Result<()> {
    let conn = zbus::Connection::system().await?;

    let logind = zbus::Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let mut sleep = logind.receive_signal("PrepareForSleep").await?;

    // NetworkManager is optional; logind alone still covers resume.
    let nm = zbus::Proxy::new(
        &conn,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )
    .await?;
    let mut network = nm.receive_signal("StateChanged").await?;
    debug!("watching logind and NetworkManager for wake events");

    loop {
        let event = tokio::select! {
            Some(msg) = sleep.next() => {
                // `true` before suspend, `false` after resume.
                match msg.body().deserialize::<bool>() {
                    Ok(false) => WakeEvent::Resumed,
                    _ => continue,
                }
            }
            Some(msg) = network.next() => {
                match msg.body().deserialize::<u32>() {
                    Ok(state) if state == NM_CONNECTED_GLOBAL => WakeEvent::NetworkUp,
                    Ok(state) => {
                        debug!("NetworkManager state {state}");
                        continue;
                    }
                    Err(_) => continue,
                }
            }
            else => return Ok(()),
        };
        info!("{}", describe(event));
        tx.send_modify(|n| *n = n.wrapping_add(1));
    }
}

fn describe(event: WakeEvent) -> &'static str {
    match event {
        WakeEvent::Resumed => "system resumed from suspend, reconnecting",
        WakeEvent::NetworkUp => "network connected, reconnecting",
    }
}
