//! The long-running receive loop.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;

use nudge_api::{Client, DEFAULT_PUSH_URL, Frame, Message, MessageId, PushStream, Receipt};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, trace, warn};

use crate::actions::{self, Clipboard};
use crate::backoff::Backoff;
use crate::cache::{self, Cache};
use crate::config::Config;
use crate::credentials::{self, Backend, Credentials};
use crate::history::History;
use crate::instance;
use crate::notify::{self, Notifier};
use crate::paths::{APP_ID, Paths};
use crate::sound;
use crate::wake::{self, WakeReceiver};

/// No keep-alive within this time means the connection is dead.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(90);
/// Pause before reconnecting when the server asked for it.
const RELOAD_DELAY: Duration = Duration::from_secs(1);
/// Closing a probably dead connection must not block reconnecting.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
/// A connection that lasted this long resets the backoff.
const STABLE_SESSION: Duration = Duration::from_secs(60);
/// How often old messages are removed from the history.
const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// Why the daemon stopped.
#[derive(Debug)]
pub enum Exit {
    /// SIGTERM or Ctrl-C.
    Shutdown,
    /// The session is no longer usable; the user has to act.
    NeedsLogin(String),
    /// Another daemon owns the bus name.
    AlreadyRunning,
}

/// How fetched messages are announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// After (re)connecting: summarize, show only emergencies one by one.
    Backlog,
    /// After a `!` frame: show every message.
    Live,
}

enum SessionEnd {
    Reload,
    /// Resume or network change: the connection is probably stale.
    Woken,
    Fatal(String),
}

struct Daemon {
    client: Client,
    user_agent: String,
    credentials: Credentials,
    history: History,
    notifier: Notifier,
    cache: Cache,
    sound: sound::Player,
    config: watch::Receiver<Config>,
    backoff: Backoff,
    wake: WakeReceiver,
    last_prune: Option<Instant>,
}

pub async fn run(paths: &Paths, user_agent: String) -> Result<Exit> {
    let Some(_instance) = instance::acquire(APP_ID).await? else {
        return Ok(Exit::AlreadyRunning);
    };
    let config_path = paths.config_file();
    let config = Config::load(&config_path)?;
    let Some((credentials, backend)) = credentials::load(paths).await? else {
        return Ok(Exit::NeedsLogin(
            "Not logged in. Run `nudge login` first.".to_owned(),
        ));
    };
    let history_path = paths.history_db();
    info!(
        "nudge {} starting: device {}, credentials in {}, history in {}",
        env!("CARGO_PKG_VERSION"),
        credentials.device_id,
        match backend {
            Backend::Keyring => "Secret Service".to_owned(),
            Backend::File => paths.credentials_file().display().to_string(),
        },
        history_path.display()
    );

    let clipboard = Clipboard::detect();
    match &clipboard {
        Some(c) => info!("copying to the clipboard with {}", c.program().display()),
        None => info!("no wl-copy or xclip found, copying is disabled"),
    }
    let (notifier, clicks) = Notifier::connect(clipboard.is_some()).await?;
    let client = Client::new(&user_agent)?;
    let handler = actions::Handler {
        client: client.clone(),
        secret: credentials.secret.clone(),
        notifier: notifier.clone(),
        history: History::open(&history_path)?,
        clipboard,
    };
    tokio::spawn(handler.run(clicks));

    let (config_tx, config_rx) = watch::channel(config);
    let mut sighup = signal(SignalKind::hangup())?;
    tokio::spawn(async move {
        while sighup.recv().await.is_some() {
            reload(&config_path, &config_tx);
        }
    });

    let mut daemon = Daemon {
        cache: Cache::new(paths.cache.clone(), client.clone()),
        sound: sound::Player::spawn(),
        client,
        user_agent,
        credentials,
        history: History::open(&history_path)?,
        notifier: notifier.clone(),
        config: config_rx,
        backoff: Backoff::default(),
        wake: wake::spawn(),
        last_prune: None,
    };

    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        exit = daemon.run_loop() => {
            if let Exit::NeedsLogin(reason) = &exit {
                error!("{reason}");
                report(notifier.show_info("Nudge stopped", reason, true).await);
            }
            Ok(exit)
        }
        _ = tokio::signal::ctrl_c() => Ok(Exit::Shutdown),
        _ = sigterm.recv() => Ok(Exit::Shutdown),
    }
}

impl Daemon {
    async fn run_loop(&mut self) -> Exit {
        loop {
            // Events from before this attempt are covered by the fetch after connecting.
            self.wake.borrow_and_update();
            let started = Instant::now();
            let end = self.session().await;
            if started.elapsed() >= STABLE_SESSION {
                self.backoff.reset();
            }
            match end {
                Ok(SessionEnd::Reload) => {
                    info!("server requested reconnect");
                    sleep(RELOAD_DELAY).await;
                }
                Ok(SessionEnd::Woken) => self.backoff.reset(),
                Ok(SessionEnd::Fatal(reason)) => return Exit::NeedsLogin(reason),
                Err(e) if is_session_rejected(&e) => {
                    return Exit::NeedsLogin(format!(
                        "The session was rejected ({e:#}). Run `nudge login` again."
                    ));
                }
                Err(e) => {
                    let delay = self.backoff.next_delay();
                    warn!(
                        "connection lost: {e:#}; reconnecting in {}s",
                        delay.as_secs()
                    );
                    wait(&mut self.wake, &mut self.backoff, delay).await;
                }
            }
        }
    }

    /// One WebSocket connection, from login to disconnect.
    async fn session(&mut self) -> Result<SessionEnd> {
        let mut stream = PushStream::connect(
            DEFAULT_PUSH_URL,
            &self.user_agent,
            &self.credentials.device_id,
            &self.credentials.secret,
        )
        .await
        .context("cannot connect to the push server")?;
        info!("connected to push server, waiting for messages");

        // Connect first, then fetch: anything arriving after this point
        // triggers a `!`, so nothing slips through between the two steps.
        self.sync(Mode::Backlog).await?;

        loop {
            let frame = tokio::select! {
                frame = timeout(KEEPALIVE_TIMEOUT, stream.next_frame()) => frame
                    .map_err(|_| anyhow!("no keep-alive for {}s", KEEPALIVE_TIMEOUT.as_secs()))??,
                Ok(()) = self.wake.changed() => {
                    close(stream).await;
                    return Ok(SessionEnd::Woken);
                }
            };
            match frame {
                None => return Err(anyhow!("server closed the connection")),
                Some(Frame::Keepalive) => trace!("keep-alive"),
                Some(Frame::NewMessages) => self.sync(Mode::Live).await?,
                Some(Frame::Reload) => {
                    close(stream).await;
                    return Ok(SessionEnd::Reload);
                }
                Some(Frame::Error) => {
                    return Ok(SessionEnd::Fatal(
                        "The push server rejected this device. Run `nudge login` again or \
                         re-enable the device on pushover.net."
                            .to_owned(),
                    ));
                }
                Some(Frame::LoggedInElsewhere) => {
                    return Ok(SessionEnd::Fatal(
                        "This device was logged in from another session. Run `nudge login` \
                         to use it here again."
                            .to_owned(),
                    ));
                }
                Some(Frame::Unknown(byte)) => debug!("ignoring unknown frame {byte:#04x}"),
            }
        }
    }

    /// Fetch → persist → notify → delete on server.
    async fn sync(&mut self, mode: Mode) -> Result<()> {
        self.prune_history();
        let device = &self.credentials.device_id;
        let fetched = self
            .client
            .fetch_messages(&self.credentials.secret, device)
            .await?;
        if fetched.is_empty() {
            match mode {
                Mode::Backlog => info!("no pending messages"),
                Mode::Live => debug!("no pending messages"),
            }
            return Ok(());
        }
        for m in &fetched.malformed {
            warn!(id = ?m.id, "cannot decode message: {}", m.error);
        }

        let inserted = tokio::task::block_in_place(|| self.history.store(device, &fetched))
            .context("cannot store messages in history")?;

        let total = fetched.messages.len() + fetched.malformed.len();
        let new_count = inserted.messages.len() + inserted.malformed;
        match mode {
            Mode::Backlog => info!("fetched {total} pending message(s), {new_count} new"),
            Mode::Live => debug!("fetched {total} message(s), {new_count} new"),
        }

        // The server repeats an unacknowledged emergency by delivering the
        // same message (same id) again, so known emergencies are shown again.
        let (mut new, known): (Vec<&Message>, Vec<&Message>) = fetched
            .messages
            .iter()
            .partition(|m| inserted.messages.contains(&m.id));
        let (repeated, skipped): (Vec<&Message>, Vec<&Message>) = known
            .into_iter()
            .partition(|m| m.requires_persistent_display());
        for m in &repeated {
            info!(
                "message {} repeated by the server, not yet acknowledged",
                m.id
            );
        }
        let skipped = skipped.len() + fetched.malformed.len() - inserted.malformed;
        if skipped > 0 {
            info!("{skipped} message(s) already in history, not shown again");
        }
        new.extend(repeated);
        // While offline, an emergency may have been repeated many times:
        // show only its latest copy.
        let latest = latest_per_receipt(&new);
        new.retain(|m| match &m.receipt {
            Some(r) if latest.get(r) != Some(&m.id) => {
                log_message(m, "superseded by a later retry");
                false
            }
            _ => true,
        });
        // Repeats of an emergency the user acknowledged here, or is acknowledging.
        tokio::task::block_in_place(|| {
            new.retain(|m| match &m.receipt {
                Some(r)
                    if m.priority >= 2
                        && (self.notifier.is_acknowledging(r)
                            || self.history.is_acked(r).unwrap_or(false)) =>
                {
                    log_message(m, "already acknowledged");
                    false
                }
                _ => true,
            });
        });
        self.announce(&new, mode).await;
        if inserted.malformed > 0 {
            report(
                self.notifier
                    .show_info(
                        "Unreadable message",
                        "Nudge received a message it could not read. It was saved to the history.",
                        false,
                    )
                    .await,
            );
        }

        // Delete last: if it fails, the next fetch returns the same entries,
        // the history skips them and nothing is announced twice or lost.
        if let Some(highest) = fetched.highest_id() {
            self.client
                .delete_up_to(
                    &self.credentials.secret,
                    &self.credentials.device_id,
                    highest,
                )
                .await?;
            debug!("deleted messages up to id {highest} on the server");
        }
        Ok(())
    }

    /// At most once a day; failures only cost disk space.
    fn prune_history(&mut self) {
        if self
            .last_prune
            .is_some_and(|t| t.elapsed() < PRUNE_INTERVAL)
        {
            return;
        }
        self.last_prune = Some(Instant::now());
        let Some(keep) = self.config.borrow().history.retention() else {
            return;
        };
        match tokio::task::block_in_place(|| self.history.prune(keep)) {
            Ok(0) => {}
            Ok(n) => info!("removed {n} old message(s) from the history"),
            Err(e) => warn!("cannot prune history: {e:#}"),
        }
    }

    async fn announce(&mut self, messages: &[&Message], mode: Mode) {
        let quiet = self.config.borrow().quiet_hours.active();
        let (hidden, visible): (Vec<&Message>, Vec<&Message>) = messages
            .iter()
            .copied()
            .partition(|m| notify::display_for(m, quiet).is_none());
        for m in hidden {
            log_message(m, "history only");
        }

        if mode == Mode::Live || visible.len() <= 1 {
            for m in visible {
                self.show(m, quiet).await;
            }
            return;
        }

        let (persistent, rest): (Vec<&Message>, Vec<&Message>) = visible
            .into_iter()
            .partition(|m| m.requires_persistent_display());
        for m in persistent {
            self.show(m, quiet).await;
        }
        match rest.as_slice() {
            [] => {}
            [single] => self.show(single, quiet).await,
            _ => {
                for m in &rest {
                    log_message(m, "summarized");
                }
                report(self.notifier.show_summary(&rest, quiet).await);
            }
        }
    }

    async fn show(&mut self, message: &Message, quiet: bool) {
        let action = match notify::display_for(message, quiet) {
            Some(d) if d.persistent => "shown, persistent",
            Some(d) if quiet && d.urgency == notify::Urgency::Low && message.priority == 0 => {
                "shown quietly"
            }
            _ => "shown",
        };
        log_message(message, action);

        let icon = match message.icon.as_deref() {
            Some(name) => self.cache.get(cache::Kind::Icon, name).await,
            None => None,
        };
        report(
            self.notifier
                .show_message(message, quiet, icon.as_deref())
                .await,
        );

        let sound = self.config.borrow().sound.clone();
        if let Some(name) = sound::sound_for(message, &sound, quiet) {
            let mut path = self.cache.get(cache::Kind::Sound, name).await;
            // An unavailable custom sound must not silence the message.
            if path.is_none() && name != sound.default {
                path = self.cache.get(cache::Kind::Sound, &sound.default).await;
            }
            if let Some(path) = path {
                self.sound.play(path, sound.volume);
            }
        }
    }
}

/// Logs metadata only; the message text stays out of the journal.
fn log_message(message: &Message, action: &str) {
    info!(
        "message {} [{}] priority {}, {}: {:?}",
        message.id,
        action,
        message.priority,
        message.app,
        message.display_title()
    );
}

/// Highest message id for each receipt.
fn latest_per_receipt(messages: &[&Message]) -> HashMap<Receipt, MessageId> {
    let mut latest: HashMap<Receipt, MessageId> = HashMap::new();
    for m in messages {
        if let Some(r) = &m.receipt {
            let id = latest.entry(r.clone()).or_insert(m.id);
            *id = (*id).max(m.id);
        }
    }
    latest
}

/// Re-reads the configuration; an invalid file keeps the current one.
fn reload(path: &std::path::Path, config: &watch::Sender<Config>) {
    match Config::load(path) {
        Ok(new) => {
            info!("configuration reloaded");
            config.send_replace(new);
        }
        Err(e) => warn!("{e:#}; keeping the current configuration"),
    }
}

/// Sleeps before the next attempt; a wake event cuts the wait short.
async fn wait(wake: &mut WakeReceiver, backoff: &mut Backoff, delay: Duration) {
    tokio::select! {
        _ = sleep(delay) => {}
        // `changed` fails if the watcher is gone; the branch is then disabled.
        Ok(()) = wake.changed() => backoff.reset(),
    }
}

async fn close(stream: PushStream) {
    if timeout(CLOSE_TIMEOUT, stream.close()).await.is_err() {
        debug!("closing the old connection timed out");
    }
}

/// Notification failures are logged; messages are already stored.
fn report(result: Result<()>) {
    if let Err(e) = result {
        warn!("cannot show notification: {e:#}");
    }
}

/// HTTP 4xx from the API with an established session: secret or device is invalid.
fn is_session_rejected(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|e| e.downcast_ref::<nudge_api::Error>())
        .any(nudge_api::Error::is_client_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_latest_retry_per_receipt() {
        let m = |id: u64, receipt: Option<&str>| -> Message {
            serde_json::from_value(serde_json::json!({"id": id, "priority": 2, "receipt": receipt}))
                .unwrap()
        };
        let all = [
            m(1, Some("a")),
            m(3, Some("a")),
            m(2, Some("b")),
            m(4, None),
        ];
        let refs: Vec<&Message> = all.iter().collect();
        let latest = latest_per_receipt(&refs);
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[&Receipt::new("a")], MessageId(3));
        assert_eq!(latest[&Receipt::new("b")], MessageId(2));
    }

    #[tokio::test(start_paused = true)]
    async fn wake_event_cuts_wait_short_and_resets_backoff() {
        let (tx, mut rx) = watch::channel(0u64);
        let mut backoff = Backoff::default();
        for _ in 0..4 {
            backoff.next_delay();
        }

        let start = tokio::time::Instant::now();
        let sender = tokio::spawn(async move {
            sleep(Duration::from_secs(3)).await;
            tx.send_modify(|n| *n += 1);
            tx
        });
        wait(&mut rx, &mut backoff, Duration::from_secs(300)).await;
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(backoff.next_delay() <= Duration::from_secs(6));
        drop(sender.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_without_watcher_sleeps_full_delay() {
        let (tx, mut rx) = watch::channel(0u64);
        drop(tx);
        let start = tokio::time::Instant::now();
        wait(&mut rx, &mut Backoff::default(), Duration::from_secs(30)).await;
        assert!(start.elapsed() >= Duration::from_secs(30));
    }
}
