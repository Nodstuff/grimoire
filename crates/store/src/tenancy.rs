//! Multi-user tenancy (ADR 0004): who sees and who may write what.
//!
//! A doc's *space* is resolved from its ancestor chain: the nearest labelled
//! ancestor (itself included) gives its workspace; with no label anywhere up
//! the chain it sits in the Unsorted of its root's owner (`docs.owner_id` of
//! the top ancestor, NULL = the instance owner). A user sees a doc iff they
//! are a member of its workspace, or it is in their own Unsorted.
//!
//! Everything here is the ONE definition of that rule: [`vis_pred`] for list
//! queries (a SQL predicate) and [`space_conn`] + [`access_conn`] for by-id
//! checks. Both walk the same chain, so they agree.

use crate::scope::{Role, Scope};
use crate::sqlite::SqliteStore;
use crate::{Result, StoreError};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// The instance owner: the first `owner`-role user (`auth enroll`'s user).
pub(crate) const INSTANCE_OWNER_SQL: &str =
    "(SELECT id FROM auth_users WHERE role = 'owner' ORDER BY created_at, id LIMIT 1)";

/// The ids of every doc (live or tombstoned) `user` can see, as a subquery.
/// `within` narrows it to one workspace. The ids are formatted UUIDs, so
/// inlining them as literals is injection-safe; it also keeps the statement
/// cache keyed per user.
pub(crate) fn vis_subquery(user: Uuid, within: Option<Uuid>) -> String {
    let u = user.hyphenated().to_string();
    let rule = match within {
        Some(w) => {
            let w = w.hyphenated().to_string();
            format!(
                "r.ws = '{w}' AND (r.ws IN (SELECT workspace_id FROM workspace_members WHERE user_id = '{u}')
                     OR r.ws IN (SELECT id FROM workspaces WHERE owner_id IS NULL AND '{u}' = {INSTANCE_OWNER_SQL}))"
            )
        }
        None => format!(
            "(r.ws IS NULL AND COALESCE(r.owner, {INSTANCE_OWNER_SQL}) = '{u}')
             OR r.ws IN (SELECT workspace_id FROM workspace_members WHERE user_id = '{u}')
             OR r.ws IN (SELECT id FROM workspaces WHERE owner_id IS NULL AND '{u}' = {INSTANCE_OWNER_SQL})"
        ),
    };
    format!(
        "WITH RECURSIVE r(id, ws, owner) AS (
             SELECT d.id, l.workspace_id, d.owner_id FROM docs d
             LEFT JOIN doc_workspace l ON l.doc_id = d.id
             WHERE d.parent_id IS NULL OR NOT EXISTS (SELECT 1 FROM docs p WHERE p.id = d.parent_id)
             UNION ALL
             SELECT d.id, COALESCE(l.workspace_id, r.ws), r.owner FROM docs d
             JOIN r ON d.parent_id = r.id
             LEFT JOIN doc_workspace l ON l.doc_id = d.id)
         SELECT r.id FROM r WHERE {rule}"
    )
}

/// The visible-docs subquery for a filtered scope (None: System/Local).
pub(crate) fn vis_sub(scope: Scope) -> Option<String> {
    match scope {
        Scope::System | Scope::Local => None,
        Scope::User(u) => Some(vis_subquery(u, None)),
        Scope::Within { user, workspace } => Some(vis_subquery(user, Some(workspace))),
    }
}

/// A predicate keeping rows whose doc id column `col` the scope can see.
/// System and Local add nothing (`1`), so LOCAL mode runs the SQL it always
/// ran.
pub(crate) fn vis_pred(scope: Scope, col: &str) -> String {
    match vis_sub(scope) {
        None => "1".into(),
        Some(sub) => format!("{col} IN ({sub})"),
    }
}

/// Where a doc lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Space {
    Workspace(Uuid),
    /// Someone's Unsorted (None: a database with no users at all).
    Unsorted(Option<Uuid>),
}

fn parse(s: String, what: &str) -> Result<Uuid> {
    Uuid::parse_str(&s).map_err(|_| StoreError::InvalidOp(format!("bad uuid in {what}: {s}")))
}

pub(crate) fn instance_owner_conn(conn: &Connection) -> Result<Option<Uuid>> {
    let id: Option<String> = conn
        .query_row(&format!("SELECT {INSTANCE_OWNER_SQL}"), [], |r| r.get(0))
        .optional()?
        .flatten();
    id.map(|s| parse(s, "auth_users.id")).transpose()
}

/// The space of `doc` (live or tombstoned). NotFound when there is no row.
pub(crate) fn space_conn(conn: &Connection, doc: Uuid) -> Result<Space> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .prepare_cached(
            "WITH RECURSIVE up(id, parent_id, owner, depth) AS (
                 SELECT id, parent_id, owner_id, 0 FROM docs WHERE id = ?1
                 UNION ALL
                 SELECT d.id, d.parent_id, d.owner_id, up.depth + 1 FROM docs d JOIN up ON d.id = up.parent_id
                 WHERE up.depth < 256)
             SELECT (SELECT l.workspace_id FROM up JOIN doc_workspace l ON l.doc_id = up.id ORDER BY up.depth LIMIT 1),
                    (SELECT owner FROM up ORDER BY depth DESC LIMIT 1)
             WHERE EXISTS (SELECT 1 FROM up)",
        )?
        .query_row(params![doc.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((ws, owner)) = row else {
        return Err(StoreError::NotFound(format!("doc {doc}")));
    };
    match ws {
        Some(w) => Ok(Space::Workspace(parse(w, "doc_workspace.workspace_id")?)),
        None => match owner {
            Some(o) => Ok(Space::Unsorted(Some(parse(o, "docs.owner_id")?))),
            None => Ok(Space::Unsorted(instance_owner_conn(conn)?)),
        },
    }
}

/// The space a new root doc created in `scope` lands in.
pub(crate) fn own_unsorted(conn: &Connection, scope: Scope) -> Result<Space> {
    Ok(match scope.user() {
        Some(u) => Space::Unsorted(Some(u)),
        None => Space::Unsorted(instance_owner_conn(conn)?),
    })
}

/// A user's role in a workspace: their member row, or `owner` of a
/// workspace with no owner recorded when they are the instance owner.
pub(crate) fn member_role_conn(conn: &Connection, ws: Uuid, user: Uuid) -> Result<Option<Role>> {
    let role: Option<String> = conn
        .prepare_cached("SELECT role FROM workspace_members WHERE workspace_id = ?1 AND user_id = ?2")?
        .query_row(params![ws.to_string(), user.to_string()], |r| r.get(0))
        .optional()?;
    if let Some(r) = role {
        return Ok(Role::parse(&r));
    }
    let unowned: Option<Option<String>> = conn
        .prepare_cached("SELECT owner_id FROM workspaces WHERE id = ?1")?
        .query_row(params![ws.to_string()], |r| r.get(0))
        .optional()?;
    if let Some(None) = unowned
        && instance_owner_conn(conn)? == Some(user)
    {
        return Ok(Some(Role::Owner));
    }
    Ok(None)
}

/// What `scope` may do in `space`: None = it does not exist for them.
pub(crate) fn access_conn(conn: &Connection, scope: Scope, space: Space) -> Result<Option<Role>> {
    match scope {
        Scope::System | Scope::Local => Ok(Some(Role::Owner)),
        Scope::User(u) => match space {
            Space::Workspace(w) => member_role_conn(conn, w, u),
            Space::Unsorted(o) => Ok((o == Some(u)).then_some(Role::Owner)),
        },
        Scope::Within { user, workspace } => match space {
            Space::Workspace(w) if w == workspace => member_role_conn(conn, w, user),
            _ => Ok(None),
        },
    }
}

/// The scope's role on `doc`'s space; NotFound when it cannot see it.
pub(crate) fn doc_access_conn(conn: &Connection, scope: Scope, doc: Uuid) -> Result<Role> {
    let space = space_conn(conn, doc)?;
    access_conn(conn, scope, space)?.ok_or_else(|| StoreError::NotFound(format!("doc {doc}")))
}

pub(crate) fn ensure_visible_conn(conn: &Connection, scope: Scope, doc: Uuid) -> Result<()> {
    if scope.sees_all() {
        return Ok(());
    }
    doc_access_conn(conn, scope, doc).map(|_| ())
}

pub(crate) fn read_only(what: &str) -> StoreError {
    StoreError::Forbidden(format!("read-only: you are a viewer of the workspace {what} is in"))
}

/// NotFound when invisible, Forbidden when visible but read-only.
pub(crate) fn ensure_write_conn(conn: &Connection, scope: Scope, doc: Uuid) -> Result<()> {
    if scope.sees_all() {
        return Ok(());
    }
    if !doc_access_conn(conn, scope, doc)?.can_write() {
        return Err(read_only("this doc"));
    }
    Ok(())
}

/// Number of members of a workspace (a workspace is *shared* above one).
pub(crate) fn member_count_conn(conn: &Connection, ws: Uuid) -> Result<i64> {
    Ok(conn
        .prepare_cached("SELECT count(*) FROM workspace_members WHERE workspace_id = ?1")?
        .query_row(params![ws.to_string()], |r| r.get(0))?)
}

pub(crate) fn space_is_shared(conn: &Connection, space: Space) -> Result<bool> {
    match space {
        Space::Workspace(w) => Ok(member_count_conn(conn, w)? > 1),
        Space::Unsorted(_) => Ok(false),
    }
}

/// Is `principal` an agent (the share gate's subject)?
pub(crate) fn is_agent_conn(conn: &Connection, principal: Uuid) -> Result<bool> {
    let kind: Option<String> = conn
        .prepare_cached("SELECT kind FROM principals WHERE id = ?1")?
        .query_row(params![principal.to_string()], |r| r.get(0))
        .optional()?;
    Ok(kind.as_deref() == Some("agent"))
}

/// The share gate (ADR 0004 §5): an agent writing into a doc whose space is
/// a shared workspace never lands green.
pub(crate) fn agent_into_shared(conn: &Connection, doc: Uuid, principal: Uuid) -> Result<bool> {
    if !is_agent_conn(conn, principal)? {
        return Ok(false);
    }
    space_is_shared(conn, space_conn(conn, doc)?)
}

/// A space change (move, label) is allowed when the scope can write both
/// ends and owns the source (only an owner takes docs out of a workspace).
pub(crate) fn check_space_change(conn: &Connection, scope: Scope, from: Space, to: Space) -> Result<()> {
    if scope.sees_all() || from == to {
        return Ok(());
    }
    // the source first: a non-owner learns nothing about where it would go
    match access_conn(conn, scope, from)? {
        None => return Err(StoreError::NotFound("doc".into())),
        Some(s) if !s.can_write() => return Err(read_only("this doc")),
        Some(s) if s != Role::Owner => {
            return Err(StoreError::Forbidden("only the workspace owner can move docs out of it".into()));
        }
        Some(_) => {}
    }
    match access_conn(conn, scope, to)? {
        None => Err(StoreError::NotFound("destination".into())),
        Some(d) if !d.can_write() => Err(read_only("the destination")),
        Some(_) => Ok(()),
    }
}

// ---- the access journal: who gained or lost which docs ----

/// Every user's visible doc set (System work: computed for all users).
pub(crate) type AccessSnapshot = Vec<(Uuid, HashSet<String>)>;

pub(crate) fn access_snapshot(conn: &Connection) -> Result<AccessSnapshot> {
    let users: Vec<String> = {
        let mut st = conn.prepare_cached("SELECT id FROM auth_users ORDER BY created_at, id")?;
        st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::with_capacity(users.len());
    for u in users {
        let user = parse(u, "auth_users.id")?;
        let mut st = conn.prepare_cached(&vis_subquery(user, None))?;
        let ids: HashSet<String> = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        out.push((user, ids));
    }
    Ok(out)
}

/// Journal targeted rows for what changed since `before`: `deleted` (access
/// revoked: the client drops the doc) for docs a user lost, `tree` for docs
/// a user gained (the client fetches them).
pub(crate) fn journal_access_diff(conn: &Connection, before: AccessSnapshot) -> Result<()> {
    if before.is_empty() {
        return Ok(());
    }
    let after: HashMap<Uuid, HashSet<String>> = access_snapshot(conn)?.into_iter().collect();
    let mut st = conn.prepare_cached(
        "INSERT INTO changes (doc_id, kind, epoch, user_id)
         SELECT ?1, ?2, (SELECT current_epoch FROM docs WHERE id = ?1), ?3",
    )?;
    for (user, was) in before {
        let now = after.get(&user).cloned().unwrap_or_default();
        let mut lost: Vec<&String> = was.difference(&now).collect();
        let mut gained: Vec<&String> = now.difference(&was).collect();
        lost.sort();
        gained.sort();
        for d in lost {
            st.execute(params![d, "deleted", user.to_string()])?;
        }
        for d in gained {
            st.execute(params![d, "tree", user.to_string()])?;
        }
    }
    Ok(())
}

// ---- audit events ----

pub(crate) fn audit_conn(conn: &Connection, actor: Option<Uuid>, event: &str, subject: &str, detail: &serde_json::Value) -> Result<()> {
    conn.execute(
        "INSERT INTO audit_events (id, actor, event, subject, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![Uuid::now_v7().to_string(), actor.map(|a| a.to_string()), event, subject, detail.to_string()],
    )?;
    Ok(())
}

/// One recorded audit event.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AuditEvent {
    pub id: String,
    pub at: String,
    pub actor: Option<String>,
    pub event: String,
    pub subject: String,
    pub detail: serde_json::Value,
}

/// A workspace member, for the members list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Member {
    pub user_id: Uuid,
    pub name: String,
    pub role: Role,
    pub added_at: String,
    pub added_by: Option<Uuid>,
}

/// Adoption (ADR 0004 §7): everything with no owner becomes `owner`'s, and
/// every workspace gets its owner as an `owner` member. Idempotent.
pub(crate) fn adopt_unowned_conn(conn: &Connection, owner: Uuid) -> Result<()> {
    let o = owner.to_string();
    conn.execute("UPDATE docs SET owner_id = ?1 WHERE owner_id IS NULL", params![o])?;
    conn.execute("UPDATE workspaces SET owner_id = ?1 WHERE owner_id IS NULL", params![o])?;
    conn.execute("UPDATE gardeners SET owner_id = ?1 WHERE owner_id IS NULL", params![o])?;
    conn.execute(
        "INSERT OR IGNORE INTO workspace_members (workspace_id, user_id, role, added_by)
         SELECT id, owner_id, 'owner', owner_id FROM workspaces WHERE owner_id IS NOT NULL",
        [],
    )?;
    Ok(())
}

impl SqliteStore {
    /// The scope every call on this store currently runs for.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// The instance owner (the first `owner`-role user), if any.
    pub fn instance_owner(&self) -> Result<Option<Uuid>> {
        instance_owner_conn(&self.conn)
    }

    /// The scope background work owned by the instance owner runs in:
    /// `User(owner)` on a server, `Local` on a database with no users.
    pub fn owner_scope(&self) -> Result<Scope> {
        Ok(Scope::for_owner(self.instance_owner()?))
    }

    /// The space a doc resolves to (NotFound when the scope cannot see it).
    pub fn doc_space(&self, doc: Uuid) -> Result<Space> {
        ensure_visible_conn(&self.conn, self.scope, doc)?;
        space_conn(&self.conn, doc)
    }

    /// The scope's role on a doc: NotFound when invisible.
    pub fn doc_role(&self, doc: Uuid) -> Result<Role> {
        doc_access_conn(&self.conn, self.scope, doc)
    }

    /// Can the scope write this doc? (false when invisible or read-only)
    pub fn can_write_doc(&self, doc: Uuid) -> bool {
        ensure_write_conn(&self.conn, self.scope, doc).is_ok()
    }

    /// Is the doc's space a shared workspace? (NotFound when invisible)
    pub fn doc_is_shared(&self, doc: Uuid) -> Result<bool> {
        let space = self.doc_space(doc)?;
        space_is_shared(&self.conn, space)
    }

    /// The scope for an automated writer targeting `doc` on behalf of
    /// `owner`: narrowed to the doc's workspace when that workspace is
    /// shared, so it cannot copy one member's private docs into it.
    pub fn writer_scope_for(&self, owner: Option<Uuid>, doc: Uuid) -> Result<Scope> {
        let Some(user) = owner else { return Ok(Scope::Local) };
        Ok(match space_conn(&self.conn, doc)? {
            Space::Workspace(w) if member_count_conn(&self.conn, w)? > 1 => Scope::Within { user, workspace: w },
            _ => Scope::User(user),
        })
    }

    /// The root owner of a doc's tree (None = the instance owner / LOCAL).
    pub fn doc_home_user(&self, doc: Uuid) -> Result<Option<Uuid>> {
        ensure_visible_conn(&self.conn, self.scope, doc)?;
        Ok(match space_conn(&self.conn, doc)? {
            Space::Unsorted(o) => o,
            Space::Workspace(w) => {
                let o: Option<Option<String>> = self
                    .conn
                    .query_row("SELECT owner_id FROM workspaces WHERE id = ?1", params![w.to_string()], |r| r.get(0))
                    .optional()?;
                match o.flatten() {
                    Some(o) => Some(parse(o, "workspaces.owner_id")?),
                    None => self.instance_owner()?,
                }
            }
        })
    }

    /// Every block id the scope may see (None: System/Local, no filter) —
    /// the allowlist a global in-memory index (embeddings) is cut to before
    /// ranking.
    pub fn visible_block_ids(&self) -> Result<Option<HashSet<Uuid>>> {
        let Some(sub) = vis_sub(self.scope) else { return Ok(None) };
        let mut st = self
            .conn
            .prepare_cached(&format!("SELECT id FROM blocks WHERE deleted = 0 AND doc_id IN ({sub})"))?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for r in rows {
            out.insert(parse(r?, "blocks.id")?);
        }
        Ok(Some(out))
    }

    /// The scope's own live root doc titled `title` (the per-person
    /// singletons: Inbox, To-do, Answers, Claude Memory). A root whose
    /// owner is the viewer; System/Local: any root. Oldest first.
    pub fn own_root_titled(&self, title: &str) -> Result<Option<crate::Doc>> {
        let owner = match self.scope.user() {
            None => "1".to_string(),
            Some(u) => format!("COALESCE(owner_id, {INSTANCE_OWNER_SQL}) = '{u}'"),
        };
        let id: Option<String> = self
            .conn
            .query_row(
                &format!(
                    "SELECT id FROM docs WHERE parent_id IS NULL AND deleted = 0 AND title = ?1 AND {owner} AND {}
                     ORDER BY created_at, id LIMIT 1",
                    vis_pred(self.scope, "id")
                ),
                params![title],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => Ok(Some(crate::BlockStore::get_doc(self, parse(id, "docs.id")?)?)),
            None => Ok(None),
        }
    }

    /// Record an audit event as the current scope's user.
    pub fn audit(&mut self, event: &str, subject: &str, detail: serde_json::Value) -> Result<()> {
        audit_conn(&self.conn, self.scope.user(), event, subject, &detail)
    }

    /// Audit events, newest first. System scope only (the box's CLI).
    pub fn audit_events(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        if self.scope != Scope::System {
            return Err(StoreError::Forbidden("audit events are for the box's CLI".into()));
        }
        let mut st = self.conn.prepare(
            "SELECT id, at, actor, event, subject, detail FROM audit_events ORDER BY at DESC, id DESC LIMIT ?1",
        )?;
        let rows = st.query_map(params![limit as i64], |r| {
            let detail: String = r.get(5)?;
            Ok(AuditEvent {
                id: r.get(0)?,
                at: r.get(1)?,
                actor: r.get(2)?,
                event: r.get(3)?,
                subject: r.get(4)?,
                detail: serde_json::from_str(&detail).unwrap_or(serde_json::Value::Null),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adopt every unowned doc/workspace/gardener for `owner` (see
    /// [`adopt_unowned_conn`]).
    pub fn adopt_unowned(&mut self, owner: Uuid) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        adopt_unowned_conn(&tx, owner)?;
        tx.commit()?;
        Ok(())
    }

    /// Every user who can see at least one change after `since` (the push
    /// fan-out). System scope only.
    pub fn change_audience(&self, since: i64) -> Result<HashSet<Uuid>> {
        if self.scope != Scope::System {
            return Err(StoreError::Forbidden("change_audience is System work".into()));
        }
        let users: Vec<String> = {
            let mut st = self.conn.prepare_cached("SELECT id FROM auth_users")?;
            st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let mut out = HashSet::new();
        for u in users {
            let user = parse(u, "auth_users.id")?;
            let n: i64 = self.conn.query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM changes c WHERE c.seq > ?1
                       AND ((c.user_id IS NULL AND c.doc_id IN ({})) OR c.user_id = ?2))",
                    vis_subquery(user, None)
                ),
                params![since, user.to_string()],
                |r| r.get(0),
            )?;
            if n != 0 {
                out.insert(user);
            }
        }
        Ok(out)
    }
}
