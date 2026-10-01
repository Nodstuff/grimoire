//! Push devices (schema.sql, "Push devices"): APNs tokens registered by a
//! user's app, and what the last send to each said. Times are unix seconds,
//! passed in by the caller.

use crate::Result;
use crate::sqlite::SqliteStore;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDevice {
    /// lowercase hex, as APNs addresses it
    pub token: String,
    pub user_id: Uuid,
    /// `ios`
    pub platform: String,
    /// `sandbox` or `production`: which APNs host the token belongs to
    pub env: String,
    pub app_version: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_sent_at: Option<i64>,
    pub last_error: Option<String>,
    pub disabled_at: Option<i64>,
}

const COLS: &str =
    "token, user_id, platform, env, app_version, created_at, updated_at, last_sent_at, last_error, disabled_at";

fn row(r: &rusqlite::Row) -> rusqlite::Result<PushDevice> {
    let user: String = r.get(1)?;
    Ok(PushDevice {
        token: r.get(0)?,
        user_id: Uuid::parse_str(&user).unwrap_or_default(),
        platform: r.get(2)?,
        env: r.get(3)?,
        app_version: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        last_sent_at: r.get(7)?,
        last_error: r.get(8)?,
        disabled_at: r.get(9)?,
    })
}

impl SqliteStore {
    /// Register (or refresh) a token for `user`. Re-registering re-enables a
    /// disabled token and clears its error; a token another user held moves
    /// to `user` (the device signed in as someone else).
    pub fn push_device_upsert(
        &mut self,
        user: Uuid,
        token: &str,
        platform: &str,
        env: &str,
        app_version: &str,
        now: i64,
    ) -> Result<PushDevice> {
        self.conn.execute(
            "INSERT INTO push_devices (token, user_id, platform, env, app_version, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT (token) DO UPDATE SET user_id = excluded.user_id, platform = excluded.platform,
                 env = excluded.env, app_version = excluded.app_version, updated_at = excluded.updated_at,
                 last_error = NULL, disabled_at = NULL",
            params![token, user.to_string(), platform, env, app_version, now],
        )?;
        Ok(self.push_device(token)?.expect("just upserted"))
    }

    pub fn push_device(&self, token: &str) -> Result<Option<PushDevice>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {COLS} FROM push_devices WHERE token = ?1"), [token], row)
            .optional()?)
    }

    /// Remove `user`'s token; false when `user` holds no such token.
    pub fn push_device_delete(&mut self, user: Uuid, token: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM push_devices WHERE token = ?1 AND user_id = ?2",
            params![token, user.to_string()],
        )? > 0)
    }

    pub fn push_devices_for_user(&self, user: Uuid) -> Result<Vec<PushDevice>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {COLS} FROM push_devices WHERE user_id = ?1 ORDER BY created_at, token"))?;
        Ok(st.query_map([user.to_string()], row)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Every token a send should go to (not disabled).
    pub fn push_devices_active(&self) -> Result<Vec<PushDevice>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {COLS} FROM push_devices WHERE disabled_at IS NULL ORDER BY created_at, token"))?;
        Ok(st.query_map([], row)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Record a send's outcome: `error` None = delivered; `disable` stops
    /// further sends until the app registers the token again.
    pub fn push_device_result(&mut self, token: &str, error: Option<&str>, disable: bool, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE push_devices SET
                 last_sent_at = CASE WHEN ?2 IS NULL THEN ?4 ELSE last_sent_at END,
                 last_error = ?2,
                 disabled_at = CASE WHEN ?3 THEN ?4 ELSE disabled_at END
             WHERE token = ?1",
            params![token, error, disable, now],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockStore, PrincipalKind};

    fn users(s: &mut SqliteStore) -> (Uuid, Uuid) {
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let owner = s.auth_ensure_owner(p, "tom", 1).unwrap().id;
        let q = s.create_principal(PrincipalKind::Human, "ann", None).unwrap().id;
        let member = Uuid::now_v7();
        s.conn
            .execute(
                "INSERT INTO auth_users (id, principal_id, name, role, created_at) VALUES (?1, ?2, 'ann', 'member', 1)",
                params![member.to_string(), q.to_string()],
            )
            .unwrap();
        (owner, member)
    }

    #[test]
    fn upsert_delete_scoping_and_disable() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let (tom, ann) = users(&mut s);
        let d = s.push_device_upsert(tom, "aa01", "ios", "sandbox", "1.0 (1)", 10).unwrap();
        assert_eq!((d.env.as_str(), d.created_at, d.updated_at), ("sandbox", 10, 10));
        // a refresh keeps created_at, takes the new env/version
        let d = s.push_device_upsert(tom, "aa01", "ios", "production", "1.1 (2)", 20).unwrap();
        assert_eq!((d.env.as_str(), d.app_version.as_str(), d.created_at, d.updated_at), ("production", "1.1 (2)", 10, 20));
        assert_eq!(s.push_devices_for_user(tom).unwrap().len(), 1);
        // another user cannot delete it
        assert!(!s.push_device_delete(ann, "aa01").unwrap());
        assert_eq!(s.push_devices_active().unwrap().len(), 1);
        // a gone token is disabled, keeps its error, leaves the active set
        s.push_device_result("aa01", Some("410 Unregistered"), true, 30).unwrap();
        let d = s.push_device("aa01").unwrap().unwrap();
        assert_eq!((d.disabled_at, d.last_error.as_deref(), d.last_sent_at), (Some(30), Some("410 Unregistered"), None));
        assert!(s.push_devices_active().unwrap().is_empty());
        // re-registering re-enables; a success stamps last_sent_at, clears the error
        s.push_device_upsert(tom, "aa01", "ios", "production", "1.1 (2)", 40).unwrap();
        s.push_device_result("aa01", None, false, 50).unwrap();
        let d = s.push_device("aa01").unwrap().unwrap();
        assert_eq!((d.disabled_at, d.last_error, d.last_sent_at), (None, None, Some(50)));
        // the same token signed in as another user moves to them
        s.push_device_upsert(ann, "aa01", "ios", "production", "1.1 (2)", 60).unwrap();
        assert!(s.push_devices_for_user(tom).unwrap().is_empty());
        assert!(s.push_device_delete(ann, "aa01").unwrap());
        assert!(s.push_device("aa01").unwrap().is_none());
    }
}
