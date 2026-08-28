use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("omarchycast")
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub launches: u32,
    pub last_used: i64,
}

/// Launch counters, persisted to SQLite but mirrored in memory so the query path
/// never touches the database.
pub struct Store {
    conn: Mutex<Connection>,
    cache: RwLock<HashMap<String, Usage>>,
}

impl Store {
    pub fn open() -> Result<Self> {
        let dir = data_dir();
        crate::safeio::ensure_private_dir(&dir)?;
        let conn = Connection::open(dir.join("omarchycast.db"))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS usage (
                 key       TEXT PRIMARY KEY,
                 launches  INTEGER NOT NULL DEFAULT 0,
                 last_used INTEGER NOT NULL DEFAULT 0
             );",
        )?;

        let cache = {
            let mut stmt = conn.prepare("SELECT key, launches, last_used FROM usage")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Usage { launches: r.get::<_, i64>(1)? as u32, last_used: r.get(2)? },
                ))
            })?;
            rows.filter_map(|r| r.ok()).collect::<HashMap<_, _>>()
        };

        Ok(Store { conn: Mutex::new(conn), cache: RwLock::new(cache) })
    }

    /// In-memory read used on every keystroke.
    pub fn usage(&self, key: &str) -> Usage {
        self.cache.read().map(|c| c.get(key).copied().unwrap_or_default()).unwrap_or_default()
    }

    pub fn record_launch(&self, key: &str) {
        let now = now_unix();
        if let Ok(mut cache) = self.cache.write() {
            let entry = cache.entry(key.to_string()).or_default();
            entry.launches += 1;
            entry.last_used = now;
        }
        // A failed write only costs ranking quality, so it must never break the launch.
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO usage (key, launches, last_used) VALUES (?1, 1, ?2)
                 ON CONFLICT(key) DO UPDATE SET launches = launches + 1, last_used = ?2",
                rusqlite::params![key, now],
            );
        }
    }
}
