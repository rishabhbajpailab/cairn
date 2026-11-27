//! Durable local spool (ADR 0014), replacing the CAPTURES and TASKS Queues.
//!
//! Invariant 1: `enqueue` returns only after the row is committed with `synchronous = FULL`,
//! so a 202 means the capture is on disk. Rows leave the spool only when the consumer acks
//! them. Failures back off exponentially. After `max_attempts` a row is marked dead (the DLQ),
//! kept, and never retried automatically.

use async_trait::async_trait;
use cairn_app::ports::{CaptureMsg, PortError, Spool};
use cairn_app::schedule::TaskFire;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::Mutex;
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queue {
    Captures,
    Tasks,
}

impl Queue {
    fn name(self) -> &'static str {
        match self {
            Queue::Captures => "captures",
            Queue::Tasks => "tasks",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "captures" => Some(Queue::Captures),
            "tasks" => Some(Queue::Tasks),
            _ => None,
        }
    }

    /// Same retry budgets as the Workers queues had.
    pub fn max_attempts(self) -> u32 {
        match self {
            Queue::Captures => 10,
            Queue::Tasks => 3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub id: i64,
    pub queue: Queue,
    pub body: String,
    pub attempts: u32,
}

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct Counts {
    pub pending: i64,
    pub dead: i64,
}

pub struct SqliteSpool {
    conn: Mutex<Connection>,
    /// Wakes the consumer when something is enqueued.
    pub ready: Notify,
}

fn err(e: impl ToString) -> PortError {
    PortError(e.to_string())
}

/// 2^attempts seconds, capped at 10 minutes.
pub fn backoff(attempts: u32) -> Duration {
    Duration::seconds(1i64 << attempts.min(10)).min(Duration::minutes(10))
}

impl SqliteSpool {
    pub fn open(path: &Path) -> Result<Self, PortError> {
        let c = Connection::open(path).map_err(err)?;
        c.pragma_update(None, "journal_mode", "WAL").map_err(err)?;
        c.pragma_update(None, "synchronous", "FULL").map_err(err)?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS spool (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 queue      TEXT NOT NULL,
                 body       TEXT NOT NULL,
                 attempts   INTEGER NOT NULL DEFAULT 0,
                 not_before INTEGER NOT NULL,          -- unix ms
                 dead       INTEGER NOT NULL DEFAULT 0,
                 last_error TEXT,
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS spool_due ON spool (dead, not_before);
             CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        )
        .map_err(err)?;
        Ok(Self { conn: Mutex::new(c), ready: Notify::new() })
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, PortError> {
        self.conn.lock().map_err(|_| PortError("spool mutex poisoned".into()))
    }

    pub fn push(&self, queue: Queue, body: &str, now: DateTime<Utc>) -> Result<i64, PortError> {
        let ms = now.timestamp_millis();
        let c = self.conn()?;
        c.execute(
            "INSERT INTO spool (queue, body, not_before, created_at) VALUES (?1, ?2, ?3, ?3)",
            params![queue.name(), body, ms],
        )
        .map_err(err)?;
        let id = c.last_insert_rowid();
        drop(c);
        self.ready.notify_one();
        Ok(id)
    }

    pub fn push_task(&self, fire: &TaskFire, now: DateTime<Utc>) -> Result<i64, PortError> {
        self.push(Queue::Tasks, &serde_json::to_string(fire).map_err(err)?, now)
    }

    /// Up to `limit` live rows whose retry time has come, oldest first.
    pub fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Row>, PortError> {
        let c = self.conn()?;
        let mut st = c
            .prepare(
                "SELECT id, queue, body, attempts FROM spool WHERE dead = 0 AND not_before <= ?1 \
                 ORDER BY not_before, id LIMIT ?2",
            )
            .map_err(err)?;
        let rows = st
            .query_map(params![now.timestamp_millis(), limit as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, u32>(3)?))
            })
            .map_err(err)?;
        let mut out = Vec::new();
        for r in rows {
            let (id, q, body, attempts) = r.map_err(err)?;
            match Queue::parse(&q) {
                Some(queue) => out.push(Row { id, queue, body, attempts }),
                None => tracing::error!(id, queue = %q, "unknown spool queue; leaving row alone"),
            }
        }
        Ok(out)
    }

    /// Done: the row leaves the spool.
    pub fn ack(&self, id: i64) -> Result<(), PortError> {
        self.conn()?.execute("DELETE FROM spool WHERE id = ?1", params![id]).map_err(err)?;
        Ok(())
    }

    /// Failed: retry later, or mark dead once the queue's budget is spent. Returns true if dead.
    pub fn fail(&self, row: &Row, error: &str, now: DateTime<Utc>) -> Result<bool, PortError> {
        let attempts = row.attempts + 1;
        let dead = attempts >= row.queue.max_attempts();
        let next = (now + backoff(attempts)).timestamp_millis();
        self.conn()?
            .execute(
                "UPDATE spool SET attempts = ?2, not_before = ?3, dead = ?4, last_error = ?5 WHERE id = ?1",
                params![row.id, attempts, next, dead as i64, error],
            )
            .map_err(err)?;
        Ok(dead)
    }

    /// Undecodable forever: straight to dead, kept for inspection.
    pub fn bury(&self, row: &Row, error: &str) -> Result<(), PortError> {
        self.conn()?
            .execute("UPDATE spool SET dead = 1, last_error = ?2 WHERE id = ?1", params![row.id, error])
            .map_err(err)?;
        Ok(())
    }

    /// When the earliest live row becomes due, if any.
    pub fn next_due(&self) -> Result<Option<DateTime<Utc>>, PortError> {
        let ms: Option<i64> = self
            .conn()?
            .query_row("SELECT MIN(not_before) FROM spool WHERE dead = 0", [], |r| r.get(0))
            .map_err(err)?;
        Ok(ms.and_then(DateTime::from_timestamp_millis))
    }

    pub fn counts(&self) -> Result<Counts, PortError> {
        self.conn()?
            .query_row("SELECT COALESCE(SUM(dead = 0), 0), COALESCE(SUM(dead = 1), 0) FROM spool", [], |r| {
                Ok(Counts { pending: r.get(0)?, dead: r.get(1)? })
            })
            .map_err(err)
    }

    /// Put dead rows back in the queue (operator action: `cairn spool retry-dead`).
    pub fn revive_dead(&self, now: DateTime<Utc>) -> Result<usize, PortError> {
        self.conn()?
            .execute(
                "UPDATE spool SET dead = 0, attempts = 0, not_before = ?1 WHERE dead = 1",
                params![now.timestamp_millis()],
            )
            .map_err(err)
    }

    pub fn get(&self, key: &str) -> Result<Option<String>, PortError> {
        self.conn()?.query_row("SELECT v FROM kv WHERE k = ?1", params![key], |r| r.get(0)).optional().map_err(err)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<(), PortError> {
        self.conn()?
            .execute(
                "INSERT INTO kv (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                params![key, value],
            )
            .map_err(err)?;
        Ok(())
    }
}

/// The app's capture port: commit to disk, then wake the consumer.
pub struct SpoolPort(pub std::sync::Arc<SqliteSpool>);

#[async_trait]
impl Spool for SpoolPort {
    async fn enqueue(&self, msg: &CaptureMsg) -> Result<(), PortError> {
        let body = serde_json::to_string(msg).map_err(err)?;
        self.0.push(Queue::Captures, &body, Utc::now()).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("cairn-spool-{name}-{}.sqlite", std::process::id()));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
        }
        p
    }

    #[test]
    fn survives_reopen_and_acks_remove() {
        let p = tmp("reopen");
        let now = Utc::now();
        {
            let s = SqliteSpool::open(&p).unwrap();
            s.push(Queue::Captures, "{\"a\":1}", now).unwrap();
            s.push(Queue::Tasks, "{\"b\":2}", now).unwrap();
        }
        let s = SqliteSpool::open(&p).unwrap();
        let due = s.due(now, 10).unwrap();
        assert_eq!(due.len(), 2, "rows survive a restart");
        assert_eq!(due[0].queue, Queue::Captures);
        s.ack(due[0].id).unwrap();
        assert_eq!(s.counts().unwrap(), Counts { pending: 1, dead: 0 });
    }

    #[test]
    fn failures_back_off_then_go_dead() {
        let p = tmp("dead");
        let s = SqliteSpool::open(&p).unwrap();
        let mut now = Utc::now();
        s.push(Queue::Tasks, "{}", now).unwrap();
        for attempt in 1..=Queue::Tasks.max_attempts() {
            let row = s.due(now, 1).unwrap().pop().expect("due");
            assert!(s.due(now, 10).unwrap().len() == 1);
            let dead = s.fail(&row, "boom", now).unwrap();
            assert_eq!(dead, attempt == Queue::Tasks.max_attempts());
            assert!(s.due(now, 10).unwrap().is_empty(), "not retried before its backoff");
            now += backoff(attempt) + Duration::milliseconds(1);
        }
        assert!(s.due(now + Duration::days(1), 10).unwrap().is_empty(), "dead rows are never retried");
        assert_eq!(s.counts().unwrap(), Counts { pending: 0, dead: 1 });
        assert_eq!(s.revive_dead(now).unwrap(), 1);
        assert_eq!(s.due(now, 10).unwrap().len(), 1);
    }

    #[test]
    fn next_due_and_kv() {
        let p = tmp("kv");
        let s = SqliteSpool::open(&p).unwrap();
        assert_eq!(s.next_due().unwrap(), None);
        let now = Utc::now();
        s.push(Queue::Captures, "{}", now).unwrap();
        assert!(s.next_due().unwrap().is_some());
        assert_eq!(s.get("last_tick").unwrap(), None);
        s.set("last_tick", "x").unwrap();
        s.set("last_tick", "y").unwrap();
        assert_eq!(s.get("last_tick").unwrap().as_deref(), Some("y"));
    }

    #[test]
    fn backoff_is_capped() {
        assert_eq!(backoff(1), Duration::seconds(2));
        assert_eq!(backoff(30), Duration::minutes(10));
    }
}
