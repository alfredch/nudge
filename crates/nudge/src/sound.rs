//! Message sounds, played with rodio on a dedicated thread.
//!
//! The audio device is opened per sound and closed afterwards: nothing
//! keeps the sound server busy between messages, and a changed default
//! device (headphones, Bluetooth) is picked up automatically.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::time::Duration;

use anyhow::{Context, Result};
use nudge_api::Message;
use rodio::{Decoder, DeviceSinkBuilder, Player as RodioPlayer, Source};
use tracing::{debug, warn};

use crate::config;
use crate::notify::{self, Conditions, Urgency};

/// Sounds waiting while one plays; more are dropped.
const QUEUE: usize = 4;
/// Custom sounds can be long; a notification sound is not.
const MAX_LENGTH: Duration = Duration::from_secs(30);

/// Sound names that mean silence.
/// Messages carry short codes (verified: "None (silent)" arrives as "no",
/// "Vibrate Only" as "vi", "Bike" as "bk"); the long names are accepted for
/// the configured default.
const SILENT: [&str; 4] = ["no", "none", "vi", "vibrate"];

/// Name of the sound to play for a message, or `None` for silence.
/// Follows the display decision: no notification or low urgency (priority
/// -1, quiet hours) means no sound.
pub fn sound_for<'a>(
    message: &'a Message,
    config: &'a config::Sound,
    conditions: Conditions,
) -> Option<&'a str> {
    let display = notify::display_for(message, conditions)?;
    if !config.enabled || display.urgency == Urgency::Low {
        return None;
    }
    let name = message
        .sound
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&config.default);
    (!name.is_empty() && !SILENT.contains(&name)).then_some(name)
}

struct Request {
    path: PathBuf,
    volume: f32,
}

#[derive(Clone)]
pub struct Player {
    tx: Option<SyncSender<Request>>,
}

impl Player {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(QUEUE);
        let thread = std::thread::Builder::new()
            .name("sound".to_owned())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    if let Err(e) = play(&request.path, request.volume) {
                        warn!("cannot play {}: {e:#}", request.path.display());
                    }
                }
            });
        match thread {
            Ok(_) => Self { tx: Some(tx) },
            Err(e) => {
                warn!("cannot start sound thread, sounds are disabled: {e}");
                Self { tx: None }
            }
        }
    }

    /// Queues a sound; never blocks.
    pub fn play(&self, path: PathBuf, volume_percent: u8) {
        let Some(tx) = &self.tx else {
            return;
        };
        let request = Request {
            path,
            volume: f32::from(volume_percent.min(100)) / 100.0,
        };
        match tx.try_send(request) {
            Ok(()) => {}
            Err(TrySendError::Full(r)) => {
                debug!("too many sounds queued, skipping {}", r.path.display())
            }
            Err(TrySendError::Disconnected(_)) => warn!("sound thread has stopped"),
        }
    }
}

/// Blocks until the sound has finished.
fn play(path: &Path, volume: f32) -> Result<()> {
    let file = File::open(path).context("cannot open file")?;
    let source = Decoder::try_from(BufReader::new(file))
        .context("cannot decode file")?
        .take_duration(MAX_LENGTH);
    let mut sink = DeviceSinkBuilder::open_default_sink().context("cannot open audio device")?;
    sink.log_on_drop(false);
    let player = RodioPlayer::connect_new(sink.mixer());
    player.set_volume(volume);
    player.append(source);
    player.sleep_until_end();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(json: serde_json::Value) -> Message {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn chooses_message_sound_or_default() {
        let c = config::Sound::default();
        let m = msg(serde_json::json!({"id": 1, "sound": "bike"}));
        assert_eq!(sound_for(&m, &c, Conditions::default()), Some("bike"));
        let m = msg(serde_json::json!({"id": 1}));
        assert_eq!(sound_for(&m, &c, Conditions::default()), Some("pushover"));
        let m = msg(serde_json::json!({"id": 1, "sound": ""}));
        assert_eq!(sound_for(&m, &c, Conditions::default()), Some("pushover"));
    }

    #[test]
    fn silent_cases() {
        let c = config::Sound::default();
        for json in [
            serde_json::json!({"id": 1, "sound": "no"}),
            serde_json::json!({"id": 1, "sound": "none"}),
            serde_json::json!({"id": 1, "sound": "vi"}),
            serde_json::json!({"id": 1, "sound": "vibrate"}),
            serde_json::json!({"id": 1, "priority": -1}),
            serde_json::json!({"id": 1, "priority": -2}),
        ] {
            assert_eq!(sound_for(&msg(json), &c, Conditions::default()), None);
        }
        let off = config::Sound {
            enabled: false,
            ..config::Sound::default()
        };
        assert_eq!(
            sound_for(
                &msg(serde_json::json!({"id": 1})),
                &off,
                Conditions::default()
            ),
            None
        );
        let silent_default = config::Sound {
            default: "none".to_owned(),
            ..config::Sound::default()
        };
        assert_eq!(
            sound_for(
                &msg(serde_json::json!({"id": 1})),
                &silent_default,
                Conditions::default()
            ),
            None
        );
    }

    #[test]
    fn quiet_hours_silence_only_normal_and_low() {
        let c = config::Sound::default();
        let p = |prio: i8| msg(serde_json::json!({"id": 1, "priority": prio}));
        assert_eq!(sound_for(&p(0), &c, Conditions::QUIET), None);
        assert_eq!(sound_for(&p(1), &c, Conditions::QUIET), Some("pushover"));
        assert_eq!(sound_for(&p(2), &c, Conditions::QUIET), Some("pushover"));
    }

    #[test]
    fn pause_silences_all_but_emergencies() {
        let c = config::Sound::default();
        let p = |prio: i8| msg(serde_json::json!({"id": 1, "priority": prio, "receipt": "r"}));
        assert_eq!(sound_for(&p(1), &c, Conditions::PAUSED), None);
        assert_eq!(sound_for(&p(2), &c, Conditions::PAUSED), Some("pushover"));
    }
}
