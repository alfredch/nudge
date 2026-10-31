//! Single-instance guard via a well-known name on the session bus.

use anyhow::{Context, Result};
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};

/// Holds the bus name for as long as it lives.
pub struct Instance {
    _conn: zbus::Connection,
}

impl Instance {
    /// The connection owning the name; the daemon serves its interface here.
    pub fn connection(&self) -> &zbus::Connection {
        &self._conn
    }
}

/// Claims `name`. `None` if another process already owns it.
pub async fn acquire(name: &str) -> Result<Option<Instance>> {
    let conn = zbus::Connection::session()
        .await
        .context("cannot connect to the D-Bus session bus")?;
    let dbus = DBusProxy::new(&conn).await?;
    let name = zbus::names::WellKnownName::try_from(name)?;
    let reply = dbus
        .request_name(name.clone(), RequestNameFlags::DoNotQueue.into())
        .await
        .with_context(|| format!("cannot request bus name {name}"))?;
    match reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {
            Ok(Some(Instance { _conn: conn }))
        }
        RequestNameReply::Exists | RequestNameReply::InQueue => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a session bus; skipped where there is none (e.g. plain CI).
    #[tokio::test]
    async fn second_instance_is_rejected() {
        if zbus::Connection::session().await.is_err() {
            eprintln!("no session bus, skipping");
            return;
        }
        let name = format!("io.github.alfredch.Nudge.Test.p{}", std::process::id());
        let first = acquire(&name).await.unwrap();
        assert!(first.is_some());
        assert!(acquire(&name).await.unwrap().is_none());
        drop(first);
        // Released with the connection; give the bus a moment.
        let mut again = None;
        for _ in 0..20 {
            if let Some(i) = acquire(&name).await.unwrap() {
                again = Some(i);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(again.is_some(), "name was not released");
    }
}
