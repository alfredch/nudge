//! Commands from the tray and the D-Bus interface, and the pause timer.
//!
//! Callers only send commands; the work happens in [`Controller::run`].

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use nudge_api::{Message, MessageId};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::{interval, sleep};
use tracing::{info, warn};

use crate::cache::{self, Kind};
use crate::config::Config;
use crate::export;
use crate::notify::{Conditions, Notifier};
use crate::paths::Paths;
use crate::status::{Pause, StatusRx, StatusTx, unix_now};

pub const OBJECT_PATH: &str = "/io/github/alfredch/Nudge";
pub const INTERFACE: &str = "io.github.alfredch.Nudge1";

/// Checks for expired alerts and pause ends missed during suspend.
const TICK: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum Command {
    /// `None` pauses until resumed.
    Pause(Option<Duration>),
    Resume,
    /// Shows a recent message or alert again, without sound.
    Show(MessageId),
    OpenHistory,
    Quit,
}

/// Cheap handle for sending commands.
#[derive(Clone)]
pub struct Control {
    tx: mpsc::UnboundedSender<Command>,
}

impl Control {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<Command>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    pub fn send(&self, command: Command) {
        // Fails only when the daemon is shutting down.
        let _ = self.tx.send(command);
    }
}

pub struct Controller {
    pub status: StatusTx,
    pub notifier: Notifier,
    pub config: watch::Receiver<Config>,
    pub paths: Paths,
    /// Signalled on `Quit`; the daemon then shuts down.
    pub quit: Arc<Notify>,
}

impl Controller {
    pub async fn run(self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let mut tick = interval(TICK);
        loop {
            let pause_end = match self.status.borrow().pause {
                Pause::Until(end) => Some(end),
                _ => None,
            };
            let pause_timer = async {
                match pause_end {
                    Some(end) => sleep(seconds_until(end)).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.handle(command).await,
                    None => return,
                },
                () = pause_timer => self.end_pause("pause ended").await,
                _ = tick.tick() => {
                    // Timers stand still during suspend; check the wall clock.
                    if pause_end.is_some_and(|end| unix_now() >= end) {
                        self.end_pause("pause ended").await;
                    }
                    self.status.send_if_modified(|s| s.expire_alerts(unix_now()));
                }
            }
        }
    }

    async fn handle(&self, command: Command) {
        match command {
            Command::Pause(duration) => {
                let pause = match duration {
                    Some(d) => {
                        let secs = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
                        Pause::Until(unix_now().saturating_add(secs))
                    }
                    None => Pause::Indefinite,
                };
                info!("notifications paused {}", describe(pause));
                self.status.send_modify(|s| s.pause = pause);
            }
            Command::Resume => {
                if self.status.borrow().paused() {
                    self.end_pause("resumed").await;
                }
            }
            Command::Show(id) => self.show(id).await,
            Command::OpenHistory => {
                if let Err(e) = export::open_history(&self.paths).await {
                    warn!("cannot open history: {e:#}");
                    report(
                        self.notifier
                            .show_info("Cannot open history", &format!("{e:#}"), false)
                            .await,
                    );
                }
            }
            Command::Quit => {
                info!("quit requested");
                self.quit.notify_one();
            }
        }
    }

    /// Ends the pause and summarizes what was held back.
    async fn end_pause(&self, why: &str) {
        let mut held_back = Vec::new();
        self.status.send_modify(|s| {
            s.pause = Pause::Off;
            held_back = std::mem::take(&mut s.held_back);
        });
        info!("{why}, {} message(s) held back", held_back.len());
        if held_back.is_empty() {
            return;
        }
        let quiet = self.config.borrow().quiet_hours.active();
        let refs: Vec<&Message> = held_back.iter().collect();
        report(
            self.notifier
                .show_summary("while paused", &refs, quiet)
                .await,
        );
    }

    async fn show(&self, id: MessageId) {
        let message = {
            let status = self.status.borrow();
            status
                .alerts
                .iter()
                .chain(&status.recent)
                .find(|m| m.id == id)
                .cloned()
        };
        let Some(message) = message else {
            return;
        };
        let icon = message
            .icon
            .as_deref()
            .and_then(|name| cache::cached(&self.paths.cache, Kind::Icon, name));
        report(
            self.notifier
                .show_message(&message, Conditions::default(), icon.as_deref())
                .await,
        );
    }
}

/// "until 14:30" or "until resumed", in local time.
pub fn describe(pause: Pause) -> String {
    match pause {
        Pause::Off => "off".to_owned(),
        Pause::Indefinite => "until resumed".to_owned(),
        Pause::Until(end) => format!("until {}", local_time(end)),
    }
}

/// Local wall-clock time of a Unix timestamp, e.g. "14:30".
pub fn local_time(unix: i64) -> String {
    Timestamp::from_second(unix)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| unix.to_string())
}

/// Local date and time, e.g. "2026-10-30 14:30".
pub fn local_datetime(unix: i64) -> String {
    Timestamp::from_second(unix)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| unix.to_string())
}

fn seconds_until(unix: i64) -> Duration {
    Duration::from_secs(u64::try_from(unix.saturating_sub(unix_now())).unwrap_or(0))
}

fn report(result: anyhow::Result<()>) {
    if let Err(e) = result {
        warn!("cannot show notification: {e:#}");
    }
}

/// `io.github.alfredch.Nudge1` on the daemon's bus name, used by the CLI.
pub struct Interface {
    pub control: Control,
    pub status: StatusRx,
}

#[zbus::interface(name = "io.github.alfredch.Nudge1")]
impl Interface {
    /// Daemon state as JSON (see `status::Report`).
    fn status(&self) -> zbus::fdo::Result<String> {
        serde_json::to_string(&self.status.borrow().report())
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Pauses notifications; 0 seconds means until resumed.
    fn pause(&self, seconds: u64) {
        let duration = (seconds > 0).then(|| Duration::from_secs(seconds));
        self.control.send(Command::Pause(duration));
    }

    fn resume(&self) {
        self.control.send(Command::Resume);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Report, Status};

    #[test]
    fn describes_pause() {
        assert_eq!(describe(Pause::Indefinite), "until resumed");
        assert!(describe(Pause::Until(0)).starts_with("until "));
    }

    /// Serves the interface on a private name and calls it like the CLI.
    #[tokio::test]
    async fn interface_round_trip() {
        let Ok(conn) = zbus::Connection::session().await else {
            eprintln!("no session bus, skipping");
            return;
        };
        let name = format!("io.github.alfredch.Nudge.Test.c{}", std::process::id());
        let (control, mut commands) = Control::channel();
        let (_status_tx, status) = watch::channel(Status::new("dev".into()));
        conn.object_server()
            .at(OBJECT_PATH, Interface { control, status })
            .await
            .unwrap();
        conn.request_name(name.as_str()).await.unwrap();

        let client = zbus::Connection::session().await.unwrap();
        let proxy = zbus::Proxy::new(&client, name.as_str(), OBJECT_PATH, INTERFACE)
            .await
            .unwrap();
        let json: String = proxy.call("Status", &()).await.unwrap();
        let report: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(report.device, "dev");

        let () = proxy.call("Pause", &(600u64,)).await.unwrap();
        assert!(matches!(
            commands.recv().await,
            Some(Command::Pause(Some(d))) if d == Duration::from_secs(600)
        ));
        let () = proxy.call("Pause", &(0u64,)).await.unwrap();
        assert!(matches!(commands.recv().await, Some(Command::Pause(None))));
    }
}
