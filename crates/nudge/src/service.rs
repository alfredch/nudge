//! `install-service` and `uninstall-service`: the systemd user unit.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths::APP_ID;

const UNIT: &str = "nudge.service";

fn unit_path() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new().context("cannot determine the home directory")?;
    Ok(dirs.config_dir().join("systemd/user").join(UNIT))
}

/// The unit file. It starts with the graphical session, so the display,
/// the notification server and the tray host are there.
fn unit(exe: &Path) -> String {
    format!(
        "\
# Written by `nudge install-service`; remove with `nudge uninstall-service`.
[Unit]
Description=Nudge, unofficial Pushover Open Client
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart={} daemon
Restart=on-failure
RestartSec=10
# 69: another daemon is running, 78: login needed. Restarting won't help.
RestartPreventExitStatus=69 78

[Install]
WantedBy=graphical-session.target
",
        quote(&exe.to_string_lossy())
    )
}

/// Quotes a path for `ExecStart=` (see systemd.syntax(7) and the `%`
/// specifiers in systemd.unit(5)).
fn quote(path: &str) -> String {
    let escaped = path
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    if escaped.contains(char::is_whitespace) {
        format!("\"{escaped}\"")
    } else {
        escaped
    }
}

pub async fn install() -> Result<()> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("cannot determine the path of the nudge binary")?;
    if exe.components().any(|c| c.as_os_str() == "target") {
        eprintln!(
            "Note: {} looks like a development build. The service runs exactly this \
             file; install a release build for everyday use.",
            exe.display()
        );
    }

    // A unit started with `systemd-run` shadows the file; stop it first.
    if property("Transient").as_deref() == Some("yes") {
        println!("Stopping the temporary {UNIT} started with systemd-run.");
        systemctl(&["stop", UNIT])?;
    }
    // A failed transient unit stays loaded and would still shadow the file.
    let _ = Command::new("systemctl")
        .args(["--user", "reset-failed", UNIT])
        .stderr(std::process::Stdio::null())
        .status();
    if property("ActiveState").as_deref() != Some("active") && daemon_running().await? {
        bail!(
            "a nudge daemon is already running outside systemd. Stop it first (Ctrl-C in its \
             terminal, or `pkill -x nudge`), then run this command again."
        );
    }

    let path = unit_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    std::fs::write(&path, unit(&exe))
        .with_context(|| format!("cannot write {}", path.display()))?;
    println!("Wrote {}", path.display());

    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    // Picks up a changed ExecStart if it was already running.
    systemctl(&["restart", UNIT])?;
    println!("nudge now starts with your desktop session and is running.");
    println!("Logs: journalctl --user -u nudge -f");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = unit_path()?;
    if !path.exists() {
        println!("{} is not installed.", path.display());
        return Ok(());
    }
    systemctl(&["disable", "--now", UNIT])?;
    std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
    systemctl(&["daemon-reload"])?;
    println!("Stopped nudge and removed {}", path.display());
    Ok(())
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("cannot run systemctl")?;
    if !status.success() {
        bail!("`systemctl --user {}` failed ({status})", args.join(" "));
    }
    Ok(())
}

/// A property of the unit, e.g. `Transient` → "yes".
fn property(name: &str) -> Option<String> {
    let out = Command::new("systemctl")
        .args(["--user", "show", UNIT, "--value", "-p", name])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

async fn daemon_running() -> Result<bool> {
    let conn = zbus::Connection::session()
        .await
        .context("cannot connect to the D-Bus session bus")?;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    Ok(dbus.name_has_owner(APP_ID.try_into()?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_file() {
        let u = unit(Path::new("/usr/bin/nudge"));
        assert!(u.contains("ExecStart=/usr/bin/nudge daemon\n"));
        assert!(u.contains("RestartPreventExitStatus=69 78"));
        assert!(u.contains("WantedBy=graphical-session.target"));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("/opt/nudge"), "/opt/nudge");
        assert_eq!(quote("/home/a b/nudge"), "\"/home/a b/nudge\"");
        assert_eq!(quote("/x/100%/nudge"), "/x/100%%/nudge");
    }
}
