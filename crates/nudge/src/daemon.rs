//! The long-running receive loop.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use nudge_api::{Client, DEFAULT_PUSH_URL, Frame, Message, PushStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, trace, warn};

use crate::credentials::{self, Credentials};
use crate::history::History;
use crate::notify;
use crate::paths::Paths;

/// No keep-alive within this time means the connection is dead.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(90);
/// Pause before reconnecting after a failure. Backoff follows in M3.
const RECONNECT_DELAY: Duration = Duration::from_secs(10);
/// Pause before reconnecting when the server asked for it.
const RELOAD_DELAY: Duration = Duration::from_secs(1);

/// Why the daemon stopped.
#[derive(Debug)]
pub enum Exit {
    /// SIGTERM or Ctrl-C.
    Shutdown,
    /// The session is no longer usable; the user has to act.
    NeedsLogin(String),
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
    Fatal(String),
}

struct Daemon {
    client: Client,
    user_agent: String,
    credentials: Credentials,
    history: History,
}

pub async fn run(paths: &Paths, user_agent: String) -> Result<Exit> {
    let Some((credentials, backend)) = credentials::load(paths).await? else {
        return Ok(Exit::NeedsLogin(
            "Not logged in. Run `nudge login` first.".to_owned(),
        ));
    };
    debug!(?backend, device = %credentials.device_id, "credentials loaded");

    let mut daemon = Daemon {
        client: Client::new(&user_agent)?,
        user_agent,
        credentials,
        history: History::open(&paths.history_db())?,
    };

    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        exit = daemon.run_loop() => {
            if let Exit::NeedsLogin(reason) = &exit {
                error!("{reason}");
                if let Err(e) = notify::show_info("Nudge stopped", reason, true).await {
                    warn!("cannot show notification: {e:#}");
                }
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
            match self.session().await {
                Ok(SessionEnd::Reload) => {
                    info!("server requested reconnect");
                    sleep(RELOAD_DELAY).await;
                }
                Ok(SessionEnd::Fatal(reason)) => return Exit::NeedsLogin(reason),
                Err(e) if is_session_rejected(&e) => {
                    return Exit::NeedsLogin(format!(
                        "The session was rejected ({e:#}). Run `nudge login` again."
                    ));
                }
                Err(e) => {
                    warn!(
                        "connection lost: {e:#}; reconnecting in {}s",
                        RECONNECT_DELAY.as_secs()
                    );
                    sleep(RECONNECT_DELAY).await;
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
        info!("connected to push server");

        // Connect first, then fetch: anything arriving after this point
        // triggers a `!`, so nothing slips through between the two steps.
        self.sync(Mode::Backlog).await?;

        loop {
            let frame = timeout(KEEPALIVE_TIMEOUT, stream.next_frame())
                .await
                .map_err(|_| anyhow!("no keep-alive for {}s", KEEPALIVE_TIMEOUT.as_secs()))??;
            match frame {
                None => return Err(anyhow!("server closed the connection")),
                Some(Frame::Keepalive) => trace!("keep-alive"),
                Some(Frame::NewMessages) => self.sync(Mode::Live).await?,
                Some(Frame::Reload) => {
                    let _ = stream.close().await;
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

    /// Fetch → persist → delete on server → notify.
    async fn sync(&mut self, mode: Mode) -> Result<()> {
        let device = &self.credentials.device_id;
        let fetched = self
            .client
            .fetch_messages(&self.credentials.secret, device)
            .await?;
        if fetched.is_empty() {
            debug!("no pending messages");
            return Ok(());
        }
        for m in &fetched.malformed {
            warn!(id = ?m.id, "cannot decode message: {}", m.error);
        }

        let inserted = tokio::task::block_in_place(|| self.history.store(device, &fetched))
            .context("cannot store messages in history")?;

        if let Some(highest) = fetched.highest_id() {
            self.client
                .delete_up_to(&self.credentials.secret, device, highest)
                .await?;
        }
        info!(
            fetched = fetched.messages.len() + fetched.malformed.len(),
            new = inserted.messages.len() + inserted.malformed,
            ?mode,
            "messages processed"
        );

        let new: Vec<&Message> = fetched
            .messages
            .iter()
            .filter(|m| inserted.messages.contains(&m.id))
            .collect();
        announce(&new, mode).await;
        if inserted.malformed > 0 {
            report(
                notify::show_info(
                    "Unreadable message",
                    "Nudge received a message it could not read. It was saved to the history.",
                    false,
                )
                .await,
            );
        }
        Ok(())
    }
}

async fn announce(messages: &[&Message], mode: Mode) {
    let visible: Vec<&Message> = messages
        .iter()
        .copied()
        .filter(|m| notify::display_for(m).is_some())
        .collect();

    if mode == Mode::Live || visible.len() <= 1 {
        for m in visible {
            report(notify::show_message(m).await);
        }
        return;
    }

    let (persistent, rest): (Vec<&Message>, Vec<&Message>) = visible
        .into_iter()
        .partition(|m| m.requires_persistent_display());
    for m in persistent {
        report(notify::show_message(m).await);
    }
    match rest.as_slice() {
        [] => {}
        [single] => report(notify::show_message(single).await),
        _ => report(notify::show_summary(&rest).await),
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
