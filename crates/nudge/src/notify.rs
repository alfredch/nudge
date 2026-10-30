//! Desktop notifications via the freedesktop D-Bus interface.

use anyhow::Result;
use notify_rust::{Hint, Notification, Timeout, Urgency};
use nudge_api::Message;

const APP_NAME: &str = "Nudge";
/// Lines listed in a summary notification before "and N more".
const SUMMARY_LINES: usize = 5;

/// How a message is shown, derived from its priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Display {
    pub urgency: Urgency,
    /// Stays on screen until the user dismisses it.
    pub persistent: bool,
}

/// `None` means history only, no notification.
pub fn display_for(message: &Message) -> Option<Display> {
    let (urgency, persistent) = match message.priority {
        i8::MIN..=-2 => return None,
        -1 => (Urgency::Low, false),
        0 => (Urgency::Normal, false),
        1 => (Urgency::Critical, false),
        2.. => (Urgency::Critical, message.requires_persistent_display()),
    };
    Some(Display {
        urgency,
        persistent,
    })
}

pub async fn show_message(message: &Message) -> Result<()> {
    let Some(display) = display_for(message) else {
        return Ok(());
    };
    let mut body = if message.html {
        escape(&html_to_text(&message.message))
    } else {
        escape(&message.message)
    };
    if let Some(url) = &message.url {
        let label = message.url_title.as_deref().unwrap_or(url);
        body.push_str("\n\n");
        body.push_str(&escape(label));
        if label != url {
            body.push_str(": ");
            body.push_str(&escape(url));
        }
    }

    let mut n = base(message.display_title(), &body);
    n.urgency(display.urgency);
    if display.persistent {
        n.timeout(Timeout::Never).hint(Hint::Resident(true));
    }
    n.show_async().await?;
    Ok(())
}

/// One notification for messages that arrived while nudge was not running.
pub async fn show_summary(messages: &[&Message]) -> Result<()> {
    let summary = match messages.len() {
        1 => "1 message while you were away".to_owned(),
        n => format!("{n} messages while you were away"),
    };
    let mut lines: Vec<String> = messages
        .iter()
        .take(SUMMARY_LINES)
        .map(|m| escape(&format!("{}: {}", m.app, m.display_title())))
        .collect();
    if messages.len() > SUMMARY_LINES {
        lines.push(format!("… and {} more", messages.len() - SUMMARY_LINES));
    }
    base(&summary, &lines.join("\n")).show_async().await?;
    Ok(())
}

/// Status notification from nudge itself.
pub async fn show_info(summary: &str, body: &str, critical: bool) -> Result<()> {
    let mut n = base(summary, &escape(body));
    if critical {
        n.urgency(Urgency::Critical)
            .timeout(Timeout::Never)
            .hint(Hint::Resident(true));
    }
    n.show_async().await?;
    Ok(())
}

fn base(summary: &str, body: &str) -> Notification {
    let mut n = Notification::new();
    n.appname(APP_NAME).summary(summary).body(body);
    n
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
/// text. Proper sanitizing that keeps supported markup follows in M4.
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
        let d = |p: i8| display_for(&msg(serde_json::json!({"id": 1, "priority": p})));
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
        assert_eq!(display_for(&m).map(|d| d.persistent), Some(false));
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
    fn html_result_is_escaped_for_display() {
        assert_eq!(escape(&html_to_text("&lt;script&gt;")), "&lt;script&gt;");
    }
}
