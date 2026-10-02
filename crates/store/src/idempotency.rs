//! Durable retry dedupe (schema.sql, "Idempotency"): the first outcome of a
//! write, keyed by (principal, key), so a retry after a restart still replays
//! it. The store keeps rows; callers decide the window (`since`) and the
//! sweep horizon. Times are unix seconds, passed in by the caller.

use crate::Result;
use crate::sqlite::SqliteStore;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

impl SqliteStore {
    /// The recorded outcome for (principal, key), if recorded at or after `since`.
    pub fn idempotency_get(&self, principal: Uuid, key: Uuid, since: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT response FROM idempotency WHERE principal = ?1 AND key = ?2 AND created_at >= ?3",
                params![principal.to_string(), key.to_string(), since],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Record an outcome. A live row is kept (the first outcome wins); one
    /// already past every window is replaced.
    pub fn idempotency_put(&mut self, principal: Uuid, key: Uuid, response: &str, now: i64, horizon: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO idempotency (principal, key, response, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (principal, key) DO UPDATE SET response = excluded.response, created_at = excluded.created_at
             WHERE idempotency.created_at < ?5",
            params![principal.to_string(), key.to_string(), response, now, now - horizon],
        )?;
        Ok(())
    }

    /// Delete rows recorded before `before`.
    pub fn idempotency_cleanup(&mut self, before: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM idempotency WHERE created_at < ?1", [before])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_outcome_wins_windows_and_sweep() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let (p, q, k) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        s.idempotency_put(p, k, "first", 100, 50).unwrap();
        s.idempotency_put(p, k, "second", 110, 50).unwrap();
        assert_eq!(s.idempotency_get(p, k, 0).unwrap().as_deref(), Some("first"), "a live row is kept");
        assert_eq!(s.idempotency_get(q, k, 0).unwrap(), None, "keyed by principal");
        assert_eq!(s.idempotency_get(p, k, 101).unwrap(), None, "outside the caller's window");
        s.idempotency_put(p, k, "late", 200, 50).unwrap();
        assert_eq!(s.idempotency_get(p, k, 0).unwrap().as_deref(), Some("late"), "a dead row is replaced");
        assert_eq!(s.idempotency_cleanup(201).unwrap(), 1);
        assert_eq!(s.idempotency_get(p, k, 0).unwrap(), None);
    }

    #[test]
    fn survives_reopen() {
        let dir = std::env::temp_dir().join(format!("taisce-idem-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("ks.db");
        let (p, k) = (Uuid::now_v7(), Uuid::now_v7());
        SqliteStore::open(&db).unwrap().idempotency_put(p, k, "kept", 100, 50).unwrap();
        assert_eq!(SqliteStore::open(&db).unwrap().idempotency_get(p, k, 0).unwrap().as_deref(), Some("kept"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
