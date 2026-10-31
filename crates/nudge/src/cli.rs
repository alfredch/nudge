//! `status`, `pause`, `resume` (talk to the daemon) and `history` (reads
//! the database directly).

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use jiff::{Span, SpanRelativeTo, Unit};

use crate::control::{INTERFACE, OBJECT_PATH, local_datetime, local_time};
use crate::history::{Entry, Filter, History};
use crate::paths::{APP_ID, Paths};
use crate::status::{Report, unix_now};

/// `nudge status` when no daemon runs (LSB: "program is not running").
pub const EXIT_NOT_RUNNING: u8 = 3;

/// Proxy to the running daemon, or `None` if there is none.
async fn daemon() -> Result<Option<zbus::Proxy<'static>>> {
    let conn = zbus::Connection::session()
        .await
        .context("cannot connect to the D-Bus session bus")?;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    if !dbus.name_has_owner(APP_ID.try_into()?).await? {
        return Ok(None);
    }
    Ok(Some(
        zbus::Proxy::new(&conn, APP_ID, OBJECT_PATH, INTERFACE).await?,
    ))
}

/// A daemon that owns the name but does not answer (hung, or an old build
/// without the interface) must not block the command.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

async fn call<B, R>(daemon: &zbus::Proxy<'_>, method: &str, body: &B) -> Result<R>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    tokio::time::timeout(CALL_TIMEOUT, daemon.call(method, body))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "the daemon did not answer within {}s; restart it with `systemctl --user restart nudge`",
                CALL_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("{method} failed; is the daemon older than this command?"))
}

fn not_running() -> ExitCode {
    eprintln!("The nudge daemon is not running.");
    ExitCode::from(EXIT_NOT_RUNNING)
}

pub async fn status(paths: &Paths) -> Result<ExitCode> {
    let Some(daemon) = daemon().await? else {
        let newest = History::open_read_only(&paths.history_db())?
            .map(|h| h.newest())
            .transpose()?
            .flatten();
        if let Some(e) = newest {
            println!("Last message: {}", describe_entry(&e));
        }
        return Ok(not_running());
    };
    let json: String = call(&daemon, "Status", &()).await?;
    let report: Report = serde_json::from_str(&json).context("unexpected status")?;
    print!("{}", format_report(&report, unix_now()));
    // The daemon only knows messages since it started.
    if report.last_message.is_none()
        && let Some(e) = History::open_read_only(&paths.history_db())?
            .map(|h| h.newest())
            .transpose()?
            .flatten()
    {
        println!("Last:        {}", describe_entry(&e));
    }
    Ok(ExitCode::SUCCESS)
}

fn format_report(r: &Report, now: i64) -> String {
    let mut out = format!("Device:      {}\n", r.device);
    out += &match r.connected_since {
        Some(since) => format!(
            "Connection:  connected since {}\n",
            relative_date(since, now)
        ),
        None => "Connection:  disconnected, reconnecting\n".to_owned(),
    };
    out += &match (r.paused, r.paused_until) {
        (false, _) => "Pause:       off\n".to_owned(),
        (true, Some(until)) => format!("Pause:       until {}", local_time(until)),
        (true, None) => "Pause:       until resumed".to_owned(),
    };
    if r.paused {
        out += &format!(" ({} message(s) held back)\n", r.held_back);
    }
    for a in &r.alerts {
        out += &format!("Emergency:   {}: {} (not acknowledged)\n", a.app, a.title);
    }
    if let Some(m) = &r.last_message {
        out += &format!(
            "Last:        {}  {}: {}\n",
            relative_date(m.date, now),
            m.app,
            m.title
        );
    }
    out
}

/// "14:30" today, else "2026-10-30 14:30".
fn relative_date(unix: i64, now: i64) -> String {
    let tz = jiff::tz::TimeZone::system();
    let (Ok(t), Ok(n)) = (
        jiff::Timestamp::from_second(unix),
        jiff::Timestamp::from_second(now),
    ) else {
        return unix.to_string();
    };
    let (t, n) = (t.to_zoned(tz.clone()), n.to_zoned(tz));
    if t.date() == n.date() {
        t.strftime("%H:%M").to_string()
    } else {
        t.strftime("%Y-%m-%d %H:%M").to_string()
    }
}

pub async fn pause(duration: Option<&str>) -> Result<ExitCode> {
    let duration = duration.map(parse_duration).transpose()?;
    let Some(daemon) = daemon().await? else {
        return Ok(not_running());
    };
    let seconds = duration.map_or(0, |d| d.as_secs().max(1));
    let () = call(&daemon, "Pause", &(seconds,)).await?;
    match duration {
        Some(_) => println!(
            "Notifications paused until {}.",
            local_time(unix_now().saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX)))
        ),
        None => println!("Notifications paused until `nudge resume`."),
    }
    println!("Emergency messages (priority 2) still get through.");
    Ok(ExitCode::SUCCESS)
}

pub async fn resume() -> Result<ExitCode> {
    let Some(daemon) = daemon().await? else {
        return Ok(not_running());
    };
    let () = call(&daemon, "Resume", &()).await?;
    println!("Notifications resumed.");
    Ok(ExitCode::SUCCESS)
}

/// "30m", "2h", "1d", "1h30m", "2 hours", ...
pub fn parse_duration(text: &str) -> Result<Duration> {
    let span: Span = text
        .parse()
        .with_context(|| format!("invalid duration {text:?}, expected e.g. 30m, 2h or 1d"))?;
    let secs = span.total((Unit::Second, SpanRelativeTo::days_are_24_hours()))?;
    if !secs.is_finite() || secs <= 0.0 {
        bail!("the duration must be positive");
    }
    Ok(Duration::from_secs_f64(secs))
}

/// A duration ("2d": that long ago) or a local date/time ("2026-10-30",
/// "2026-10-30 14:00").
pub fn parse_since(text: &str, now: i64) -> Result<i64> {
    if let Ok(d) = parse_duration(text) {
        return Ok(now.saturating_sub(i64::try_from(d.as_secs()).unwrap_or(i64::MAX)));
    }
    let tz = jiff::tz::TimeZone::system();
    let local = if let Ok(dt) = text.parse::<jiff::civil::DateTime>() {
        dt.to_zoned(tz)
    } else if let Ok(date) = text.parse::<jiff::civil::Date>() {
        date.to_zoned(tz)
    } else {
        bail!("invalid --since {text:?}, expected a duration (2d, 3h) or a date (2026-10-30)");
    };
    Ok(local.context("invalid local time")?.timestamp().as_second())
}

pub fn history(
    paths: &Paths,
    app: Option<String>,
    since: Option<&str>,
    limit: usize,
    json: bool,
) -> Result<ExitCode> {
    let since = since.map(|s| parse_since(s, unix_now())).transpose()?;
    let Some(db) = History::open_read_only(&paths.history_db())? else {
        if json {
            println!("[]");
        } else {
            eprintln!("No history yet.");
        }
        return Ok(ExitCode::SUCCESS);
    };
    let mut entries = db.query(&Filter { app, since, limit })?;
    // Oldest first, like a log.
    entries.reverse();
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(ExitCode::SUCCESS);
    }
    if entries.is_empty() {
        eprintln!("No matching messages.");
    }
    for e in &entries {
        println!("{}", describe_entry(e));
    }
    Ok(ExitCode::SUCCESS)
}

/// One line: date, priority, app, title, and the start of the text.
fn describe_entry(e: &Entry) -> String {
    let date = local_datetime(e.date);
    if e.malformed {
        return format!("{date}       (unreadable message)");
    }
    let text = match (&e.message, e.html) {
        (Some(m), true) => crate::notify::html_to_text(m),
        (Some(m), false) => m.clone(),
        (None, _) => String::new(),
    };
    let first_line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut line = format!(
        "{date}  {:>2}  {}: {}",
        e.priority,
        e.app.as_deref().unwrap_or("?"),
        e.display_title()
    );
    if !first_line.is_empty() && first_line != e.display_title() {
        line += " — ";
        line += &truncate(first_line, 80);
    }
    line
}

fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Brief;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert!(parse_duration("90").is_err());
        assert!(parse_duration("0m").is_err());
        assert!(parse_duration("-5m").is_err());
    }

    #[test]
    fn since() {
        assert_eq!(parse_since("2h", 10_000).unwrap(), 10_000 - 7200);
        assert!(parse_since("2026-10-30", 0).is_ok());
        assert!(parse_since("2026-10-30T14:00", 0).is_ok());
        assert!(parse_since("yesterday", 0).is_err());
    }

    #[test]
    fn report_text() {
        let r = Report {
            version: "",
            device: "dev".into(),
            connected_since: None,
            paused: true,
            paused_until: None,
            held_back: 2,
            alerts: vec![Brief {
                id: 1,
                date: 0,
                priority: 2,
                app: "Mon".into(),
                title: "Down".into(),
            }],
            last_message: None,
        };
        let text = format_report(&r, 0);
        assert!(text.contains("disconnected"));
        assert!(text.contains("until resumed (2 message(s) held back)"));
        assert!(text.contains("Emergency:   Mon: Down"));
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate("äöü", 2), "äö…");
        assert_eq!(truncate("abc", 5), "abc");
    }
}
