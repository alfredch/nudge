//! Handles clicks on notification buttons.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use nudge_api::{Client, Receipt, Secret};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::history::History;
use crate::notify::{Action, Invoked, Notifier, Target, is_web_url};

/// A command line tool that puts its stdin on the clipboard.
#[derive(Debug, Clone)]
pub struct Clipboard {
    program: PathBuf,
    args: &'static [&'static str],
}

impl Clipboard {
    /// `wl-copy` on Wayland, `xclip` on X11 (or through Xwayland).
    pub fn detect() -> Option<Self> {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let x11 = std::env::var_os("DISPLAY").is_some();
        let candidates: [(bool, &str, &'static [&'static str]); 2] = [
            (wayland, "wl-copy", &[]),
            (x11, "xclip", &["-selection", "clipboard"]),
        ];
        candidates
            .into_iter()
            .filter(|(usable, ..)| *usable)
            .find_map(|(_, name, args)| find_in_path(name).map(|program| Self { program, args }))
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    async fn copy(&self, text: &str) -> std::io::Result<()> {
        let mut child = Command::new(&self.program)
            .args(self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes()).await?;
        }
        // The tool may keep running to serve the clipboard; reap it whenever
        // it exits, without waiting here.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

pub struct Handler {
    pub client: Client,
    pub secret: Secret,
    pub notifier: Notifier,
    pub history: History,
    pub clipboard: Option<Clipboard>,
}

impl Handler {
    pub async fn run(mut self, mut clicks: mpsc::UnboundedReceiver<Invoked>) {
        while let Some(Invoked { action, target }) = clicks.recv().await {
            match action {
                Action::Acknowledge => self.acknowledge(&target).await,
                Action::OpenLink => open_link(&target).await,
                Action::Copy => copy(self.clipboard.as_ref(), &target).await,
            }
        }
    }

    /// Only ever called for an explicit click on "Acknowledge", as the API
    /// requires; closing or timing out never acknowledges.
    async fn acknowledge(&mut self, target: &Target) {
        let Some(receipt) = &target.receipt else {
            return;
        };
        self.notifier.set_acknowledging(receipt, true);
        let result = self.client.acknowledge(&self.secret, receipt).await;
        if result.is_ok() {
            // Before clearing the flag, so no repeat slips through in between.
            self.mark_acked(receipt);
        }
        self.notifier.set_acknowledging(receipt, false);
        match result {
            Ok(()) => {
                info!("message {} acknowledged", target.message);
                if let Err(e) = self.notifier.close_receipt(receipt).await {
                    warn!("cannot close notification: {e:#}");
                }
            }
            Err(e) => {
                // No automatic retry: the API forbids retrying without delay,
                // and the notification stays for the user to try again.
                warn!("cannot acknowledge message {}: {e}", target.message);
                let shown = self
                    .notifier
                    .show_info(
                        "Could not acknowledge",
                        &format!("{e}\n\nThe emergency message is still active. Try again."),
                        false,
                    )
                    .await;
                if let Err(e) = shown {
                    warn!("cannot show notification: {e:#}");
                }
            }
        }
    }

    fn mark_acked(&mut self, receipt: &Receipt) {
        if let Err(e) = tokio::task::block_in_place(|| self.history.mark_acked(receipt)) {
            warn!("cannot mark message as acknowledged in history: {e:#}");
        }
    }
}

async fn copy(clipboard: Option<&Clipboard>, target: &Target) {
    let Some(clipboard) = clipboard else {
        return;
    };
    match clipboard.copy(&target.text).await {
        Ok(()) => info!("message {} copied to clipboard", target.message),
        Err(e) => warn!("cannot run {}: {e}", clipboard.program.display()),
    }
}

async fn open_link(target: &Target) {
    let Some(url) = target.url.as_deref().filter(|u| is_web_url(u)) else {
        return;
    };
    let child = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(mut child) => {
            info!("opening link of message {}", target.message);
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
        Err(e) => warn!("cannot run xdg-open: {e}"),
    }
}
