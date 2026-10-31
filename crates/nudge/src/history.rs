//! Local message history in SQLite.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use nudge_api::{DeviceId, Fetched, MessageId, Receipt};
use rusqlite::{Connection, OpenFlags, params};

const SCHEMA_VERSION: i32 = 1;

pub struct History {
    conn: Connection,
}

/// A stored message as read back for display.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: Option<u64>,
    /// Message date, or the time it was received for unreadable entries.
    pub date: i64,
    pub app: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
    pub priority: i8,
    pub url: Option<String>,
    pub url_title: Option<String>,
    pub html: bool,
    pub acked: bool,
    pub malformed: bool,
}

impl Entry {
    /// Title as shown in notifications: the title, else the app name.
    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .filter(|t| !t.is_empty())
            .or(self.app.as_deref())
            .unwrap_or("")
    }
}

/// Which entries `query` returns, newest first.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Case-insensitive app name.
    pub app: Option<String>,
    /// Unix time; older entries are skipped.
    pub since: Option<i64>,
    pub limit: usize,
}

/// What `store` actually inserted; entries seen before are skipped.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Inserted {
    pub messages: Vec<MessageId>,
    pub malformed: usize,
}

impl History {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // The daemon and the action handler write concurrently.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::init(conn)
    }

    /// Opens an existing database without changing it. `None` if there is
    /// no history yet; a read-only command must not create one.
    pub fn open_read_only(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("cannot open {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => Ok(None),
            SCHEMA_VERSION => Ok(Some(Self { conn })),
            other => bail!("history database has unknown schema version {other}"),
        }
    }

    pub fn query(&self, filter: &Filter) -> Result<Vec<Entry>> {
        let limit = i64::try_from(filter.limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT id, COALESCE(date, received_at), app, title, message, COALESCE(priority, 0),
                    url, url_title, html, acked, malformed
             FROM messages
             WHERE (?1 IS NULL OR app = ?1 COLLATE NOCASE)
               AND (?2 IS NULL OR COALESCE(date, received_at) >= ?2)
             ORDER BY COALESCE(date, received_at) DESC, rowid DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![filter.app, filter.since, limit], |row| {
            Ok(Entry {
                id: row
                    .get::<_, Option<i64>>(0)?
                    .and_then(|id| u64::try_from(id).ok()),
                date: row.get(1)?,
                app: row.get(2)?,
                title: row.get(3)?,
                message: row.get(4)?,
                priority: row.get(5)?,
                url: row.get(6)?,
                url_title: row.get(7)?,
                html: row.get(8)?,
                acked: row.get(9)?,
                malformed: row.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Date of the newest message, for `nudge status` without a daemon.
    pub fn newest(&self) -> Result<Option<Entry>> {
        Ok(self
            .query(&Filter {
                limit: 1,
                ..Filter::default()
            })?
            .into_iter()
            .next())
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                conn.execute_batch(
                    "BEGIN;
                     CREATE TABLE messages (
                         rowid       INTEGER PRIMARY KEY,
                         device_id   TEXT    NOT NULL,
                         id          INTEGER,
                         umid        INTEGER,
                         app         TEXT,
                         title       TEXT,
                         message     TEXT,
                         priority    INTEGER,
                         date        INTEGER,
                         url         TEXT,
                         url_title   TEXT,
                         receipt     TEXT,
                         acked       INTEGER NOT NULL DEFAULT 0,
                         html        INTEGER NOT NULL DEFAULT 0,
                         malformed   INTEGER NOT NULL DEFAULT 0,
                         raw         TEXT    NOT NULL,
                         received_at INTEGER NOT NULL,
                         UNIQUE (device_id, id)
                     );
                     CREATE INDEX messages_date ON messages (date);
                     PRAGMA user_version = 1;
                     COMMIT;",
                )?;
            }
            SCHEMA_VERSION => {}
            other => bail!("history database has unknown schema version {other}"),
        }
        Ok(Self { conn })
    }

    /// Stores all fetched entries in one transaction.
    pub fn store(&mut self, device: &DeviceId, fetched: &Fetched) -> Result<Inserted> {
        let now = unix_now();
        let tx = self.conn.transaction()?;
        let mut inserted = Inserted::default();
        {
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO messages
                     (device_id, id, umid, app, title, message, priority, date,
                      url, url_title, receipt, acked, html, malformed, raw, received_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0, ?14, ?15)",
            )?;
            for m in &fetched.messages {
                let changed = insert.execute(params![
                    device.as_str(),
                    to_sql_id(m.id.0)?,
                    to_sql_id(m.umid)?,
                    m.app,
                    m.title,
                    m.message,
                    m.priority,
                    m.date,
                    m.url,
                    m.url_title,
                    m.receipt.as_ref().map(|r| r.as_str()),
                    m.acked,
                    m.html,
                    serde_json::to_string(m)?,
                    now,
                ])?;
                if changed > 0 {
                    inserted.messages.push(m.id);
                }
            }

            let mut insert_raw = tx.prepare(
                "INSERT OR IGNORE INTO messages (device_id, id, malformed, raw, received_at)
                 VALUES (?1, ?2, 1, ?3, ?4)",
            )?;
            for m in &fetched.malformed {
                let id = m.id.map(|id| to_sql_id(id.0)).transpose()?;
                let changed =
                    insert_raw.execute(params![device.as_str(), id, m.raw.to_string(), now])?;
                inserted.malformed += changed;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Marks every message with this receipt (the original and its retries)
    /// as acknowledged.
    pub fn mark_acked(&mut self, receipt: &Receipt) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE messages SET acked = 1 WHERE receipt = ?1",
            [receipt.as_str()],
        )?)
    }

    /// Deletes messages received more than `keep` ago.
    pub fn prune(&mut self, keep: std::time::Duration) -> Result<usize> {
        let keep = i64::try_from(keep.as_secs()).unwrap_or(i64::MAX);
        let cutoff = unix_now().saturating_sub(keep);
        Ok(self
            .conn
            .execute("DELETE FROM messages WHERE received_at < ?1", [cutoff])?)
    }

    /// Whether the user acknowledged this receipt on this computer.
    pub fn is_acked(&self, receipt: &Receipt) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE receipt = ?1 AND acked = 1)",
            [receipt.as_str()],
            |row| row.get(0),
        )?)
    }
}

fn to_sql_id(id: u64) -> Result<i64> {
    i64::try_from(id).context("message id out of range")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudge_api::Malformed;

    fn fetched() -> Fetched {
        let messages = vec![
            serde_json::from_value(serde_json::json!({"id": 3, "app": "A", "message": "hi"}))
                .unwrap(),
            serde_json::from_value(serde_json::json!({"id": 4, "priority": 2, "receipt": "r"}))
                .unwrap(),
        ];
        Fetched {
            messages,
            malformed: vec![Malformed {
                id: Some(MessageId(5)),
                raw: serde_json::json!({"id": 5, "date": "x"}),
                error: "bad date".into(),
            }],
        }
    }

    #[test]
    fn store_is_idempotent() {
        let mut h = History::in_memory().unwrap();
        let dev = DeviceId::new("dev");

        let first = h.store(&dev, &fetched()).unwrap();
        assert_eq!(first.messages, [MessageId(3), MessageId(4)]);
        assert_eq!(first.malformed, 1);

        let second = h.store(&dev, &fetched()).unwrap();
        assert_eq!(second, Inserted::default());

        let count: i64 = h
            .conn
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn same_id_on_other_device_is_new() {
        let mut h = History::in_memory().unwrap();
        h.store(&DeviceId::new("a"), &fetched()).unwrap();
        let other = h.store(&DeviceId::new("b"), &fetched()).unwrap();
        assert_eq!(other.messages.len(), 2);
    }

    #[test]
    fn acknowledged_receipt() {
        let mut h = History::in_memory().unwrap();
        h.store(&DeviceId::new("dev"), &fetched()).unwrap();
        let r = Receipt::new("r");
        assert!(!h.is_acked(&r).unwrap());
        assert_eq!(h.mark_acked(&r).unwrap(), 1);
        assert!(h.is_acked(&r).unwrap());
        assert!(!h.is_acked(&Receipt::new("other")).unwrap());
    }

    #[test]
    fn prune_removes_old_entries() {
        let mut h = History::in_memory().unwrap();
        h.store(&DeviceId::new("dev"), &fetched()).unwrap();
        h.conn
            .execute(
                "UPDATE messages SET received_at = received_at - 10 * 86400 WHERE id = 3",
                [],
            )
            .unwrap();
        let day = std::time::Duration::from_secs(86_400);
        assert_eq!(h.prune(day * 30).unwrap(), 0);
        assert_eq!(h.prune(day * 5).unwrap(), 1);
        let count: i64 = h
            .conn
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn query_filters_and_orders() {
        let mut h = History::in_memory().unwrap();
        let messages = [(1, "Alpha", 100), (2, "beta", 300), (3, "Alpha", 200)]
            .into_iter()
            .map(|(id, app, date)| {
                serde_json::from_value(serde_json::json!({"id": id, "app": app, "date": date}))
                    .unwrap()
            })
            .collect();
        h.store(
            &DeviceId::new("dev"),
            &Fetched {
                messages,
                malformed: Vec::new(),
            },
        )
        .unwrap();
        let ids = |f: Filter| -> Vec<Option<u64>> {
            h.query(&f).unwrap().into_iter().map(|e| e.id).collect()
        };
        let all = Filter {
            limit: 10,
            ..Filter::default()
        };
        assert_eq!(ids(all.clone()), [Some(2), Some(3), Some(1)]);
        assert_eq!(
            ids(Filter {
                app: Some("alpha".into()),
                ..all.clone()
            }),
            [Some(3), Some(1)]
        );
        assert_eq!(
            ids(Filter {
                since: Some(200),
                ..all.clone()
            }),
            [Some(2), Some(3)]
        );
        assert_eq!(ids(Filter { limit: 1, ..all }), [Some(2)]);
        assert_eq!(h.newest().unwrap().unwrap().display_title(), "beta");
    }

    #[test]
    fn read_only_does_not_create() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        assert!(History::open_read_only(&path).unwrap().is_none());
        assert!(!path.exists());
        History::open(&path).unwrap();
        assert!(History::open_read_only(&path).unwrap().is_some());
    }

    #[test]
    fn raw_json_is_kept() {
        let mut h = History::in_memory().unwrap();
        h.store(&DeviceId::new("dev"), &fetched()).unwrap();
        let raw: String = h
            .conn
            .query_row("SELECT raw FROM messages WHERE id = 4", [], |r| r.get(0))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["receipt"], "r");
    }

    #[test]
    fn reopen_keeps_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        History::open(&path)
            .unwrap()
            .store(&DeviceId::new("d"), &fetched())
            .unwrap();
        let mut again = History::open(&path).unwrap();
        assert_eq!(
            again.store(&DeviceId::new("d"), &fetched()).unwrap(),
            Inserted::default()
        );
    }
}
