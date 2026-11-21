//! Agent Fabric (fabric.sql, ADR 0007): a person's runtimes and the sessions
//! those runtimes last reported. Times are unix seconds, passed in by the
//! caller; the store never sees a credential, only its SHA-256.
//!
//! Who sees what (the Scope rule):
//! - `Scope::User(u)`: `u`'s own runtimes and sessions. Anyone else's is
//!   `NotFound`, never a permission error.
//! - `Scope::System`: everything. The daemon uses it to resolve a credential
//!   (before a user is known) and to expire old sessions; the box CLI uses it
//!   to enrol and remove runtimes.
//! - `Scope::Local`: nothing. A fleet is keyed to a signed-in user and LOCAL
//!   mode has none, so lists are empty, by-id reads are `NotFound` and
//!   enrolling is refused.
//! - `Scope::Within`: nothing. A fleet lies in no workspace, so a scope
//!   narrowed to one workspace has no business reading it.
//! - `Scope::Public`: denied (`deny_public`).

mod mail;
#[cfg(test)]
mod work_tests;
mod work;

pub use mail::{FabricConversation, FabricMessage, MAX_UNREAD, MESSAGE_TEXT_TTL, Party};
pub use work::{
    DownKind, FabricAttention, FabricOrder, FabricPairing, FabricProject, FabricSettings, FabricTurn, MAX_OPEN_ATTENTION,
    ORDER_UNDELIVERED, PAIRING_ROW_RETENTION, WORK_RETENTION,
};

use crate::scope::Scope;
use crate::sqlite::SqliteStore;
use crate::{Result, StoreError};
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

/// Sessions are history: a row goes this long after its last update.
pub const SESSION_RETENTION: i64 = 7 * 86400;
/// Live runtimes one person may have enrolled.
pub const MAX_RUNTIMES_PER_USER: usize = 16;
/// Sessions one runtime may hold.
pub const MAX_SESSIONS_PER_RUNTIME: usize = 200;

/// An enrolled runtime. `credential_hash` is never read back out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricRuntime {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
    /// When its stream was last up, as the hub saw it.
    pub last_seen_at: Option<i64>,
    /// The launch that last connected.
    pub instance_id: Option<String>,
    /// The last event the hub took from that launch.
    pub last_up_seq: u64,
    pub version: String,
    /// The last headroom report: a JSON array.
    pub accounts: String,
    pub accounts_at: Option<i64>,
    /// The last RuntimeStatus, as JSON (`{}` before the first).
    pub status: String,
}

/// What the hub knows about a runtime's connection, written whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLink<'a> {
    pub instance_id: Option<&'a str>,
    pub last_up_seq: u64,
    pub version: &'a str,
    pub last_seen_at: Option<i64>,
    pub accounts: &'a str,
    pub accounts_at: Option<i64>,
    pub status: &'a str,
}

/// What a session is doing, as its runtime reports it. `Unknown` is a state
/// this build has no name for (a newer runtime's, or a row a newer build
/// wrote): it is shown, sorts beside working, and never alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Unknown,
    Working,
    NeedsYou,
    Finished,
    Stalled,
    WaitingOnPeer,
    Idle,
    Asleep,
    Exited,
}

impl SessionStatus {
    pub const ALL: [SessionStatus; 9] = [
        SessionStatus::Unknown,
        SessionStatus::Working,
        SessionStatus::NeedsYou,
        SessionStatus::Finished,
        SessionStatus::Stalled,
        SessionStatus::WaitingOnPeer,
        SessionStatus::Idle,
        SessionStatus::Asleep,
        SessionStatus::Exited,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SessionStatus::Unknown => "unknown",
            SessionStatus::Working => "working",
            SessionStatus::NeedsYou => "needs_you",
            SessionStatus::Finished => "finished",
            SessionStatus::Stalled => "stalled",
            SessionStatus::WaitingOnPeer => "waiting_on_peer",
            SessionStatus::Idle => "idle",
            SessionStatus::Asleep => "asleep",
            SessionStatus::Exited => "exited",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// One session as its runtime last reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricSession {
    pub runtime_id: Uuid,
    /// The runtime's own id for it (a terminal pane's UUID).
    pub id: Uuid,
    pub user_id: Uuid,
    pub address: String,
    pub fabric: String,
    pub project: String,
    pub worktree: String,
    pub branch: String,
    pub account: String,
    pub state: SessionStatus,
    pub tool: String,
    pub note: String,
    pub last_line: String,
    pub context_used_pct: Option<u8>,
    pub since: i64,
    pub updated_at: i64,
    /// Snoozed by its person until then (no alert before it). Not the
    /// runtime's to say: writing a session never changes it, only
    /// [`SqliteStore::fabric_session_quiet`] does.
    pub quiet_until: Option<i64>,
    /// Followed by its person: every finished turn alerts. Theirs, like
    /// `quiet_until`: only [`SqliteStore::fabric_session_follow`] writes it.
    pub following: bool,
    /// The named fabric's id; empty for the project's own.
    pub fabric_id: String,
    /// The latest turn's id, running or finished.
    pub turn_id: String,
    /// The queued prompts (order ids), in the order they will be typed.
    pub queue: Vec<String>,
    /// Why the queue is paused; empty when it is not.
    pub queue_paused: String,
}

impl FabricSession {
    /// A session with nothing reported but its identity, for tests and
    /// for building one field by field.
    pub fn blank(runtime_id: Uuid, id: Uuid, user_id: Uuid, address: &str, state: SessionStatus, at: i64) -> Self {
        FabricSession {
            runtime_id,
            id,
            user_id,
            address: address.into(),
            fabric: String::new(),
            project: String::new(),
            worktree: String::new(),
            branch: String::new(),
            account: String::new(),
            state,
            tool: String::new(),
            note: String::new(),
            last_line: String::new(),
            context_used_pct: None,
            since: at,
            updated_at: at,
            quiet_until: None,
            following: false,
            fabric_id: String::new(),
            turn_id: String::new(),
            queue: vec![],
            queue_paused: String::new(),
        }
    }
}

const RUNTIME_COLS: &str =
    "id, user_id, name, created_at, revoked_at, last_seen_at, instance_id, last_up_seq, version, accounts, accounts_at, status";

/// A UUID column. A value that is not one is a corrupt row and an error,
/// never the nil id.
fn uuid_at(r: &Row, col: usize) -> rusqlite::Result<Uuid> {
    let text: String = r.get(col)?;
    Uuid::parse_str(&text).map_err(|e| rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e)))
}

fn runtime_row(r: &Row) -> rusqlite::Result<FabricRuntime> {
    let seq: i64 = r.get(7)?;
    Ok(FabricRuntime {
        id: uuid_at(r, 0)?,
        user_id: uuid_at(r, 1)?,
        name: r.get(2)?,
        created_at: r.get(3)?,
        revoked_at: r.get(4)?,
        last_seen_at: r.get(5)?,
        instance_id: r.get(6)?,
        last_up_seq: seq.max(0) as u64,
        version: r.get(8)?,
        accounts: r.get(9)?,
        accounts_at: r.get(10)?,
        status: r.get(11)?,
    })
}

const SESSION_COLS: &str = "runtime_id, id, user_id, address, fabric, project, worktree, branch, account, state, tool, \
     note, last_line, context_used_pct, since, updated_at, quiet_until, following, fabric_id, turn_id, queue, queue_paused";

fn session_row(r: &Row) -> rusqlite::Result<FabricSession> {
    let state: String = r.get(9)?;
    let pct: Option<i64> = r.get(13)?;
    Ok(FabricSession {
        runtime_id: uuid_at(r, 0)?,
        id: uuid_at(r, 1)?,
        user_id: uuid_at(r, 2)?,
        address: r.get(3)?,
        fabric: r.get(4)?,
        project: r.get(5)?,
        worktree: r.get(6)?,
        branch: r.get(7)?,
        account: r.get(8)?,
        // a state this build does not know (a row from a newer one)
        state: SessionStatus::parse(&state).unwrap_or(SessionStatus::Unknown),
        tool: r.get(10)?,
        note: r.get(11)?,
        last_line: r.get(12)?,
        context_used_pct: pct.map(|p| p.clamp(0, 100) as u8),
        since: r.get(14)?,
        updated_at: r.get(15)?,
        quiet_until: r.get(16)?,
        following: r.get(17)?,
        fabric_id: r.get(18)?,
        turn_id: r.get(19)?,
        // a queue that does not parse is an empty one: it is the runtime's to restate
        queue: serde_json::from_str(&r.get::<_, String>(20)?).unwrap_or_default(),
        queue_paused: r.get(21)?,
    })
}

fn no_runtime(key: impl std::fmt::Display) -> StoreError {
    StoreError::NotFound(format!("runtime {key}"))
}

/// Whose fleet a call may touch.
#[derive(Clone, Copy)]
enum Fleet {
    /// System: every person's.
    All,
    Of(Uuid),
    /// Local and Within: no fleet at all.
    Nobody,
}

impl Fleet {
    /// The `user_id` filter (`?N IS NULL OR user_id = ?N`), or None when the
    /// scope sees no fleet.
    fn filter(self) -> Option<Option<String>> {
        match self {
            Fleet::All => Some(None),
            Fleet::Of(u) => Some(Some(u.to_string())),
            Fleet::Nobody => None,
        }
    }
}

impl SqliteStore {
    fn fleet(&self) -> Result<Fleet> {
        self.deny_public()?;
        Ok(match self.scope {
            Scope::System => Fleet::All,
            Scope::User(u) => Fleet::Of(u),
            Scope::Local | Scope::Within { .. } | Scope::Public => Fleet::Nobody,
        })
    }

    // ---- runtimes ----

    /// Enrol a runtime for `user` under `name` (unique among the person's
    /// live runtimes: a clash is `InvalidOp`). Only the person themselves or
    /// System (the box CLI) may: for anyone else the user does not exist.
    pub fn fabric_runtime_create(&mut self, user: Uuid, name: &str, credential_hash: &str, now: i64) -> Result<FabricRuntime> {
        match self.fleet()? {
            Fleet::All => {}
            Fleet::Of(u) if u == user => {}
            Fleet::Of(_) => return Err(StoreError::NotFound(format!("user {user}"))),
            Fleet::Nobody => return Err(StoreError::InvalidOp("a runtime needs a signed-in user".into())),
        }
        let live: i64 = self.conn.query_row(
            "SELECT count(*) FROM fabric_runtimes WHERE user_id = ?1 AND revoked_at IS NULL",
            [user.to_string()],
            |r| r.get(0),
        )?;
        if live as usize >= MAX_RUNTIMES_PER_USER {
            return Err(StoreError::InvalidOp(format!("at most {MAX_RUNTIMES_PER_USER} runtimes; remove one first")));
        }
        let exists = |sql: &str, p: &[&dyn rusqlite::ToSql]| self.conn.query_row(sql, p, |_| Ok(())).optional().map(|r| r.is_some());
        if !exists("SELECT 1 FROM auth_users WHERE id = ?1", &[&user.to_string()])? {
            return Err(StoreError::NotFound(format!("user {user}")));
        }
        if exists("SELECT 1 FROM fabric_runtimes WHERE user_id = ?1 AND name = ?2 AND revoked_at IS NULL", &[&user.to_string(), &name])? {
            return Err(StoreError::InvalidOp(format!("a live runtime named {name:?} already exists")));
        }
        // a removed runtime's credential stays registered: it is never valid again
        if exists("SELECT 1 FROM fabric_runtimes WHERE credential_hash = ?1", &[&credential_hash])? {
            return Err(StoreError::InvalidOp("that credential is already registered (to a live or a removed runtime): mint a new one".into()));
        }
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO fabric_runtimes (id, user_id, name, credential_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id.to_string(), user.to_string(), name, credential_hash, now],
        )?;
        self.fabric_runtime(id)
    }

    /// The live runtime behind a credential's hash. The daemon's sign-in for
    /// the runtime channel calls this as System, before a user is known: the
    /// hash is the credential.
    pub fn fabric_runtime_by_credential(&self, credential_hash: &str) -> Result<Option<FabricRuntime>> {
        let Some(user) = self.fleet()?.filter() else { return Ok(None) };
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {RUNTIME_COLS} FROM fabric_runtimes
                     WHERE credential_hash = ?1 AND revoked_at IS NULL AND (?2 IS NULL OR user_id = ?2)"
                ),
                params![credential_hash, user],
                runtime_row,
            )
            .optional()?)
    }

    /// The scope's runtimes, oldest first; removed ones only when asked.
    pub fn fabric_runtimes(&self, include_revoked: bool) -> Result<Vec<FabricRuntime>> {
        let Some(user) = self.fleet()?.filter() else { return Ok(vec![]) };
        let mut st = self.conn.prepare(&format!(
            "SELECT {RUNTIME_COLS} FROM fabric_runtimes
             WHERE (?1 IS NULL OR user_id = ?1) AND (?2 OR revoked_at IS NULL) ORDER BY created_at, id"
        ))?;
        let rows = st.query_map(params![user, include_revoked], runtime_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One of the scope's live runtimes; anyone else's is NotFound.
    pub fn fabric_runtime(&self, id: Uuid) -> Result<FabricRuntime> {
        let Some(user) = self.fleet()?.filter() else { return Err(no_runtime(id)) };
        self.conn
            .query_row(
                &format!(
                    "SELECT {RUNTIME_COLS} FROM fabric_runtimes
                     WHERE id = ?1 AND revoked_at IS NULL AND (?2 IS NULL OR user_id = ?2)"
                ),
                params![id.to_string(), user],
                runtime_row,
            )
            .optional()?
            .ok_or_else(|| no_runtime(id))
    }

    /// Remove one of the scope's live runtimes: its credential stops
    /// resolving and its sessions are deleted. Anyone else's, or one already
    /// removed, is NotFound. (Finding a runtime by name is the caller's job.)
    pub fn fabric_runtime_revoke(&mut self, id: Uuid, now: i64) -> Result<FabricRuntime> {
        let r = self.fabric_runtime(id)?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM fabric_sessions WHERE runtime_id = ?1", [id.to_string()])?;
        tx.execute("UPDATE fabric_runtimes SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL", params![id.to_string(), now])?;
        tx.commit()?;
        Ok(FabricRuntime { revoked_at: Some(now), ..r })
    }

    // ---- sessions ----

    /// Every session of the scope's live runtimes.
    pub fn fabric_sessions(&self) -> Result<Vec<FabricSession>> {
        let Some(user) = self.fleet()?.filter() else { return Ok(vec![]) };
        let mut st = self.conn.prepare(&format!(
            "SELECT {SESSION_COLS} FROM fabric_sessions WHERE (?1 IS NULL OR user_id = ?1) ORDER BY runtime_id, id"
        ))?;
        let rows = st.query_map(params![user], session_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Snooze one session of one of the scope's runtimes until `until`
    /// (None = not snoozed), and hand back the row. A session of anyone
    /// else's runtime, or one that is not there, is NotFound.
    pub fn fabric_session_quiet(&mut self, runtime: Uuid, id: Uuid, until: Option<i64>) -> Result<FabricSession> {
        self.fabric_runtime(runtime)?;
        self.conn
            .query_row(
                &format!("UPDATE fabric_sessions SET quiet_until = ?3 WHERE runtime_id = ?1 AND id = ?2 RETURNING {SESSION_COLS}"),
                params![runtime.to_string(), id.to_string(), until],
                session_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("session {id}")))
    }

    /// Follow one session of one of the scope's runtimes (or stop), and hand
    /// back the row. Anyone else's, or one not there, is NotFound.
    pub fn fabric_session_follow(&mut self, runtime: Uuid, id: Uuid, following: bool) -> Result<FabricSession> {
        self.fabric_runtime(runtime)?;
        self.conn
            .query_row(
                &format!("UPDATE fabric_sessions SET following = ?3 WHERE runtime_id = ?1 AND id = ?2 RETURNING {SESSION_COLS}"),
                params![runtime.to_string(), id.to_string(), following],
                session_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("session {id}")))
    }

    /// Lift the snoozes of one of the scope's runtimes that have run out at
    /// `now`, and only those: one set again meanwhile (to a time ahead) is
    /// left alone. Says how many were lifted.
    pub fn fabric_sessions_wake(&mut self, runtime: Uuid, now: i64) -> Result<usize> {
        self.fabric_runtime(runtime)?;
        Ok(self.conn.execute(
            "UPDATE fabric_sessions SET quiet_until = NULL WHERE runtime_id = ?1 AND quiet_until <= ?2",
            params![runtime.to_string(), now],
        )?)
    }

    /// Write sessions of one of the scope's runtimes, each whole (insert or
    /// replace). See [`SqliteStore::fabric_runtime_write`].
    pub fn fabric_sessions_put(&mut self, runtime: Uuid, sessions: &[FabricSession]) -> Result<()> {
        self.fabric_runtime_write(runtime, &[], sessions, None)
    }

    /// Everything the hub writes about one of the scope's runtimes, in one
    /// transaction: sessions to `delete` (it makes room for new ones by
    /// dropping the oldest exited), sessions to `put` (each whole, insert or
    /// replace) and, when given, what it knows of the runtime's connection.
    /// A row's `quiet_until` is not written: a snooze is its person's
    /// ([`SqliteStore::fabric_session_quiet`]) and a new row has none.
    /// Each row's `runtime_id` and `user_id` are ignored: rows are keyed to
    /// `runtime` and to its owner, so a caller can only ever write into a
    /// fleet it can see. A runtime left over [`MAX_SESSIONS_PER_RUNTIME`] is
    /// refused and nothing is written.
    pub fn fabric_runtime_write(
        &mut self,
        runtime: Uuid,
        delete: &[Uuid],
        put: &[FabricSession],
        link: Option<&RuntimeLink>,
    ) -> Result<()> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let tx = self.conn.transaction()?;
        for id in delete {
            tx.execute("DELETE FROM fabric_sessions WHERE runtime_id = ?1 AND id = ?2", params![runtime.to_string(), id.to_string()])?;
        }
        if !put.is_empty() {
            let mut st = tx.prepare(
                "INSERT INTO fabric_sessions (runtime_id, id, user_id, address, fabric, project, worktree, branch,
                     account, state, tool, note, last_line, context_used_pct, since, updated_at,
                     fabric_id, turn_id, queue, queue_paused)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                 ON CONFLICT (runtime_id, id) DO UPDATE SET address = excluded.address, fabric = excluded.fabric,
                     project = excluded.project, worktree = excluded.worktree, branch = excluded.branch,
                     account = excluded.account, state = excluded.state, tool = excluded.tool, note = excluded.note,
                     last_line = excluded.last_line, context_used_pct = excluded.context_used_pct,
                     since = excluded.since, updated_at = excluded.updated_at, fabric_id = excluded.fabric_id,
                     turn_id = excluded.turn_id, queue = excluded.queue, queue_paused = excluded.queue_paused",
            )?;
            for s in put {
                st.execute(params![
                    runtime.to_string(),
                    s.id.to_string(),
                    owner.to_string(),
                    s.address,
                    s.fabric,
                    s.project,
                    s.worktree,
                    s.branch,
                    s.account,
                    s.state.as_str(),
                    s.tool,
                    s.note,
                    s.last_line,
                    s.context_used_pct,
                    s.since,
                    s.updated_at,
                    s.fabric_id,
                    s.turn_id,
                    serde_json::to_string(&s.queue).unwrap_or_else(|_| "[]".into()),
                    s.queue_paused
                ])?;
            }
            drop(st);
            let held: i64 =
                tx.query_row("SELECT count(*) FROM fabric_sessions WHERE runtime_id = ?1", [runtime.to_string()], |r| r.get(0))?;
            if held as usize > MAX_SESSIONS_PER_RUNTIME {
                // dropping the transaction rolls everything back
                return Err(StoreError::InvalidOp(format!("a runtime holds at most {MAX_SESSIONS_PER_RUNTIME} sessions")));
            }
        }
        if let Some(link) = link {
            tx.execute(
                "UPDATE fabric_runtimes SET instance_id = ?2, last_up_seq = ?3, version = ?4, last_seen_at = ?5,
                     accounts = ?6, accounts_at = ?7, status = ?8
                 WHERE id = ?1",
                params![
                    runtime.to_string(),
                    link.instance_id,
                    i64::try_from(link.last_up_seq).unwrap_or(i64::MAX),
                    link.version,
                    link.last_seen_at,
                    link.accounts,
                    link.accounts_at,
                    link.status
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Say that the sessions of one of the scope's runtimes that have not
    /// exited still exist at `now`, so a long-lived session is not expired
    /// while its runtime keeps reporting it.
    pub fn fabric_sessions_touch(&mut self, runtime: Uuid, now: i64) -> Result<usize> {
        self.fabric_runtime(runtime)?;
        Ok(self.conn.execute(
            "UPDATE fabric_sessions SET updated_at = ?2 WHERE runtime_id = ?1 AND state != 'exited' AND updated_at < ?2",
            params![runtime.to_string(), now],
        )?)
    }

    /// Delete every person's sessions last updated before `cutoff` and say
    /// which went: `(user, runtime, session)`. Maintenance: System only.
    pub fn fabric_sessions_expire(&mut self, cutoff: i64) -> Result<Vec<(Uuid, Uuid, Uuid)>> {
        if !matches!(self.fleet()?, Fleet::All) {
            return Err(StoreError::InvalidOp("expiring sessions is maintenance (System scope)".into()));
        }
        let tx = self.conn.transaction()?;
        let gone = {
            let mut st =
                tx.prepare("DELETE FROM fabric_sessions WHERE updated_at < ?1 RETURNING user_id, runtime_id, id")?;
            let rows = st.query_map([cutoff], |r| Ok((uuid_at(r, 0)?, uuid_at(r, 1)?, uuid_at(r, 2)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.commit()?;
        Ok(gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockStore, PrincipalKind};

    fn users(s: &mut SqliteStore) -> (Uuid, Uuid) {
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let tom = s.auth_ensure_owner(p, "tom", 1).unwrap().id;
        let ann = s.auth_add_user("ann", 1).unwrap().id;
        (tom, ann)
    }

    fn session(id: Uuid, address: &str, state: SessionStatus, updated_at: i64) -> FabricSession {
        FabricSession {
            runtime_id: Uuid::nil(),
            id,
            user_id: Uuid::nil(),
            address: address.into(),
            fabric: "taisce".into(),
            project: "taisce".into(),
            worktree: "main".into(),
            branch: "main".into(),
            account: "personal".into(),
            state,
            tool: String::new(),
            note: String::new(),
            last_line: "Shall I go on?".into(),
            context_used_pct: Some(41),
            since: updated_at,
            updated_at,
            quiet_until: None,
            following: false,
            fabric_id: String::new(),
            turn_id: String::new(),
            queue: vec![],
            queue_paused: String::new(),
        }
    }

    fn is_not_found<T: std::fmt::Debug>(r: Result<T>) -> bool {
        matches!(r, Err(StoreError::NotFound(_)))
    }

    #[test]
    fn runtimes_enrol_resolve_and_revoke() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let (tom, _) = users(&mut s);
        let r = s.fabric_runtime_create(tom, "mbp", "h1", 10).unwrap();
        assert_eq!((r.name.as_str(), r.user_id, r.created_at, r.last_seen_at, r.last_up_seq), ("mbp", tom, 10, None, 0));
        assert_eq!(r.accounts, "[]");
        // a live name is taken; so is a registered credential; each says which
        let refused = |r: Result<FabricRuntime>| match r {
            Err(StoreError::InvalidOp(m)) => m,
            other => panic!("{other:?}"),
        };
        assert!(refused(s.fabric_runtime_create(tom, "mbp", "h2", 11)).contains("named \"mbp\""));
        assert!(refused(s.fabric_runtime_create(tom, "other", "h1", 11)).contains("credential is already registered"));
        assert!(is_not_found(s.fabric_runtime_create(Uuid::now_v7(), "ghost", "h9", 11)), "no such user");
        assert_eq!(s.fabric_runtime_by_credential("h1").unwrap().unwrap().id, r.id);
        assert!(s.fabric_runtime_by_credential("nope").unwrap().is_none());
        // the link is written whole and read back
        let link = RuntimeLink {
            instance_id: Some("launch-1"),
            last_up_seq: 42,
            version: "1.4",
            last_seen_at: Some(20),
            accounts: r#"[{"name":"personal"}]"#,
            accounts_at: Some(19),
            status: "{}",
        };
        s.fabric_runtime_write(r.id, &[], &[], Some(&link)).unwrap();
        let got = s.fabric_runtime(r.id).unwrap();
        assert_eq!((got.instance_id.as_deref(), got.last_up_seq, got.version.as_str()), (Some("launch-1"), 42, "1.4"));
        assert_eq!((got.last_seen_at, got.accounts.as_str(), got.accounts_at), (Some(20), r#"[{"name":"personal"}]"#, Some(19)));
        // revoking takes the sessions, stops the credential, frees the name
        s.fabric_sessions_put(r.id, &[session(Uuid::now_v7(), "taisce/main", SessionStatus::Working, 20)]).unwrap();
        assert!(is_not_found(s.fabric_runtime_revoke(Uuid::now_v7(), 30)));
        assert_eq!(s.fabric_runtime_revoke(r.id, 30).unwrap().revoked_at, Some(30));
        assert!(s.fabric_runtime_by_credential("h1").unwrap().is_none());
        assert!(is_not_found(s.fabric_runtime(r.id)));
        assert!(s.fabric_sessions().unwrap().is_empty());
        assert!(is_not_found(s.fabric_runtime_revoke(r.id, 31)), "already removed");
        assert!(s.fabric_runtimes(false).unwrap().is_empty());
        assert_eq!(s.fabric_runtimes(true).unwrap().len(), 1);
        // the name is free again; the removed runtime's credential is not
        assert!(refused(s.fabric_runtime_create(tom, "mbp", "h1", 40)).contains("credential is already registered"));
        s.fabric_runtime_create(tom, "mbp", "h3", 40).unwrap();
    }

    #[test]
    fn a_person_holds_a_bounded_number_of_runtimes() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let (tom, ann) = users(&mut s);
        for i in 0..MAX_RUNTIMES_PER_USER {
            s.fabric_runtime_create(tom, &format!("mac-{i}"), &format!("h{i}"), 1).unwrap();
        }
        assert!(matches!(s.fabric_runtime_create(tom, "one-more", "hx", 1), Err(StoreError::InvalidOp(_))));
        // the cap is per person, and removing one makes room
        s.fabric_runtime_create(ann, "mac-0", "ha", 1).unwrap();
        let first = s.fabric_runtimes(false).unwrap()[0].id;
        s.with_scope_for_test(Scope::User(tom), |s| {
            assert_eq!(s.fabric_runtime_revoke(first, 2).unwrap().user_id, tom);
            s.fabric_runtime_create(tom, "one-more", "hx", 3).unwrap();
        });
        assert_eq!(s.fabric_runtimes(false).unwrap().len(), MAX_RUNTIMES_PER_USER + 1);
    }

    #[test]
    fn sessions_are_written_whole_and_expire() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let (tom, _) = users(&mut s);
        let r = s.fabric_runtime_create(tom, "mbp", "h1", 1).unwrap();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        s.fabric_sessions_put(
            r.id,
            &[session(a, "taisce/main", SessionStatus::Working, 100), session(b, "taisce/main:2", SessionStatus::Exited, 100)],
        )
        .unwrap();
        let got = s.fabric_sessions().unwrap();
        assert_eq!(got.len(), 2);
        // keyed to the runtime and its owner, whatever the rows said
        assert!(got.iter().all(|x| x.runtime_id == r.id && x.user_id == tom));
        let mut next = session(a, "taisce/main", SessionStatus::NeedsYou, 200);
        next.context_used_pct = None;
        s.fabric_sessions_put(r.id, &[next]).unwrap();
        let got = s.fabric_sessions().unwrap();
        let one = got.iter().find(|x| x.id == a).unwrap();
        assert_eq!((one.state, one.context_used_pct, one.updated_at), (SessionStatus::NeedsYou, None, 200));
        // a touch keeps what still exists; an exited session is left to age
        assert_eq!(s.fabric_sessions_touch(r.id, 300).unwrap(), 1);
        let gone = s.fabric_sessions_expire(250).unwrap();
        assert_eq!(gone, vec![(tom, r.id, b)]);
        assert_eq!(s.fabric_sessions().unwrap().len(), 1);
        // deleting by id takes only what is named, and only in this runtime
        let c = Uuid::now_v7();
        s.fabric_sessions_put(r.id, &[session(c, "taisce/gone", SessionStatus::Exited, 400)]).unwrap();
        s.fabric_runtime_write(r.id, &[c, Uuid::now_v7()], &[], None).unwrap();
        assert_eq!(s.fabric_sessions().unwrap().len(), 1);
        // one transaction: a batch that ends over the cap undoes its deletes too
        let many: Vec<FabricSession> =
            (0..=MAX_SESSIONS_PER_RUNTIME).map(|i| session(Uuid::now_v7(), &format!("p/w:{i}"), SessionStatus::Idle, 400)).collect();
        assert!(matches!(s.fabric_runtime_write(r.id, &[a], &many, None), Err(StoreError::InvalidOp(_))));
        assert_eq!(s.fabric_sessions().unwrap().iter().map(|x| x.id).collect::<Vec<_>>(), vec![a]);
        // over the cap: refused whole, nothing of the batch lands
        let many: Vec<FabricSession> =
            (0..MAX_SESSIONS_PER_RUNTIME).map(|i| session(Uuid::now_v7(), &format!("p/w:{i}"), SessionStatus::Idle, 400)).collect();
        assert!(matches!(s.fabric_sessions_put(r.id, &many), Err(StoreError::InvalidOp(_))));
        assert_eq!(s.fabric_sessions().unwrap().len(), 1);
    }

    #[test]
    fn a_fleet_is_its_owners_alone() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let (tom, ann) = users(&mut s);
        let r = s.fabric_runtime_create(tom, "mbp", "h1", 1).unwrap();
        let sid = Uuid::now_v7();
        s.fabric_sessions_put(r.id, &[session(sid, "taisce/main", SessionStatus::Working, 100)]).unwrap();
        let ann_rt = s.fabric_runtime_create(ann, "air", "h2", 1).unwrap();

        s.with_scope_for_test(Scope::User(ann), |s| {
            assert_eq!(s.fabric_runtimes(true).unwrap().iter().map(|r| r.id).collect::<Vec<_>>(), vec![ann_rt.id]);
            assert!(s.fabric_sessions().unwrap().is_empty());
            assert!(is_not_found(s.fabric_runtime(r.id)));
            assert!(s.fabric_runtime_by_credential("h1").unwrap().is_none());
            // every write at Tom's runtime is NotFound, and changes nothing
            assert!(is_not_found(s.fabric_sessions_put(r.id, &[session(sid, "ann/was-here", SessionStatus::Exited, 999)])));
            assert!(is_not_found(s.fabric_sessions_touch(r.id, 999)));
            assert!(is_not_found(s.fabric_runtime_write(r.id, &[sid], &[], None)));
            let link = RuntimeLink { instance_id: None, last_up_seq: 9, version: "x", last_seen_at: None, accounts: "[]", accounts_at: None, status: "{}" };
            assert!(is_not_found(s.fabric_runtime_write(r.id, &[], &[], Some(&link))));
            assert!(is_not_found(s.fabric_runtime_revoke(r.id, 5)));
            assert!(is_not_found(s.fabric_runtime_create(tom, "planted", "h9", 5)));
            assert!(matches!(s.fabric_sessions_expire(i64::MAX), Err(StoreError::InvalidOp(_))));
            // the same session id under her own runtime is her own row
            s.fabric_sessions_put(ann_rt.id, &[session(sid, "ann/own", SessionStatus::Idle, 100)]).unwrap();
        });
        let toms = s.with_scope_for_test(Scope::User(tom), |s| s.fabric_sessions().unwrap());
        assert_eq!(toms.len(), 1);
        assert_eq!((toms[0].address.as_str(), toms[0].state, toms[0].updated_at), ("taisce/main", SessionStatus::Working, 100));
        assert_eq!(s.fabric_runtime(r.id).unwrap().last_up_seq, 0);
    }

    #[test]
    fn a_snooze_is_its_persons_and_a_report_never_changes_it() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let (tom, ann) = users(&mut s);
        let r = s.fabric_runtime_create(tom, "mbp", "h1", 1).unwrap();
        let sid = Uuid::now_v7();
        s.fabric_sessions_put(r.id, &[session(sid, "taisce/main", SessionStatus::Working, 100)]).unwrap();
        assert_eq!(s.fabric_sessions().unwrap()[0].quiet_until, None);
        s.with_scope_for_test(Scope::User(tom), |s| {
            assert_eq!(s.fabric_session_quiet(r.id, sid, Some(5000)).unwrap().quiet_until, Some(5000));
            assert!(is_not_found(s.fabric_session_quiet(r.id, Uuid::now_v7(), Some(5000))), "no such session");
        });
        // the runtime reports it again, whatever its row says: still snoozed
        let mut again = session(sid, "taisce/main", SessionStatus::NeedsYou, 200);
        again.quiet_until = Some(1);
        s.fabric_sessions_put(r.id, &[again]).unwrap();
        let got = s.fabric_sessions().unwrap();
        assert_eq!((got[0].state, got[0].quiet_until), (SessionStatus::NeedsYou, Some(5000)));
        // nobody else can snooze it, wake it or see that it is snoozed
        let ann_rt = s.fabric_runtime_create(ann, "air", "h2", 1).unwrap();
        s.with_scope_for_test(Scope::User(ann), |s| {
            assert!(is_not_found(s.fabric_session_quiet(r.id, sid, None)));
            assert!(is_not_found(s.fabric_session_quiet(r.id, sid, Some(9))));
            // naming her own runtime with Tom's session id reaches nothing
            assert!(is_not_found(s.fabric_session_quiet(ann_rt.id, sid, Some(9))));
            assert!(s.fabric_sessions().unwrap().is_empty());
        });
        for scope in [Scope::Local, Scope::Within { user: tom, workspace: Uuid::now_v7() }, Scope::Public] {
            s.with_scope_for_test(scope, |s| assert!(is_not_found(s.fabric_session_quiet(r.id, sid, None)), "{scope:?}"));
        }
        assert_eq!(s.fabric_sessions().unwrap()[0].quiet_until, Some(5000));
        // a wake lifts only what has run out, and only in the caller's own runtime
        s.with_scope_for_test(Scope::User(ann), |s| assert!(is_not_found(s.fabric_sessions_wake(r.id, i64::MAX))));
        s.with_scope_for_test(Scope::User(tom), |s| assert_eq!(s.fabric_sessions_wake(r.id, 4999).unwrap(), 0));
        assert_eq!(s.fabric_sessions().unwrap()[0].quiet_until, Some(5000));
        s.with_scope_for_test(Scope::User(tom), |s| {
            assert_eq!(s.fabric_sessions_wake(r.id, 5000).unwrap(), 1);
            assert_eq!(s.fabric_session_quiet(r.id, sid, Some(5000)).unwrap().quiet_until, Some(5000));
        });
        s.with_scope_for_test(Scope::User(tom), |s| {
            assert_eq!(s.fabric_session_quiet(r.id, sid, None).unwrap().quiet_until, None);
        });
    }

    #[test]
    fn local_within_and_public_scopes_see_no_fleet() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let (tom, _) = users(&mut s);
        let r = s.fabric_runtime_create(tom, "mbp", "h1", 1).unwrap();
        s.fabric_sessions_put(r.id, &[session(Uuid::now_v7(), "taisce/main", SessionStatus::Working, 100)]).unwrap();
        for scope in [Scope::Local, Scope::Within { user: tom, workspace: Uuid::now_v7() }] {
            s.with_scope_for_test(scope, |s| {
                assert!(s.fabric_runtimes(true).unwrap().is_empty(), "{scope:?}");
                assert!(s.fabric_sessions().unwrap().is_empty(), "{scope:?}");
                assert!(is_not_found(s.fabric_runtime(r.id)), "{scope:?}");
                assert!(s.fabric_runtime_by_credential("h1").unwrap().is_none(), "{scope:?}");
                assert!(matches!(s.fabric_runtime_create(tom, "x", "h2", 1), Err(StoreError::InvalidOp(_))), "{scope:?}");
                assert!(is_not_found(s.fabric_sessions_put(r.id, &[])), "{scope:?}");
            });
        }
        s.with_scope_for_test(Scope::Public, |s| {
            assert!(is_not_found(s.fabric_runtimes(true)));
            assert!(is_not_found(s.fabric_sessions()));
            assert!(is_not_found(s.fabric_runtime(r.id)));
            assert!(is_not_found(s.fabric_runtime_by_credential("h1")));
            assert!(is_not_found(s.fabric_runtime_create(tom, "x", "h2", 1)));
            assert!(is_not_found(s.fabric_sessions_expire(i64::MAX)));
        });
    }

    #[test]
    fn every_status_round_trips() {
        for v in SessionStatus::ALL {
            assert_eq!(SessionStatus::parse(v.as_str()), Some(v));
        }
        assert_eq!(SessionStatus::parse("offline"), None);
        assert_eq!(serde_json::to_string(&SessionStatus::WaitingOnPeer).unwrap(), "\"waiting_on_peer\"");
    }

    #[test]
    fn a_corrupt_id_is_an_error_and_an_unknown_state_is_shown_as_unknown() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let (tom, _) = users(&mut s);
        let r = s.fabric_runtime_create(tom, "mbp", "h1", 1).unwrap();
        let id = Uuid::now_v7();
        s.fabric_sessions_put(r.id, &[session(id, "taisce/main", SessionStatus::Working, 100)]).unwrap();
        s.conn.execute("UPDATE fabric_sessions SET state = 'reticulating'", []).unwrap();
        assert_eq!(s.fabric_sessions().unwrap()[0].state, SessionStatus::Unknown);
        s.conn.execute("UPDATE fabric_sessions SET id = 'not-a-uuid'", []).unwrap();
        assert!(matches!(s.fabric_sessions(), Err(StoreError::Sqlite(_))), "never the nil id");
    }
}
