//! History as an HTML page, opened in the default browser.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::{debug, info};
use zbus::zvariant::{Fd, OwnedObjectPath, Value};

use crate::control::local_datetime;
use crate::credentials::write_private;
use crate::history::{Entry, Filter, History};
use crate::notify::{html_to_text, web_url};
use crate::paths::Paths;

/// Entries on the page.
const LIMIT: usize = 500;

/// Writes the page and opens it. Through the desktop portal first: a
/// sandboxed (snap, Flatpak) browser cannot read files in hidden
/// directories such as `~/.cache`, but receives the portal's copy.
pub async fn open_history(paths: &Paths) -> Result<()> {
    let db = paths.history_db();
    let entries = tokio::task::block_in_place(|| -> Result<Vec<Entry>> {
        match History::open_read_only(&db)? {
            Some(history) => history.query(&Filter {
                limit: LIMIT,
                ..Filter::default()
            }),
            None => Ok(Vec::new()),
        }
    })?;
    let file = paths.cache.join("history.html");
    let page = render(
        &entries,
        &jiff::Zoned::now().strftime("%Y-%m-%d %H:%M").to_string(),
    );
    tokio::task::block_in_place(|| write_private(&file, page.as_bytes()))?;

    match open_with_portal(&file).await {
        Ok(()) => {}
        Err(e) => {
            debug!("portal cannot open the history ({e:#}), trying xdg-open");
            let status = tokio::process::Command::new("xdg-open")
                .arg(&file)
                .status()
                .await
                .context("cannot run xdg-open")?;
            anyhow::ensure!(status.success(), "xdg-open failed ({status})");
        }
    }
    info!("history opened ({} entries)", entries.len());
    Ok(())
}

async fn open_with_portal(file: &Path) -> Result<()> {
    let handle = std::fs::File::open(file)?;
    let conn = zbus::Connection::session().await?;
    let options: HashMap<&str, Value<'_>> = HashMap::from([("ask", Value::Bool(false))]);
    let reply = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.OpenURI"),
            "OpenFile",
            &("", Fd::from(&handle), options),
        )
        .await?;
    let request: OwnedObjectPath = reply.body().deserialize()?;
    debug!("portal request {}", request.as_str());
    Ok(())
}

fn render(entries: &[Entry], generated: &str) -> String {
    let mut rows = String::new();
    for e in entries {
        let text = match (&e.message, e.html) {
            _ if e.malformed => "(unreadable message)".to_owned(),
            (Some(m), true) => html_to_text(m),
            (Some(m), false) => m.clone(),
            (None, _) => String::new(),
        };
        let link = e
            .url
            .as_deref()
            .and_then(web_url)
            .map(|url| {
                let label = e.url_title.as_deref().unwrap_or(&url);
                format!(
                    r#"<a href="{}" rel="noreferrer">{}</a>"#,
                    escape(&url),
                    escape(label)
                )
            })
            .unwrap_or_default();
        let class = match e.priority {
            2.. => " class=\"emergency\"",
            1 => " class=\"high\"",
            i8::MIN..=-1 => " class=\"low\"",
            0 => "",
        };
        let _ = writeln!(
            rows,
            "<tr{class}><td class=\"date\">{}</td><td>{}</td><td class=\"prio\">{}</td>\
             <td><strong>{}</strong><div class=\"text\">{}</div>{}</td></tr>",
            escape(&local_datetime(e.date)),
            escape(e.app.as_deref().unwrap_or("")),
            e.priority,
            escape(e.display_title()),
            escape(&text),
            if link.is_empty() {
                String::new()
            } else {
                format!("<div>{link}</div>")
            },
        );
    }
    if entries.is_empty() {
        rows.push_str("<tr><td colspan=\"4\">No messages yet.</td></tr>\n");
    }
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Nudge history</title>
<style>
:root {{ color-scheme: light dark; --muted: #6b7280; --line: #d1d5db; --emergency: #b91c1c; --high: #b45309; }}
@media (prefers-color-scheme: dark) {{ :root {{ --muted: #9ca3af; --line: #374151; --emergency: #f87171; --high: #fbbf24; }} }}
body {{ font: 15px/1.45 system-ui, sans-serif; margin: 0 auto; max-width: 70rem; padding: 1.5rem 1rem; }}
h1 {{ font-size: 1.4rem; margin: 0; }}
p.meta {{ color: var(--muted); margin: .2rem 0 1.2rem; }}
table {{ border-collapse: collapse; width: 100%; }}
th, td {{ border-bottom: 1px solid var(--line); padding: .45rem .5rem; text-align: left; vertical-align: top; }}
th {{ color: var(--muted); font-weight: 600; }}
td.date {{ white-space: nowrap; color: var(--muted); }}
td.prio {{ text-align: right; }}
.text {{ white-space: pre-wrap; overflow-wrap: anywhere; }}
tr.emergency strong, tr.emergency td.prio {{ color: var(--emergency); }}
tr.high td.prio {{ color: var(--high); }}
tr.low {{ color: var(--muted); }}
</style>
</head>
<body>
<h1>Nudge history</h1>
<p class="meta">Last {count} messages, generated {generated}. Nudge is an unofficial Pushover Open Client.</p>
<table>
<thead><tr><th>Date</th><th>App</th><th>Prio</th><th>Message</th></tr></thead>
<tbody>
{rows}</tbody>
</table>
</body>
</html>
"#,
        count = entries.len(),
        generated = escape(generated),
    )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(message: &str, url: Option<&str>) -> Entry {
        Entry {
            id: Some(1),
            date: 0,
            app: Some("App".into()),
            title: Some("<script>alert(1)</script>".into()),
            message: Some(message.into()),
            priority: 0,
            url: url.map(Into::into),
            url_title: None,
            html: false,
            acked: false,
            malformed: false,
        }
    }

    #[test]
    fn content_is_escaped() {
        let page = render(&[entry("a <b> & \"c\"", None)], "now");
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(page.contains("a &lt;b&gt; &amp; &quot;c&quot;"));
        assert!(!page.contains("<script>"));
        assert!(page.contains("default-src 'none'"));
    }

    #[test]
    fn only_web_links_are_clickable() {
        let page = render(&[entry("x", Some("example.org/a?b=1&c=2"))], "now");
        assert!(page.contains(r#"href="https://example.org/a?b=1&amp;c=2""#));
        let page = render(&[entry("x", Some("javascript:alert(1)"))], "now");
        assert!(!page.contains("href="));
    }

    #[test]
    fn empty_history() {
        assert!(render(&[], "now").contains("No messages yet."));
    }
}
