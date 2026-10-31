//! Daemon state shared with the tray and the D-Bus interface.

use std::time::{SystemTime, UNIX_EPOCH};

use nudge_api::{Message, Receipt};
use serde::Serialize;
use tokio::sync::watch;

/// Entries in the "recent messages" menu.
pub const RECENT: usize = 10;
/// Emergencies expire on the server after at most 3 hours. An emergency
/// acknowledged elsewhere (e.g. on the phone) is never reported to nudge,
/// so the tray alert is dropped after this time.
pub const ALERT_LIFETIME: i64 = 3 * 3600;

pub type StatusTx = watch::Sender<Status>;
pub type StatusRx = watch::Receiver<Status>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pause {
    #[default]
    Off,
    /// Until this Unix time.
    Until(i64),
    Indefinite,
}

impl Pause {
    pub fn active(self, now: i64) -> bool {
        match self {
            Pause::Off => false,
            Pause::Until(end) => now < end,
            Pause::Indefinite => true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub device: String,
    /// Unix time of the current connection; `None` while disconnected.
    pub connected_since: Option<i64>,
    pub pause: Pause,
    /// Messages held back during the pause, for the summary at its end.
    pub held_back: Vec<Message>,
    /// Unacknowledged emergencies, one per receipt.
    pub alerts: Vec<Message>,
    /// Newest first.
    pub recent: Vec<Message>,
}

impl Status {
    pub fn new(device: String) -> Self {
        Self {
            device,
            ..Self::default()
        }
    }

    pub fn paused(&self) -> bool {
        self.pause.active(unix_now())
    }

    pub fn add_recent(&mut self, message: &Message) {
        self.recent.retain(|m| m.id != message.id);
        self.recent.insert(0, message.clone());
        self.recent.truncate(RECENT);
    }

    pub fn add_alert(&mut self, message: &Message) {
        if message.receipt.is_none() {
            return;
        }
        self.alerts.retain(|m| m.receipt != message.receipt);
        self.alerts.push(message.clone());
    }

    pub fn remove_alert(&mut self, receipt: &Receipt) {
        self.alerts.retain(|m| m.receipt.as_ref() != Some(receipt));
    }

    /// Drops alerts that must have expired on the server. Returns whether
    /// anything changed.
    pub fn expire_alerts(&mut self, now: i64) -> bool {
        let before = self.alerts.len();
        self.alerts.retain(|m| m.date + ALERT_LIFETIME > now);
        self.alerts.len() != before
    }

    pub fn report(&self) -> Report {
        let brief = |m: &Message| Brief {
            id: m.id.0,
            date: m.date,
            priority: m.priority,
            app: m.app.clone(),
            title: m.display_title().to_owned(),
        };
        let now = unix_now();
        Report {
            version: env!("CARGO_PKG_VERSION"),
            device: self.device.clone(),
            connected_since: self.connected_since,
            paused: self.pause.active(now),
            paused_until: match self.pause {
                Pause::Until(end) if end > now => Some(end),
                _ => None,
            },
            held_back: self.held_back.len(),
            alerts: self.alerts.iter().map(brief).collect(),
            last_message: self.recent.first().map(brief),
        }
    }
}

/// What `nudge status` shows; sent over D-Bus as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Report {
    #[serde(skip_deserializing)]
    pub version: &'static str,
    pub device: String,
    pub connected_since: Option<i64>,
    pub paused: bool,
    pub paused_until: Option<i64>,
    pub held_back: usize,
    pub alerts: Vec<Brief>,
    pub last_message: Option<Brief>,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Brief {
    pub id: u64,
    pub date: i64,
    pub priority: i8,
    pub app: String,
    pub title: String,
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(json: serde_json::Value) -> Message {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn pause_states() {
        assert!(!Pause::Off.active(100));
        assert!(Pause::Until(200).active(100));
        assert!(!Pause::Until(200).active(200));
        assert!(Pause::Indefinite.active(i64::MAX));
    }

    #[test]
    fn recent_is_bounded_and_newest_first() {
        let mut s = Status::default();
        for id in 0..15 {
            s.add_recent(&msg(serde_json::json!({"id": id})));
        }
        s.add_recent(&msg(serde_json::json!({"id": 12})));
        assert_eq!(s.recent.len(), RECENT);
        let ids: Vec<u64> = s.recent.iter().map(|m| m.id.0).collect();
        assert_eq!(ids[..3], [12, 14, 13]);
    }

    #[test]
    fn alerts_by_receipt() {
        let mut s = Status::default();
        let m = |id: u64, date: i64| {
            msg(serde_json::json!({"id": id, "priority": 2, "receipt": "r", "date": date}))
        };
        s.add_alert(&m(1, 1000));
        s.add_alert(&m(1, 1000));
        s.add_alert(&msg(serde_json::json!({"id": 2, "priority": 2})));
        assert_eq!(s.alerts.len(), 1);
        assert!(!s.expire_alerts(1000 + ALERT_LIFETIME - 1));
        assert!(s.expire_alerts(1000 + ALERT_LIFETIME));
        assert!(s.alerts.is_empty());
        s.add_alert(&m(1, 1000));
        s.remove_alert(&Receipt::new("r"));
        assert!(s.alerts.is_empty());
    }

    #[test]
    fn report_round_trips() {
        let mut s = Status::new("dev".into());
        s.connected_since = Some(5);
        s.add_recent(&msg(serde_json::json!({"id": 3, "app": "A", "title": "T"})));
        let report = s.report();
        let json = serde_json::to_string(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.device, "dev");
        assert_eq!(back.last_message.unwrap().title, "T");
        assert!(!back.paused);
    }
}
