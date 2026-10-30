//! Best-effort detection of resume from suspend and network changes.
//!
//! After either event the existing WebSocket is likely dead, but it can take
//! until the keep-alive timeout to notice. A wake signal lets the daemon
//! reconnect and fetch immediately instead.
//!
//! The logind and NetworkManager watchers are independent: on systems
//! without NetworkManager (systemd-networkd, iwd) resume detection still works.

use std::collections::HashMap;

use futures_util::StreamExt;
use tokio::sync::watch;
use tracing::{debug, info};
use zbus::zvariant::{OwnedValue, Value};

const NM_NAME: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";

/// Receiving side; each event bumps the counter, so bursts coalesce.
pub type WakeReceiver = watch::Receiver<u64>;

/// Starts the watchers on the system bus. Watchers that cannot start are
/// logged and skipped; the receiver then just never fires for them.
pub fn spawn() -> WakeReceiver {
    let (tx, rx) = watch::channel(0u64);
    tokio::spawn(async move {
        let conn = match zbus::Connection::system().await {
            Ok(conn) => conn,
            Err(e) => {
                info!("no system bus, suspend and network detection disabled: {e}");
                return;
            }
        };
        tokio::spawn(run(
            "resume detection (logind)",
            watch_logind(conn.clone(), tx.clone()),
        ));
        tokio::spawn(run(
            "network detection (NetworkManager)",
            watch_network(conn, tx),
        ));
    });
    rx
}

async fn run(what: &'static str, watcher: impl Future<Output = zbus::Result<()>>) {
    match watcher.await {
        Ok(()) => debug!("{what} stopped"),
        Err(e) => info!("{what} unavailable: {e}"),
    }
}

fn wake(tx: &watch::Sender<u64>, reason: &str) {
    info!("{reason}, reconnecting");
    tx.send_modify(|n| *n = n.wrapping_add(1));
}

async fn watch_logind(conn: zbus::Connection, tx: watch::Sender<u64>) -> zbus::Result<()> {
    let logind = zbus::Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let mut signals = logind.receive_signal("PrepareForSleep").await?;
    info!("resume detection active (logind)");
    while let Some(msg) = signals.next().await {
        // `true` before suspend, `false` after resume.
        if let Ok(false) = msg.body().deserialize::<bool>() {
            wake(&tx, "system resumed from suspend");
        }
    }
    Ok(())
}

/// Fires when NetworkManager gets a new primary connection: going online,
/// or switching networks (e.g. Wi-Fi to Ethernet) without going offline.
async fn watch_network(conn: zbus::Connection, tx: watch::Sender<u64>) -> zbus::Result<()> {
    // Fails if NetworkManager is not installed; the caller logs that.
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    if !dbus.name_has_owner(NM_NAME.try_into()?).await? {
        return Err(zbus::Error::Failure("NetworkManager is not running".into()));
    }

    let props =
        zbus::Proxy::new(&conn, NM_NAME, NM_PATH, "org.freedesktop.DBus.Properties").await?;
    let mut signals = props.receive_signal("PropertiesChanged").await?;
    info!("network detection active (NetworkManager)");
    while let Some(msg) = signals.next().await {
        let Ok((interface, changed, _)) =
            msg.body()
                .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
        else {
            continue;
        };
        if interface == NM_NAME
            && let Some(primary) = changed.get("PrimaryConnection")
        {
            match primary_connection(primary) {
                Some(path) => {
                    debug!("primary connection is now {path}");
                    wake(&tx, "network connection changed");
                }
                None => debug!("network disconnected"),
            }
        }
    }
    Ok(())
}

/// Object path of the new primary connection; `None` for "/" (offline).
fn primary_connection(value: &OwnedValue) -> Option<String> {
    match &**value {
        Value::ObjectPath(path) if path.as_str() != "/" => Some(path.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::ObjectPath;

    fn owned(path: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(ObjectPath::try_from(path).unwrap())).unwrap()
    }

    #[test]
    fn primary_connection_paths() {
        assert_eq!(
            primary_connection(&owned("/org/freedesktop/NetworkManager/ActiveConnection/3")),
            Some("/org/freedesktop/NetworkManager/ActiveConnection/3".to_owned())
        );
        assert_eq!(primary_connection(&owned("/")), None);
        assert_eq!(primary_connection(&OwnedValue::from(7u32)), None);
    }
}
