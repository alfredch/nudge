//! Desktop notifications via the freedesktop D-Bus interface.
//!
//! Sending and listening for actions share one connection: some servers
//! deliver `ActionInvoked` only to the connection that created the
//! notification.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context as _, Result};
use futures_util::StreamExt;
use nudge_api::{Message, MessageId, Receipt};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use zbus::zvariant::Value;

const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
const APP_NAME: &str = "Nudge";
/// Lines listed in a summary notification before "and N more".
const SUMMARY_LINES: usize = 5;
/// Tags every markup-capable server understands (GNOME supports no more).
const ALLOWED_TAGS: [&str; 3] = ["b", "i", "u"];
/// Notifications whose actions are remembered. Beyond that the oldest are
/// forgotten; servers do not report every notification as closed.
const MAX_TRACKED: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low = 0,
    Normal = 1,
    Critical = 2,
}

/// Buttons on a message notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Acknowledge,
    OpenLink,
    Copy,
}

impl Action {
    const ALL: [Action; 3] = [Action::Acknowledge, Action::OpenLink, Action::Copy];

    fn key(self) -> &'static str {
        match self {
            Action::Acknowledge => "acknowledge",
            Action::OpenLink => "open-link",
            Action::Copy => "copy",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Action::Acknowledge => "Acknowledge",
            Action::OpenLink => "Open link",
            Action::Copy => "Copy",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }
}

/// What an action needs to know about the message it belongs to.
#[derive(Debug, Clone)]
pub struct Target {
    pub message: MessageId,
    pub receipt: Option<Receipt>,
    pub url: Option<String>,
    /// Plain message text, for copying.
    pub text: String,
}

/// A button the user clicked.
#[derive(Debug)]
pub struct Invoked {
    pub action: Action,
    pub target: Target,
}

/// How a message is shown, derived from its priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Display {
    pub urgency: Urgency,
    /// Stays on screen until the user dismisses it.
    pub persistent: bool,
}

/// `None` means history only, no notification. During quiet hours,
/// priority -1 and 0 are delivered silently (low urgency, no banner on
/// most servers); priority 1 and 2 are not affected.
pub fn display_for(message: &Message, quiet: bool) -> Option<Display> {
    let (urgency, persistent) = match message.priority {
        i8::MIN..=-2 => return None,
        -1 => (Urgency::Low, false),
        0 if quiet => (Urgency::Low, false),
        0 => (Urgency::Normal, false),
        1 => (Urgency::Critical, false),
        2.. => (Urgency::Critical, message.requires_persistent_display()),
    };
    Some(Display {
        urgency,
        persistent,
    })
}

/// Connection to the notification server.
#[derive(Clone)]
pub struct Notifier {
    inner: Arc<Inner>,
}

struct Inner {
    conn: zbus::Connection,
    /// The server parses body markup; otherwise the spec requires plain text.
    markup: bool,
    /// The server shows action buttons.
    actions: bool,
    /// A clipboard tool is available.
    copy: bool,
    state: Mutex<State>,
}

/// One notification to send.
struct Note<'a> {
    summary: &'a str,
    body: &'a str,
    urgency: Urgency,
    persistent: bool,
    icon: Option<&'a Path>,
    actions: &'a [Action],
    replaces: u32,
}

impl Notifier {
    /// Connects to the session bus and starts listening for actions.
    /// Clicked buttons arrive on the returned receiver.
    pub async fn connect(copy: bool) -> Result<(Self, mpsc::UnboundedReceiver<Invoked>)> {
        let conn = zbus::Connection::session()
            .await
            .context("cannot connect to the D-Bus session bus")?;
        let proxy = zbus::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, BUS_NAME).await?;

        let caps: Vec<String> = match proxy.call("GetCapabilities", &()).await {
            Ok(caps) => caps,
            Err(e) => {
                warn!("cannot query notification server: {e}");
                Vec::new()
            }
        };
        debug!(?caps, "notification server capabilities");
        let has = |cap: &str| caps.iter().any(|c| c == cap);

        // Subscribe before the first notification, so no click is missed.
        let invoked = proxy.receive_signal("ActionInvoked").await?;
        let closed = proxy.receive_signal("NotificationClosed").await?;

        let notifier = Self {
            inner: Arc::new(Inner {
                conn,
                markup: has("body-markup"),
                actions: has("actions"),
                copy,
                state: Mutex::default(),
            }),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(notifier.clone().listen(invoked, closed, tx));
        Ok((notifier, rx))
    }

    async fn listen(
        self,
        mut invoked: zbus::proxy::SignalStream<'static>,
        mut closed: zbus::proxy::SignalStream<'static>,
        tx: mpsc::UnboundedSender<Invoked>,
    ) {
        loop {
            tokio::select! {
                // A click on a transient notification is followed by its
                // close signal; handle the click before forgetting the target.
                biased;
                Some(msg) = invoked.next() => {
                    let Ok((id, key)) = msg.body().deserialize::<(u32, String)>() else {
                        continue;
                    };
                    let target = self.state().targets.get(&id).cloned();
                    if let (Some(action), Some(target)) = (Action::from_key(&key), target) {
                        debug!("notification {id}: {key}");
                        if tx.send(Invoked { action, target }).is_err() {
                            return;
                        }
                    }
                }
                Some(msg) = closed.next() => {
                    if let Ok((id, _reason)) = msg.body().deserialize::<(u32, u32)>() {
                        self.state().forget(id);
                    }
                }
                else => {
                    warn!("notification server signals ended, buttons no longer work");
                    return;
                }
            }
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Plain text prepared for the body: escaped if the server parses markup.
    fn text(&self, raw: &str) -> String {
        if self.inner.markup {
            escape(raw)
        } else {
            raw.to_owned()
        }
    }

    pub async fn show_message(
        &self,
        message: &Message,
        quiet: bool,
        icon: Option<&Path>,
    ) -> Result<()> {
        let Some(display) = display_for(message, quiet) else {
            return Ok(());
        };
        let mut body = match (message.html, self.inner.markup) {
            (true, true) => sanitize_html(&message.message),
            (true, false) => html_to_text(&message.message),
            (false, _) => self.text(&message.message),
        };
        if let Some(url) = &message.url {
            let label = message.url_title.as_deref().unwrap_or(url);
            body.push_str("\n\n");
            body.push_str(&self.text(label));
            if label != url {
                body.push_str(": ");
                body.push_str(&self.text(url));
            }
        }

        let link = message.url.as_deref().and_then(web_url);
        let mut actions = Vec::new();
        if self.inner.actions {
            if display.persistent && message.can_acknowledge() {
                actions.push(Action::Acknowledge);
            }
            if link.is_some() {
                actions.push(Action::OpenLink);
            }
            if self.inner.copy {
                actions.push(Action::Copy);
            }
        }

        // Emergency retries replace the notification of the same receipt.
        let replaces = message
            .receipt
            .as_ref()
            .and_then(|r| self.state().by_receipt.get(r).copied())
            .unwrap_or(0);
        let id = self
            .notify(Note {
                summary: message.display_title(),
                body: &body,
                urgency: display.urgency,
                persistent: display.persistent,
                icon,
                actions: &actions,
                replaces,
            })
            .await?;

        if !actions.is_empty() {
            let text = if message.html {
                html_to_text(&message.message)
            } else {
                message.message.clone()
            };
            self.state().track(
                id,
                Target {
                    message: message.id,
                    receipt: message.receipt.clone(),
                    url: link,
                    text,
                },
            );
        }
        Ok(())
    }

    /// One notification for messages that arrived while nudge was not running.
    pub async fn show_summary(&self, messages: &[&Message], quiet: bool) -> Result<()> {
        let summary = match messages.len() {
            1 => "1 message while you were away".to_owned(),
            n => format!("{n} messages while you were away"),
        };
        let mut lines: Vec<String> = messages
            .iter()
            .take(SUMMARY_LINES)
            .map(|m| self.text(&format!("{}: {}", m.app, m.display_title())))
            .collect();
        if messages.len() > SUMMARY_LINES {
            lines.push(format!("… and {} more", messages.len() - SUMMARY_LINES));
        }
        self.notify(Note {
            summary: &summary,
            body: &lines.join("\n"),
            urgency: if quiet { Urgency::Low } else { Urgency::Normal },
            persistent: false,
            icon: None,
            actions: &[],
            replaces: 0,
        })
        .await?;
        Ok(())
    }

    /// Status notification from nudge itself.
    pub async fn show_info(&self, summary: &str, body: &str, critical: bool) -> Result<()> {
        self.notify(Note {
            summary,
            body: &self.text(body),
            urgency: if critical {
                Urgency::Critical
            } else {
                Urgency::Normal
            },
            persistent: critical,
            icon: None,
            actions: &[],
            replaces: 0,
        })
        .await?;
        Ok(())
    }

    /// Closes the notification showing this receipt, if there is one.
    pub async fn close_receipt(&self, receipt: &Receipt) -> Result<()> {
        let Some(id) = self.state().forget_receipt(receipt) else {
            return Ok(());
        };
        self.inner
            .conn
            .call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(BUS_NAME),
                "CloseNotification",
                &(id,),
            )
            .await?;
        Ok(())
    }

    async fn notify(&self, note: Note<'_>) -> Result<u32> {
        let icon = note.icon.and_then(Path::to_str).unwrap_or_default();
        let actions: Vec<&str> = note
            .actions
            .iter()
            .flat_map(|a| [a.key(), a.label()])
            .collect();
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("urgency", Value::U8(note.urgency as u8));
        // nudge plays message sounds itself.
        hints.insert("suppress-sound", Value::Bool(true));
        if note.persistent {
            hints.insert("resident", Value::Bool(true));
        }
        // -1: server default; 0: never expires.
        let timeout: i32 = if note.persistent { 0 } else { -1 };

        let reply = self
            .inner
            .conn
            .call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(BUS_NAME),
                "Notify",
                &(
                    APP_NAME,
                    note.replaces,
                    icon,
                    note.summary,
                    note.body,
                    actions,
                    hints,
                    timeout,
                ),
            )
            .await
            .context("cannot show notification")?;
        Ok(reply.body().deserialize()?)
    }
}

/// Targets of notifications with buttons, by notification id.
#[derive(Default)]
struct State {
    targets: BTreeMap<u32, Target>,
    /// Emergency notifications, so retries and acknowledgements find them.
    by_receipt: HashMap<Receipt, u32>,
}

impl State {
    fn track(&mut self, id: u32, target: Target) {
        if let Some(receipt) = &target.receipt {
            self.by_receipt.insert(receipt.clone(), id);
        }
        self.targets.insert(id, target);
        while self.targets.len() > MAX_TRACKED {
            if let Some((&oldest, _)) = self.targets.first_key_value() {
                self.forget(oldest);
            }
        }
    }

    fn forget(&mut self, id: u32) {
        if let Some(Target {
            receipt: Some(receipt),
            ..
        }) = self.targets.remove(&id)
            && self.by_receipt.get(&receipt) == Some(&id)
        {
            self.by_receipt.remove(&receipt);
        }
    }

    fn forget_receipt(&mut self, receipt: &Receipt) -> Option<u32> {
        let id = self.by_receipt.remove(receipt)?;
        self.targets.remove(&id);
        Some(id)
    }
}

/// Only web links are opened; other schemes could start arbitrary handlers.
/// The link to open for a message URL. Senders often omit the scheme
/// ("example.org/x"); such host names get `https://`. Any other scheme
/// (`file:`, `javascript:`, ...) yields `None`.
pub fn web_url(url: &str) -> Option<String> {
    let url = url.trim();
    if is_web_url(url) {
        return Some(url.to_owned());
    }
    if url.is_empty() || url.contains("://") || url.contains(char::is_whitespace) {
        return None;
    }
    let host_port = url.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = match host_port.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (host_port, None),
    };
    // "host:8080" is a port; "mailto:x" or "javascript:x" is a scheme.
    let port_ok = port.is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let host_ok = host.contains('.')
        && host
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-');
    (port_ok && host_ok).then(|| format!("https://{url}"))
}

pub fn is_web_url(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// Escapes text for the notification body, which servers parse as markup.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Reduces Pushover HTML (`<b>`, `<i>`, `<u>`, `<font>`, `<a>`) to plain
/// text, for servers without markup support.
pub fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            // Unclosed tag: keep the remainder as text.
            text.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let tag = rest[start + 1..start + end].trim().to_ascii_lowercase();
        if tag.starts_with("br") || tag == "/p" || tag == "/div" {
            text.push('\n');
        }
        rest = &rest[start + end + 1..];
    }
    text.push_str(rest);
    decode_entities(&text)
}

/// Converts Pushover HTML into the markup subset notification servers
/// accept: `<b>`, `<i>`, `<u>` (with `strong`/`em` as aliases). Colors and
/// other tags are dropped, links become "text (url)", text is escaped and
/// tags are always balanced, so the server never rejects the markup.
pub fn sanitize_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut open: Vec<&'static str> = Vec::new();
    // Position in `out` where the current link text starts, and its target.
    let mut link: Option<(usize, String)> = None;
    let mut rest = html;

    while let Some(start) = rest.find('<') {
        out.push_str(&escape(&decode_entities(&rest[..start])));
        let Some(len) = rest[start..].find('>') else {
            rest = &rest[start..];
            break;
        };
        let raw = &rest[start + 1..start + len];
        rest = &rest[start + len + 1..];

        let (closing, raw) = match raw.trim_start().strip_prefix('/') {
            Some(r) => (true, r),
            None => (false, raw.trim_start()),
        };
        let name: String = raw
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        let tag = match name.as_str() {
            "strong" => "b",
            "em" => "i",
            other => ALLOWED_TAGS
                .iter()
                .copied()
                .find(|t| *t == other)
                .unwrap_or(""),
        };

        match (name.as_str(), closing) {
            ("br", _) | ("p" | "div", true) => out.push('\n'),
            ("a", false) => link = href(raw).map(|url| (out.len(), url)),
            ("a", true) => {
                if let Some((start, url)) = link.take() {
                    let shown = escape(&url);
                    if out[start..].trim() != shown {
                        out.push_str(&format!(" ({shown})"));
                    }
                }
            }
            _ if tag.is_empty() => {}
            (_, false) => {
                open.push(tag);
                out.push_str(&format!("<{tag}>"));
            }
            (_, true) => {
                // Only close what is open on top; ignore stray end tags.
                if open.last() == Some(&tag) {
                    open.pop();
                    out.push_str(&format!("</{tag}>"));
                }
            }
        }
    }
    out.push_str(&escape(&decode_entities(rest)));
    while let Some(tag) = open.pop() {
        out.push_str(&format!("</{tag}>"));
    }
    out
}

/// Extracts an http(s) `href` from the inside of an `<a ...>` tag.
fn href(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pos = lower.find("href")?;
    let value = tag[pos + 4..].trim_start().strip_prefix('=')?.trim_start();
    let url = match value.chars().next()? {
        q @ ('"' | '\'') => value[1..].split(q).next()?,
        _ => value.split(|c: char| c.is_whitespace()).next()?,
    };
    let url = decode_entities(url);
    is_web_url(&url).then_some(url)
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let entity = &rest[1..end];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(json: serde_json::Value) -> Message {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn priority_mapping() {
        let d = |p: i8| display_for(&msg(serde_json::json!({"id": 1, "priority": p})), false);
        assert_eq!(d(-2), None);
        assert_eq!(d(-1).map(|d| d.urgency), Some(Urgency::Low));
        assert_eq!(d(0).map(|d| d.urgency), Some(Urgency::Normal));
        assert_eq!(
            d(1),
            Some(Display {
                urgency: Urgency::Critical,
                persistent: false
            })
        );
        assert_eq!(
            d(2),
            Some(Display {
                urgency: Urgency::Critical,
                persistent: true
            })
        );
    }

    #[test]
    fn acked_emergency_is_not_persistent() {
        let m = msg(serde_json::json!({"id": 1, "priority": 2, "acked": 1}));
        assert_eq!(display_for(&m, false).map(|d| d.persistent), Some(false));
    }

    #[test]
    fn quiet_hours_silence_only_low_and_normal() {
        let d = |p: i8| display_for(&msg(serde_json::json!({"id": 1, "priority": p})), true);
        assert_eq!(d(-2), None);
        assert_eq!(d(-1).map(|d| d.urgency), Some(Urgency::Low));
        assert_eq!(d(0).map(|d| d.urgency), Some(Urgency::Low));
        assert_eq!(d(1).map(|d| d.urgency), Some(Urgency::Critical));
        assert_eq!(
            d(2).map(|d| (d.urgency, d.persistent)),
            Some((Urgency::Critical, true))
        );
    }

    fn target(id: u64, receipt: Option<&str>) -> Target {
        Target {
            message: MessageId(id),
            receipt: receipt.map(Receipt::new),
            url: None,
            text: String::new(),
        }
    }

    #[test]
    fn retry_replaces_and_close_forgets_receipt() {
        let mut s = State::default();
        let r = Receipt::new("r1");
        s.track(7, target(1, Some("r1")));
        assert_eq!(s.by_receipt.get(&r), Some(&7));
        // A retry shown in a new notification takes over the receipt.
        s.track(9, target(2, Some("r1")));
        s.forget(7);
        assert_eq!(s.by_receipt.get(&r), Some(&9));
        assert_eq!(s.forget_receipt(&r), Some(9));
        assert!(!s.targets.contains_key(&9));
    }

    #[test]
    fn tracking_is_bounded() {
        let mut s = State::default();
        for id in 0..(MAX_TRACKED as u32 + 10) {
            s.track(id, target(u64::from(id), Some(&format!("r{id}"))));
        }
        assert_eq!(s.targets.len(), MAX_TRACKED);
        assert_eq!(s.by_receipt.len(), MAX_TRACKED);
        assert!(!s.targets.contains_key(&0));
    }

    #[test]
    fn action_keys_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_key(a.key()), Some(a));
        }
        assert_eq!(Action::from_key("default"), None);
    }

    /// Shows a real notification; needs a desktop session.
    /// `cargo test -p nudge notify::tests::smoke -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn smoke() {
        let (n, _clicks) = Notifier::connect(false).await.unwrap();
        n.show_info("nudge smoke test", "Notify call works <&>", false)
            .await
            .unwrap();
        let m = msg(serde_json::json!({
            "id": 1, "priority": 2, "receipt": "smoke", "title": "nudge smoke test",
            "message": "<b>emergency</b> & resident", "html": 1, "url": "https://example.org"
        }));
        n.show_message(&m, false, None).await.unwrap();
        n.close_receipt(&Receipt::new("smoke")).await.unwrap();
    }

    #[test]
    fn only_web_urls() {
        assert!(is_web_url("https://x.org"));
        assert!(is_web_url("HTTP://x.org"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("javascript:alert(1)"));
    }

    #[test]
    fn web_url_adds_https() {
        let w = |u: &str| web_url(u);
        assert_eq!(w("https://x.org/a").as_deref(), Some("https://x.org/a"));
        assert_eq!(w("http://x.org").as_deref(), Some("http://x.org"));
        assert_eq!(w("veltronza.com").as_deref(), Some("https://veltronza.com"));
        assert_eq!(
            w(" www.x.org/a?b=1 ").as_deref(),
            Some("https://www.x.org/a?b=1")
        );
        assert_eq!(w("x.org:8080/p").as_deref(), Some("https://x.org:8080/p"));
        assert_eq!(w("münchen.de").as_deref(), Some("https://münchen.de"));
        assert_eq!(w("javascript:alert(1)"), None);
        assert_eq!(w("mailto:a@x.org"), None);
        assert_eq!(w("file:///etc/passwd"), None);
        assert_eq!(w("ftp://x.org"), None);
        assert_eq!(w("localhost"), None);
        assert_eq!(w("not a url"), None);
        assert_eq!(w(""), None);
    }

    #[test]
    fn escapes_markup() {
        assert_eq!(escape("a < b && c > d"), "a &lt; b &amp;&amp; c &gt; d");
    }

    #[test]
    fn html_reduced_to_text() {
        assert_eq!(
            html_to_text("Version <b>1.2</b> is out"),
            "Version 1.2 is out"
        );
        assert_eq!(html_to_text("a<br>b<br/>c"), "a\nb\nc");
        assert_eq!(
            html_to_text(r#"<a href="https://x.org">link</a> &amp; &lt;tag&gt; &#228;&#x00FC;"#),
            "link & <tag> äü"
        );
        assert_eq!(html_to_text("1 < 2"), "1 < 2");
        assert_eq!(html_to_text("AT&T &unknown; &"), "AT&T &unknown; &");
    }

    #[test]
    fn sanitize_keeps_basic_formatting() {
        assert_eq!(
            sanitize_html("<b>bold</b> <i>it</i> <u>u</u>"),
            "<b>bold</b> <i>it</i> <u>u</u>"
        );
        assert_eq!(
            sanitize_html("<STRONG>x</STRONG><em>y</em>"),
            "<b>x</b><i>y</i>"
        );
    }

    #[test]
    fn sanitize_drops_colors_and_unknown_tags() {
        assert_eq!(
            sanitize_html(r##"<font color="#ff0000">red</font> <span onclick="x">s</span>"##),
            "red s"
        );
        assert_eq!(sanitize_html("<script>alert(1)</script>"), "alert(1)");
    }

    #[test]
    fn sanitize_links() {
        assert_eq!(
            sanitize_html(r#"see <a href="https://x.org/a?b=1&amp;c=2">docs</a>"#),
            "see docs (https://x.org/a?b=1&amp;c=2)"
        );
        assert_eq!(
            sanitize_html("<a href='https://x.org'>https://x.org</a>"),
            "https://x.org"
        );
        assert_eq!(sanitize_html(r#"<a href="javascript:alert(1)">x</a>"#), "x");
    }

    #[test]
    fn sanitize_balances_tags() {
        assert_eq!(sanitize_html("<b>open"), "<b>open</b>");
        assert_eq!(sanitize_html("stray</b>"), "stray");
        assert_eq!(sanitize_html("<b><i>x</b></i>"), "<b><i>x</i></b>");
    }

    #[test]
    fn sanitize_escapes_text() {
        assert_eq!(sanitize_html("1 &lt; 2 & 3 > 2"), "1 &lt; 2 &amp; 3 &gt; 2");
        assert_eq!(sanitize_html("a<br>b"), "a\nb");
        assert_eq!(sanitize_html("x < y"), "x &lt; y");
    }

    #[test]
    fn html_result_is_escaped_for_display() {
        assert_eq!(escape(&html_to_text("&lt;script&gt;")), "&lt;script&gt;");
    }
}
