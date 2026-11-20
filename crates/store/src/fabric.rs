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
}

/// What a session is doing, as its runtime reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionStatus {
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
    pub const ALL: [SessionStatus; 8] = [
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
}

const RUNTIME_COLS: &str =
    "id, user_id, name, created_at, revoked_at, last_seen_at, instance_id, last_up_seq, version, accounts, accounts_at";

fn runtime_row(r: &Row) -> rusqlite::Result<FabricRuntime> {
    let (id, user): (String, String) = (r.get(0)?, r.get(1)?);
    let seq: i64 = r.get(7)?;
    Ok(FabricRuntime {
        id: Uuid::parse_str(&id).unwrap_or_default(),
        user_id: Uuid::parse_str(&user).unwrap_or_default(),
        name: r.get(2)?,
        created_at: r.get(3)?,
        revoked_at: r.get(4)?,
        last_seen_at: r.get(5)?,
        instance_id: r.get(6)?,
        last_up_seq: seq.max(0) as u64,
        version: r.get(8)?,
        accounts: r.get(9)?,
        accounts_at: r.get(10)?,
    })
}

const SESSION_COLS: &str = "runtime_id, id, user_id, address, fabric, project, worktree, branch, account, state, tool, \
     note, last_line, context_used_pct, since, updated_at";

fn session_row(r: &Row) -> rusqlite::Result<FabricSession> {
    let (runtime, id, user): (String, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
    let state: String = r.get(9)?;
    let pct: Option<i64> = r.get(13)?;
    Ok(FabricSession {
        runtime_id: Uuid::parse_str(&runtime).unwrap_or_default(),
        id: Uuid::parse_str(&id).unwrap_or_default(),
        user_id: Uuid::parse_str(&user).unwrap_or_default(),
        address: r.get(3)?,
        fabric: r.get(4)?,
        project: r.get(5)?,
        worktree: r.get(6)?,
        branch: r.get(7)?,
        account: r.get(8)?,
        // a state this build does not know (a row from a newer one) reads as
        // exited: it is history either way
        state: SessionStatus::parse(&state).unwrap_or(SessionStatus::Exited),
        tool: r.get(10)?,
        note: r.get(11)?,
        last_line: r.get(12)?,
        context_used_pct: pct.map(|p| p.clamp(0, 100) as u8),
        since: r.get(14)?,
        updated_at: r.get(15)?,
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
        let id = Uuid::now_v7();
        let r = self.conn.execute(
            "INSERT INTO fabric_runtimes (id, user_id, name, credential_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id.to_string(), user.to_string(), name, credential_hash, now],
        );
        match r {
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                return Err(StoreError::InvalidOp(format!(
                    "a runtime named {name:?} already exists (or the credential is already registered, or the user is unknown)"
                )));
            }
            r => r?,
        };
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

    /// Remove one of the scope's live runtimes by name, id, or a unique id
    /// prefix: its credential stops resolving and its sessions are deleted.
    /// None when nothing (or more than one runtime) matches.
    pub fn fabric_runtime_revoke(&mut self, key: &str, now: i64) -> Result<Option<FabricRuntime>> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(None);
        }
        let live = self.fabric_runtimes(false)?;
        let by_name: Vec<&FabricRuntime> = live.iter().filter(|r| r.name == key).collect();
        let hits: Vec<&FabricRuntime> =
            if by_name.is_empty() { live.iter().filter(|r| r.id.to_string().starts_with(key)).collect() } else { by_name };
        let [r] = hits.as_slice() else {
            return Ok(None);
        };
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM fabric_sessions WHERE runtime_id = ?1", [r.id.to_string()])?;
        tx.execute(
            "UPDATE fabric_runtimes SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![r.id.to_string(), now],
        )?;
        tx.commit()?;
        Ok(Some(FabricRuntime { revoked_at: Some(now), ..(*r).clone() }))
    }

    /// Record what the hub knows about one of the scope's runtimes'
    /// connection.
    pub fn fabric_runtime_link(&mut self, id: Uuid, link: &RuntimeLink) -> Result<()> {
        self.fabric_runtime(id)?;
        self.conn.execute(
            "UPDATE fabric_runtimes SET instance_id = ?2, last_up_seq = ?3, version = ?4, last_seen_at = ?5,
                 accounts = ?6, accounts_at = ?7
             WHERE id = ?1",
            params![
                id.to_string(),
                link.instance_id,
                i64::try_from(link.last_up_seq).unwrap_or(i64::MAX),
                link.version,
                link.last_seen_at,
                link.accounts,
                link.accounts_at
            ],
        )?;
        Ok(())
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

    /// Write sessions of one of the scope's runtimes, each whole (insert or
    /// replace), in one transaction. Each row's `runtime_id` and `user_id`
    /// are ignored: the rows are keyed to `runtime` and to its owner, so a
    /// caller can only ever write into a fleet it can see. A runtime over
    /// [`MAX_SESSIONS_PER_RUNTIME`] is refused and nothing is written.
    pub fn fabric_sessions_put(&mut self, runtime: Uuid, sessions: &[FabricSession]) -> Result<()> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare(
                "INSERT INTO fabric_sessions (runtime_id, id, user_id, address, fabric, project, worktree, branch,
                     account, state, tool, note, last_line, context_used_pct, since, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT (runtime_id, id) DO UPDATE SET address = excluded.address, fabric = excluded.fabric,
                     project = excluded.project, worktree = excluded.worktree, branch = excluded.branch,
                     account = excluded.account, state = excluded.state, tool = excluded.tool, note = excluded.note,
                     last_line = excluded.last_line, context_used_pct = excluded.context_used_pct,
                     since = excluded.since, updated_at = excluded.updated_at",
            )?;
            for s in sessions {
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
                    s.updated_at
                ])?;
            }
        }
        let held: i64 =
            tx.query_row("SELECT count(*) FROM fabric_sessions WHERE runtime_id = ?1", [runtime.to_string()], |r| r.get(0))?;
        if held as usize > MAX_SESSIONS_PER_RUNTIME {
            // dropping the transaction rolls the batch back
            return Err(StoreError::InvalidOp(format!("a runtime holds at most {MAX_SESSIONS_PER_RUNTIME} sessions")));
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete sessions of one of the scope's runtimes by id (the hub makes
    /// room for new sessions by dropping the oldest exited ones).
    pub fn fabric_sessions_delete(&mut self, runtime: Uuid, ids: &[Uuid]) -> Result<usize> {
        self.fabric_runtime(runtime)?;
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for id in ids {
            n += tx.execute("DELETE FROM fabric_sessions WHERE runtime_id = ?1 AND id = ?2", params![runtime.to_string(), id.to_string()])?;
        }
        tx.commit()?;
        Ok(n)
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
            let rows = st.query_map([cutoff], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.commit()?;
        let id = |s: &str| Uuid::parse_str(s).unwrap_or_default();
        Ok(gone.iter().map(|(u, r, s)| (id(u), id(r), id(s))).collect())
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
        // a live name is taken; so is a registered credential
        assert!(matches!(s.fabric_runtime_create(tom, "mbp", "h2", 11), Err(StoreError::InvalidOp(_))));
        assert!(matches!(s.fabric_runtime_create(tom, "other", "h1", 11), Err(StoreError::InvalidOp(_))));
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
        };
        s.fabric_runtime_link(r.id, &link).unwrap();
        let got = s.fabric_runtime(r.id).unwrap();
        assert_eq!((got.instance_id.as_deref(), got.last_up_seq, got.version.as_str()), (Some("launch-1"), 42, "1.4"));
        assert_eq!((got.last_seen_at, got.accounts.as_str(), got.accounts_at), (Some(20), r#"[{"name":"personal"}]"#, Some(19)));
        // revoking takes the sessions, stops the credential, frees the name
        s.fabric_sessions_put(r.id, &[session(Uuid::now_v7(), "taisce/main", SessionStatus::Working, 20)]).unwrap();
        assert!(s.fabric_runtime_revoke("nobody", 30).unwrap().is_none());
        assert_eq!(s.fabric_runtime_revoke("mbp", 30).unwrap().unwrap().revoked_at, Some(30));
        assert!(s.fabric_runtime_by_credential("h1").unwrap().is_none());
        assert!(is_not_found(s.fabric_runtime(r.id)));
        assert!(s.fabric_sessions().unwrap().is_empty());
        assert!(s.fabric_runtime_revoke("mbp", 31).unwrap().is_none(), "already removed");
        assert!(s.fabric_runtimes(false).unwrap().is_empty());
        assert_eq!(s.fabric_runtimes(true).unwrap().len(), 1);
        let again = s.fabric_runtime_create(tom, "mbp", "h3", 40).unwrap();
        // by a unique id prefix too
        let prefix = &again.id.to_string()[..30];
        assert_eq!(s.fabric_runtime_revoke(prefix, 50).unwrap().unwrap().id, again.id);
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
        // System sees both people's `mac-0`: an ambiguous name removes nothing
        assert!(s.fabric_runtime_revoke("mac-0", 2).unwrap().is_none());
        s.with_scope_for_test(Scope::User(tom), |s| {
            assert_eq!(s.fabric_runtime_revoke("mac-0", 2).unwrap().unwrap().user_id, tom);
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
        assert_eq!(s.fabric_sessions_delete(r.id, &[c, Uuid::now_v7()]).unwrap(), 1);
        assert_eq!(s.fabric_sessions().unwrap().len(), 1);
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
            assert!(is_not_found(s.fabric_sessions_delete(r.id, &[sid])));
            let link = RuntimeLink { instance_id: None, last_up_seq: 9, version: "x", last_seen_at: None, accounts: "[]", accounts_at: None };
            assert!(is_not_found(s.fabric_runtime_link(r.id, &link)));
            assert!(s.fabric_runtime_revoke("mbp", 5).unwrap().is_none());
            assert!(s.fabric_runtime_revoke(&r.id.to_string(), 5).unwrap().is_none());
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
    }
}
