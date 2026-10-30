//! User configuration from `~/.config/nudge/config.toml`.
//!
//! A missing file means defaults. The documented example with all
//! defaults is `packaging/config.toml`.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use jiff::civil::Time;
use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub sound: Sound,
    pub quiet_hours: QuietHours,
    pub history: HistoryConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sound {
    pub enabled: bool,
    /// Percent, 0-100.
    #[serde(deserialize_with = "percent")]
    pub volume: u8,
    /// Played when a message names no sound.
    pub default: String,
}

impl Default for Sound {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 100,
            default: "pushover".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuietHours {
    pub enabled: bool,
    #[serde(deserialize_with = "time")]
    pub start: Time,
    #[serde(deserialize_with = "time")]
    pub end: Time,
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start: Time::constant(22, 0, 0, 0),
            end: Time::constant(7, 0, 0, 0),
        }
    }
}

impl QuietHours {
    /// Whether `now` (local wall-clock time) falls into the quiet period.
    /// The period may span midnight; `start == end` means never.
    pub fn contains(&self, now: Time) -> bool {
        if !self.enabled {
            return false;
        }
        if self.start <= self.end {
            self.start <= now && now < self.end
        } else {
            now >= self.start || now < self.end
        }
    }

    /// Whether it is quiet right now, in the local time zone.
    pub fn active(&self) -> bool {
        self.enabled && self.contains(jiff::Zoned::now().time())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    /// 0 keeps messages forever.
    pub keep_days: u32,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self { keep_days: 365 }
    }
}

impl HistoryConfig {
    /// How long messages are kept; `None` means forever.
    pub fn retention(&self) -> Option<Duration> {
        (self.keep_days > 0).then(|| Duration::from_secs(u64::from(self.keep_days) * 86_400))
    }
}

impl Config {
    /// Reads the file; a missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("invalid configuration in {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }
}

fn time<'de, D: Deserializer<'de>>(d: D) -> Result<Time, D::Error> {
    let s = String::deserialize(d)?;
    s.parse()
        .map_err(|_| serde::de::Error::custom(format!("invalid time {s:?}, expected HH:MM")))
}

fn percent<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    let v = u8::deserialize(d)?;
    if v > 100 {
        return Err(serde::de::Error::custom("volume must be between 0 and 100"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: i8, m: i8) -> Time {
        Time::new(h, m, 0, 0).unwrap()
    }

    #[test]
    fn example_file_matches_defaults() {
        let example = include_str!("../../../packaging/config.toml");
        assert_eq!(Config::parse(example).unwrap(), Config::default());
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn partial_file() {
        let c = Config::parse("[quiet_hours]\nenabled = true\nstart = \"23:30\"").unwrap();
        assert!(c.quiet_hours.enabled);
        assert_eq!(c.quiet_hours.start, t(23, 30));
        assert_eq!(c.quiet_hours.end, t(7, 0));
        assert_eq!(c.sound, Sound::default());
    }

    #[test]
    fn rejects_mistakes() {
        assert!(Config::parse("[sound]\nvolum = 50").is_err());
        assert!(Config::parse("[sound]\nvolume = 150").is_err());
        assert!(Config::parse("[quiet_hours]\nstart = \"25:00\"").is_err());
        assert!(Config::parse("[quiet_hours]\nstart = \"10pm\"").is_err());
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn quiet_hours_over_midnight() {
        let q = QuietHours {
            enabled: true,
            ..QuietHours::default()
        };
        assert!(q.contains(t(22, 0)));
        assert!(q.contains(t(23, 59)));
        assert!(q.contains(t(0, 0)));
        assert!(q.contains(t(6, 59)));
        assert!(!q.contains(t(7, 0)));
        assert!(!q.contains(t(12, 0)));
        assert!(!q.contains(t(21, 59)));
    }

    #[test]
    fn quiet_hours_within_a_day() {
        let q = QuietHours {
            enabled: true,
            start: t(12, 0),
            end: t(13, 30),
        };
        assert!(!q.contains(t(11, 59)));
        assert!(q.contains(t(12, 0)));
        assert!(q.contains(t(13, 29)));
        assert!(!q.contains(t(13, 30)));
    }

    #[test]
    fn quiet_hours_edge_cases() {
        let q = QuietHours {
            enabled: true,
            start: t(8, 0),
            end: t(8, 0),
        };
        assert!(!q.contains(t(8, 0)));
        let off = QuietHours::default();
        assert!(!off.contains(t(23, 0)));
    }

    #[test]
    fn retention() {
        assert_eq!(HistoryConfig { keep_days: 0 }.retention(), None);
        assert_eq!(
            HistoryConfig { keep_days: 2 }.retention(),
            Some(Duration::from_secs(172_800))
        );
    }
}
