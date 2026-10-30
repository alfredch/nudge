//! Local message history in SQLite.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use nudge_api::{DeviceId, Fetched, MessageId};
use rusqlite::{Connection, params};

const SCHEMA_VERSION: i32 = 1;

pub struct History {
    conn: Connection,
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
        Self::init(conn)
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
