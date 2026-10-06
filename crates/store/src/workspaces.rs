//! Workspaces (workspaces.sql): a label on a doc, inherited by its subtree.
//! A doc's workspace is its nearest labelled ancestor's (itself included);
//! none = Unsorted. Resolution is always a query, so moves re-resolve with
//! no cache to invalidate. Every label or workspace change journals a `tree`
//! row for each doc whose answer (or whose workspace's metadata) changed, so
//! a syncing client re-reads them.

use crate::scope::{Role, Scope};
use crate::sqlite::SqliteStore;
use crate::tenancy::{self, Member, Space};
use crate::{Result, StoreError};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// The `?workspace=` value `unsorted`: docs with no label up their chain.
pub const UNSORTED: &str = "unsorted";
const NAME_MAX: usize = 64;
const ATTR_MAX: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub sort_key: Option<String>,
    pub created_at: String,
    /// The live docs labelled with it (explicitly), in tree-agnostic id
    /// order: with each doc's parent_id that is enough to resolve locally.
    pub doc_ids: Vec<Uuid>,
    /// ADR 0004: the owning user (None only on a database with no users).
    pub owner_id: Option<Uuid>,
    /// The owner's display name.
    pub owner_name: Option<String>,
    /// What a switcher shows: the name, or `Name · Owner` for someone
    /// else's workspace whose name clashes with another visible one.
    pub display_name: String,
    /// The viewer's role in it.
    pub role: Role,
    /// More than one member.
    pub shared: bool,
}

/// A workspace filter: one workspace, or Unsorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceFilter {
    Unsorted,
    Id(Uuid),
}

impl WorkspaceFilter {
    /// The resolved workspace this filter keeps.
    pub fn as_option(self) -> Option<Uuid> {
        match self {
            WorkspaceFilter::Unsorted => None,
            WorkspaceFilter::Id(id) => Some(id),
        }
    }
}

/// PATCH fields: `None` leaves a field alone, `Some(None)` clears it.
#[derive(Debug, Clone, Default)]
pub struct WorkspacePatch {
    pub name: Option<String>,
    pub color: Option<Option<String>>,
    pub icon: Option<Option<String>>,
    pub sort_key: Option<Option<String>>,
}

fn clean_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        return Err(StoreError::InvalidOp("workspace name must not be empty".into()));
    }
    if n.chars().count() > NAME_MAX {
        return Err(StoreError::InvalidOp(format!("workspace name is over {NAME_MAX} characters")));
    }
    if n.eq_ignore_ascii_case(UNSORTED) {
        return Err(StoreError::InvalidOp("\"Unsorted\" is reserved for docs with no workspace".into()));
    }
    Ok(n.to_string())
}

fn clean_attr(what: &str, v: Option<&str>) -> Result<Option<String>> {
    match v.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) if v.chars().count() > ATTR_MAX => Err(StoreError::InvalidOp(format!("workspace {what} is over {ATTR_MAX} characters"))),
        Some(v) => Ok(Some(v.to_string())),
        None => Ok(None),
    }
}

fn clean_sort_key(k: Option<&str>) -> Result<Option<String>> {
    match k.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) if crate::order_key::is_valid(k) => Ok(Some(k.to_string())),
        Some(k) => Err(StoreError::InvalidOp(format!("invalid sort_key {k:?}"))),
        None => Ok(None),
    }
}

fn name_taken(e: rusqlite::Error, name: &str) -> StoreError {
    match &e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
            StoreError::InvalidOp(format!("a workspace named {name:?} already exists"))
        }
        _ => e.into(),
    }
}

fn parse_id(s: String) -> Result<Uuid> {
    Uuid::parse_str(&s).map_err(|_| StoreError::InvalidOp(format!("bad uuid in workspaces: {s}")))
}

/// The resolved workspace of `doc` (live or not): its nearest labelled
/// ancestor's, itself included. None = Unsorted (or no such doc).
pub(crate) fn resolve_conn(conn: &Connection, doc: Uuid) -> Result<Option<Uuid>> {
    let ws: Option<String> = conn
        .prepare_cached(
            "WITH RECURSIVE up(id, parent_id, depth) AS (
                 SELECT id, parent_id, 0 FROM docs WHERE id = ?1
                 UNION ALL
                 SELECT d.id, d.parent_id, up.depth + 1 FROM docs d JOIN up ON d.id = up.parent_id
                 WHERE up.depth < 256)
             SELECT l.workspace_id FROM up JOIN doc_workspace l ON l.doc_id = up.id
             ORDER BY up.depth LIMIT 1",
        )?
        .query_row(params![doc.to_string()], |r| r.get(0))
        .optional()?;
    ws.map(parse_id).transpose()
}

/// Journal a `tree` row for each live doc in `ids`.
pub(crate) fn emit_tree_conn(conn: &Connection, ids: &[Uuid]) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO changes (doc_id, kind, epoch)
         SELECT id, 'tree', current_epoch FROM docs WHERE id = ?1 AND deleted = 0",
    )?;
    for id in ids {
        stmt.execute(params![id.to_string()])?;
    }
    Ok(())
}

/// Live descendants of `doc` (excluding it).
pub(crate) fn descendants_conn(conn: &Connection, doc: Uuid) -> Result<Vec<Uuid>> {
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE sub(id) AS (
             SELECT id FROM docs WHERE parent_id = ?1 AND deleted = 0
             UNION ALL
             SELECT docs.id FROM docs JOIN sub ON docs.parent_id = sub.id WHERE docs.deleted = 0)
         SELECT id FROM sub",
    )?;
    let rows = stmt.query_map(params![doc.to_string()], |r| r.get::<_, String>(0))?;
    rows.map(|r| parse_id(r?)).collect()
}

/// Every live doc's resolved workspace, one recursive pass down the tree.
/// A live doc under a tombstoned parent resolves from itself up only as far
/// as the walk reaches, i.e. its own label or Unsorted.
fn map_conn(conn: &Connection) -> Result<HashMap<Uuid, Option<Uuid>>> {
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE r(id, ws) AS (
             SELECT d.id, l.workspace_id FROM docs d LEFT JOIN doc_workspace l ON l.doc_id = d.id
             WHERE d.deleted = 0
               AND (d.parent_id IS NULL OR NOT EXISTS (SELECT 1 FROM docs p WHERE p.id = d.parent_id AND p.deleted = 0))
             UNION ALL
             SELECT d.id, COALESCE(l.workspace_id, r.ws) FROM docs d JOIN r ON d.parent_id = r.id
             LEFT JOIN doc_workspace l ON l.doc_id = d.id
             WHERE d.deleted = 0)
         SELECT id, ws FROM r",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, ws) = row?;
        out.insert(parse_id(id)?, ws.map(parse_id).transpose()?);
    }
    Ok(out)
}

/// The explicit label on `doc`, unscoped (callers have checked access).
pub(crate) fn label_conn(conn: &Connection, doc: Uuid) -> Result<Option<Uuid>> {
    let ws: Option<String> = conn
        .prepare_cached("SELECT workspace_id FROM doc_workspace WHERE doc_id = ?1")?
        .query_row(params![doc.to_string()], |r| r.get(0))
        .optional()?;
    ws.map(parse_id).transpose()
}

/// Which workspaces a scope lists: its memberships (and, for the instance
/// owner, any workspace with no owner recorded). System/Local: all.
fn ws_pred(scope: Scope, col: &str) -> String {
    if scope.is_public() {
        return "0".into();
    }
    match scope.user() {
        None => "1".into(),
        Some(u) => format!(
            "({col} IN (SELECT workspace_id FROM workspace_members WHERE user_id = '{u}')
              OR {col} IN (SELECT id FROM workspaces WHERE owner_id IS NULL AND '{u}' = {}))",
            tenancy::INSTANCE_OWNER_SQL
        ),
    }
}

/// `Name · Owner` for a workspace someone else owns whose name clashes with
/// another one the viewer can see.
fn with_display_names(mut list: Vec<Workspace>, viewer: Option<Uuid>) -> Vec<Workspace> {
    let mut count: HashMap<String, usize> = HashMap::new();
    for w in &list {
        *count.entry(w.name.to_lowercase()).or_default() += 1;
    }
    for w in list.iter_mut() {
        let mine = viewer.is_none() || w.owner_id == viewer;
        w.display_name = match (&w.owner_name, count[&w.name.to_lowercase()] > 1 && !mine) {
            (Some(owner), true) => format!("{} · {owner}", w.name),
            _ => w.name.clone(),
        };
    }
    list
}

impl SqliteStore {
    pub fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let scope = self.scope;
        let mut labels: HashMap<String, Vec<Uuid>> = HashMap::new();
        {
            let mut stmt = self.conn.prepare_cached(&format!(
                "SELECT l.workspace_id, l.doc_id FROM doc_workspace l JOIN docs d ON d.id = l.doc_id
                 WHERE d.deleted = 0 AND {} ORDER BY l.doc_id",
                tenancy::vis_pred(scope, "d.id")
            ))?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (ws, doc) = row?;
                labels.entry(ws).or_default().push(parse_id(doc)?);
            }
        }
        let viewer = scope.user();
        let role_sql = match viewer {
            None => "'owner'".to_string(),
            Some(u) => format!(
                "COALESCE((SELECT m.role FROM workspace_members m WHERE m.workspace_id = w.id AND m.user_id = '{u}'), 'owner')"
            ),
        };
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT w.id, w.name, w.color, w.icon, w.sort_key, w.created_at,
                    COALESCE(w.owner_id, {inst}),
                    (SELECT a.name FROM auth_users a WHERE a.id = COALESCE(w.owner_id, {inst})),
                    {role_sql},
                    (SELECT count(*) FROM workspace_members m WHERE m.workspace_id = w.id)
             FROM workspaces w
             WHERE {pred}
             ORDER BY w.sort_key IS NULL, w.sort_key, w.name COLLATE NOCASE",
            inst = tenancy::INSTANCE_OWNER_SQL,
            pred = ws_pred(scope, "w.id"),
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, i64>(9)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, color, icon, sort_key, created_at, owner, owner_name, role, members) = row?;
            let doc_ids = labels.remove(&id).unwrap_or_default();
            out.push(Workspace {
                id: parse_id(id)?,
                display_name: name.clone(),
                name,
                color,
                icon,
                sort_key,
                created_at,
                doc_ids,
                owner_id: owner.map(parse_id).transpose()?,
                owner_name,
                role: Role::parse(&role).unwrap_or(Role::Viewer),
                shared: members > 1,
            });
        }
        Ok(with_display_names(out, viewer))
    }

    /// NotFound unless the scope is a member.
    pub fn get_workspace(&self, id: Uuid) -> Result<Workspace> {
        self.list_workspaces()?
            .into_iter()
            .find(|w| w.id == id)
            .ok_or_else(|| StoreError::NotFound(format!("workspace {id}")))
    }

    /// By id, or by name case-insensitively, among the scope's workspaces
    /// only (ADR 0004 §5): an id wins; else the viewer's own workspace of
    /// that name; else the one shared workspace of that name (or its
    /// `Name · Owner` display name). Several shared ones of that name are an
    /// error listing the candidates with owner names and ids.
    pub fn find_workspace(&self, name_or_id: &str) -> Result<Option<Workspace>> {
        let key = name_or_id.trim();
        let list = self.list_workspaces()?;
        if let Ok(id) = Uuid::parse_str(key) {
            return Ok(list.into_iter().find(|w| w.id == id));
        }
        let lower = key.to_lowercase();
        let viewer = self.scope.user();
        let mut hits: Vec<Workspace> = list
            .into_iter()
            .filter(|w| w.name.to_lowercase() == lower || w.display_name.to_lowercase() == lower)
            .collect();
        if hits.len() <= 1 {
            return Ok(hits.pop());
        }
        if let Some(i) = hits.iter().position(|w| viewer.is_some() && w.owner_id == viewer) {
            return Ok(Some(hits.swap_remove(i)));
        }
        if let Some(i) = hits.iter().position(|w| w.display_name.to_lowercase() == lower) {
            return Ok(Some(hits.swap_remove(i)));
        }
        let candidates: Vec<String> = hits
            .iter()
            .map(|w| format!("{} (id {})", w.display_name, w.id))
            .collect();
        Err(StoreError::InvalidOp(format!(
            "workspace {key:?} is ambiguous: {} — pass the id",
            candidates.join(", ")
        )))
    }

    /// `unsorted`, a workspace id or (case-insensitively) a name.
    pub fn parse_workspace_filter(&self, s: &str) -> Result<WorkspaceFilter> {
        if s.trim().eq_ignore_ascii_case(UNSORTED) {
            return Ok(WorkspaceFilter::Unsorted);
        }
        match self.find_workspace(s)? {
            Some(w) => Ok(WorkspaceFilter::Id(w.id)),
            None => Err(StoreError::NotFound(format!("workspace {:?}", s.trim()))),
        }
    }

    /// Create a workspace owned by the scope's user (System/Local: no
    /// recorded owner = the instance owner); no sort key appends it.
    pub fn create_workspace(&mut self, name: &str, color: Option<&str>, icon: Option<&str>, sort_key: Option<&str>) -> Result<Workspace> {
        self.deny_public()?;
        let name = clean_name(name)?;
        let color = clean_attr("color", color)?;
        let icon = clean_attr("icon", icon)?;
        let owner = self.scope.user();
        let sort_key = match clean_sort_key(sort_key)? {
            Some(k) => k,
            None => {
                let last: Option<String> = self.conn.query_row(
                    &format!("SELECT max(sort_key) FROM workspaces w WHERE {}", ws_pred(self.scope, "w.id")),
                    [],
                    |r| r.get(0),
                )?;
                crate::order_key::between(last.as_deref(), None)
            }
        };
        let id = Uuid::now_v7();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO workspaces (id, owner_id, name, color, icon, sort_key) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id.to_string(), owner.map(|o| o.to_string()), name, color, icon, sort_key],
        )
        .map_err(|e| name_taken(e, &name))?;
        let member = match owner {
            Some(o) => Some(o),
            None => tenancy::instance_owner_conn(&tx)?,
        };
        if let Some(o) = member {
            tx.execute(
                "INSERT OR IGNORE INTO workspace_members (workspace_id, user_id, role, added_by) VALUES (?1, ?2, 'owner', ?2)",
                params![id.to_string(), o.to_string()],
            )?;
            if owner.is_none() {
                tx.execute("UPDATE workspaces SET owner_id = ?1 WHERE id = ?2", params![o.to_string(), id.to_string()])?;
            }
        }
        tx.commit()?;
        self.get_workspace(id)
    }

    /// The workspace, if the scope owns it (Forbidden for other members).
    fn owned_workspace(&self, id: Uuid) -> Result<Workspace> {
        let w = self.get_workspace(id)?;
        if !self.scope.sees_all() && w.role != Role::Owner {
            return Err(StoreError::Forbidden("only the workspace owner can change or delete it".into()));
        }
        Ok(w)
    }

    /// Update a workspace's fields (owner only); journals its docs (their
    /// chips change).
    pub fn update_workspace(&mut self, id: Uuid, patch: WorkspacePatch) -> Result<Workspace> {
        let cur = self.owned_workspace(id)?;
        let name = match &patch.name {
            Some(n) => clean_name(n)?,
            None => cur.name.clone(),
        };
        let color = match &patch.color {
            Some(c) => clean_attr("color", c.as_deref())?,
            None => cur.color.clone(),
        };
        let icon = match &patch.icon {
            Some(c) => clean_attr("icon", c.as_deref())?,
            None => cur.icon.clone(),
        };
        let sort_key = match &patch.sort_key {
            Some(k) => clean_sort_key(k.as_deref())?,
            None => cur.sort_key.clone(),
        };
        let docs = self.ws_doc_ids_all(id)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE workspaces SET name = ?1, color = ?2, icon = ?3, sort_key = ?4 WHERE id = ?5",
            params![name, color, icon, sort_key, id.to_string()],
        )
        .map_err(|e| name_taken(e, &name))?;
        emit_tree_conn(&tx, &docs)?;
        tx.commit()?;
        self.get_workspace(id)
    }

    /// Delete a workspace (owner only): its labels go (CASCADE), its docs
    /// go to their root owner's Unsorted or an outer workspace; no doc is
    /// touched. Members who lose docs get access-revoked rows. Returns the
    /// labels removed.
    pub fn delete_workspace(&mut self, id: Uuid) -> Result<usize> {
        self.owned_workspace(id)?;
        let docs = self.ws_doc_ids_all(id)?;
        let ws_owner = self.get_workspace(id)?.owner_id;
        let labelled: Vec<Uuid> = {
            let mut st = self.conn.prepare("SELECT doc_id FROM doc_workspace WHERE workspace_id = ?1")?;
            st.query_map(params![id.to_string()], |r| r.get::<_, String>(0))?
                .map(|r| r.map_err(StoreError::from).and_then(parse_id))
                .collect::<Result<_>>()?
        };
        let tx = self.conn.unchecked_transaction()?;
        let before = tenancy::access_snapshot(&tx)?;
        let labels = tx.execute("DELETE FROM doc_workspace WHERE workspace_id = ?1", params![id.to_string()])?;
        tx.execute("DELETE FROM workspaces WHERE id = ?1", params![id.to_string()])?;
        // every labelled root goes back to whoever put it there (ADR 0004):
        // its contributor (the doc's owner — the workspace owner, or a member
        // who filed their own doc in). It stays where it now falls if they
        // can write there; otherwise it comes to their own root, in their
        // Unsorted. Ownership never changes hands as a side effect.
        let _ = ws_owner;
        for d in labelled {
            let contributor: Option<String> = tx
                .query_row(
                    &format!("SELECT COALESCE(owner_id, {}) FROM docs WHERE id = ?1", tenancy::INSTANCE_OWNER_SQL),
                    params![d.to_string()],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let Some(contributor) = contributor.and_then(|c| Uuid::parse_str(&c).ok()) else { continue };
            let Ok(lands) = tenancy::space_conn(&tx, d) else { continue };
            let can_write = tenancy::access_conn(&tx, Scope::User(contributor), lands)?.is_some_and(|r| r.can_write());
            if !can_write {
                tx.execute(
                    "UPDATE docs SET parent_id = NULL, owner_id = ?1 WHERE id = ?2",
                    params![contributor.to_string(), d.to_string()],
                )?;
            }
        }
        emit_tree_conn(&tx, &docs)?;
        tenancy::journal_access_diff(&tx, before)?;
        tenancy::audit_conn(&tx, self.scope.user(), "workspace.delete", &id.to_string(), &serde_json::json!({}))?;
        tx.commit()?;
        Ok(labels)
    }

    /// Every live doc resolving to `ws`, unscoped (journal bookkeeping for a
    /// workspace the caller has already been authorized on).
    fn ws_doc_ids_all(&self, ws: Uuid) -> Result<Vec<Uuid>> {
        Ok(map_conn(&self.conn)?.into_iter().filter(|(_, w)| *w == Some(ws)).map(|(id, _)| id).collect())
    }

    /// Label `doc` (Some) or clear its label (None), as `by`. Journals the
    /// doc and its live subtree. Returns the doc's resolved workspace
    /// afterwards. A label that changes the doc's space is a share: it needs
    /// write on both ends and ownership of the source, and an agent may not
    /// take a doc into or out of a shared workspace (a human surface).
    pub fn set_doc_workspace(&mut self, doc: Uuid, workspace: Option<Uuid>, by: Uuid) -> Result<Option<Uuid>> {
        let scope = self.scope;
        crate::BlockStore::get_doc(self, doc)?;
        tenancy::ensure_write_conn(&self.conn, scope, doc)?;
        if let Some(w) = workspace {
            self.get_workspace(w)?;
        }
        let from = tenancy::space_conn(&self.conn, doc)?;
        let to = match workspace {
            Some(w) => Space::Workspace(w),
            None => {
                // unlabelled, it takes its parent's space (or its root owner's Unsorted)
                let parent: Option<String> = self
                    .conn
                    .query_row("SELECT parent_id FROM docs WHERE id = ?1", params![doc.to_string()], |r| r.get(0))?;
                match parent.map(parse_id).transpose()? {
                    Some(p) => tenancy::space_conn(&self.conn, p)?,
                    None => {
                        let owner: Option<String> =
                            self.conn.query_row("SELECT owner_id FROM docs WHERE id = ?1", params![doc.to_string()], |r| r.get(0))?;
                        match owner.map(parse_id).transpose()? {
                            Some(o) => Space::Unsorted(Some(o)),
                            None => Space::Unsorted(tenancy::instance_owner_conn(&self.conn)?),
                        }
                    }
                }
            }
        };
        if let Err(e) = tenancy::check_space_change(&self.conn, scope, from, to) {
            if matches!(e, StoreError::Forbidden(_)) {
                tenancy::audit_conn(&self.conn, scope.user(), "refused.label", &doc.to_string(), &serde_json::json!({"why": e.to_string()}))?;
            }
            return Err(e);
        }
        if from != to
            && tenancy::is_agent_conn(&self.conn, by)?
            && (tenancy::space_is_shared(&self.conn, from)? || tenancy::space_is_shared(&self.conn, to)?)
        {
            tenancy::audit_conn(&self.conn, scope.user(), "refused.agent_share", &doc.to_string(), &serde_json::json!({"by": by}))?;
            return Err(StoreError::Forbidden(
                "filing a doc into or out of a shared workspace is a human action (use the app)".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        let before = tenancy::access_snapshot(&tx)?;
        let prev: Option<String> = tx
            .query_row("SELECT workspace_id FROM doc_workspace WHERE doc_id = ?1", params![doc.to_string()], |r| r.get(0))
            .optional()?;
        if prev.as_deref() != workspace.map(|w| w.to_string()).as_deref() {
            match workspace {
                Some(w) => tx.execute(
                    "INSERT INTO doc_workspace (doc_id, workspace_id) VALUES (?1, ?2)
                     ON CONFLICT (doc_id) DO UPDATE SET workspace_id = excluded.workspace_id",
                    params![doc.to_string(), w.to_string()],
                )?,
                None => tx.execute("DELETE FROM doc_workspace WHERE doc_id = ?1", params![doc.to_string()])?,
            };
            let mut ids = vec![doc];
            ids.extend(descendants_conn(&tx, doc)?);
            emit_tree_conn(&tx, &ids)?;
        }
        tenancy::journal_access_diff(&tx, before)?;
        let out = resolve_conn(&tx, doc)?;
        tx.commit()?;
        Ok(out)
    }

    /// The doc's explicit label, if any.
    pub fn doc_label(&self, doc: Uuid) -> Result<Option<Uuid>> {
        self.see(doc)?;
        label_conn(&self.conn, doc)
    }

    /// The doc's resolved workspace (None = Unsorted).
    pub fn doc_workspace(&self, doc: Uuid) -> Result<Option<Uuid>> {
        self.see(doc)?;
        resolve_conn(&self.conn, doc)
    }

    /// Every visible live doc's resolved workspace.
    pub fn workspace_map(&self) -> Result<HashMap<Uuid, Option<Uuid>>> {
        let mut map = map_conn(&self.conn)?;
        if !self.scope.sees_all() {
            let visible: HashSet<Uuid> = crate::BlockStore::list_docs(self)?.into_iter().map(|d| d.id).collect();
            map.retain(|id, _| visible.contains(id));
        }
        Ok(map)
    }

    /// The visible live docs a filter keeps (subtree semantics).
    pub fn workspace_doc_ids(&self, filter: WorkspaceFilter) -> Result<HashSet<Uuid>> {
        let want = filter.as_option();
        Ok(self.workspace_map()?.into_iter().filter(|(_, ws)| *ws == want).map(|(id, _)| id).collect())
    }

    // ---- membership (ADR 0004): human surfaces only ----

    /// The members of a workspace the scope is a member of.
    pub fn workspace_members(&self, ws: Uuid) -> Result<Vec<Member>> {
        self.get_workspace(ws)?;
        let mut st = self.conn.prepare(
            "SELECT m.user_id, COALESCE(a.name, ''), m.role, m.added_at, m.added_by
             FROM workspace_members m LEFT JOIN auth_users a ON a.id = m.user_id
             WHERE m.workspace_id = ?1
             ORDER BY m.role = 'owner' DESC, a.name COLLATE NOCASE",
        )?;
        let rows = st.query_map(params![ws.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (user, name, role, added_at, added_by) = row?;
            out.push(Member {
                user_id: parse_id(user)?,
                name,
                role: Role::parse(&role).unwrap_or(Role::Viewer),
                added_at,
                added_by: added_by.and_then(|b| Uuid::parse_str(&b).ok()),
            });
        }
        Ok(out)
    }

    /// Add `user` to a workspace as editor or viewer, or change their role.
    /// The workspace owner (or System, the box's CLI) only; the owner's own
    /// row is never changed here. Members who gain docs get access rows.
    pub fn share_workspace(&mut self, ws: Uuid, user: Uuid, role: Role) -> Result<()> {
        if role == Role::Owner {
            return Err(StoreError::InvalidOp("a workspace has one owner; share as editor or viewer".into()));
        }
        let w = self.owned_workspace(ws)?;
        if w.owner_id == Some(user) {
            return Err(StoreError::InvalidOp("that user owns this workspace".into()));
        }
        if self.auth_user(user)?.is_none() {
            return Err(StoreError::NotFound(format!("user {user}")));
        }
        let tx = self.conn.unchecked_transaction()?;
        let before = tenancy::access_snapshot(&tx)?;
        tx.execute(
            "INSERT INTO workspace_members (workspace_id, user_id, role, added_by) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = excluded.role",
            params![ws.to_string(), user.to_string(), role.as_str(), self.scope.user().map(|u| u.to_string())],
        )?;
        tenancy::journal_access_diff(&tx, before)?;
        tenancy::audit_conn(
            &tx,
            self.scope.user(),
            "workspace.share",
            &ws.to_string(),
            &serde_json::json!({"user": user, "role": role.as_str()}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Remove `user` from a workspace (owner or System only; never the
    /// owner). Their clients get access-revoked rows for its docs.
    pub fn unshare_workspace(&mut self, ws: Uuid, user: Uuid) -> Result<bool> {
        let w = self.owned_workspace(ws)?;
        if w.owner_id == Some(user) {
            return Err(StoreError::InvalidOp("the owner cannot be removed from their workspace".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        let before = tenancy::access_snapshot(&tx)?;
        let n = tx.execute(
            "DELETE FROM workspace_members WHERE workspace_id = ?1 AND user_id = ?2 AND role != 'owner'",
            params![ws.to_string(), user.to_string()],
        )?;
        tenancy::journal_access_diff(&tx, before)?;
        tenancy::audit_conn(&tx, self.scope.user(), "workspace.unshare", &ws.to_string(), &serde_json::json!({"user": user, "removed": n > 0}))?;
        tx.commit()?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockStore, PrincipalKind};

    fn setup() -> (SqliteStore, Uuid) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let h = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        (s, h)
    }

    fn tree_rows_after(s: &SqliteStore, seq: i64) -> Vec<String> {
        s.changes_since(seq, 1000).unwrap().changes.into_iter().filter(|c| c.kind == "tree").map(|c| c.doc_id).collect()
    }

    #[test]
    fn nearest_label_wins_and_moves_and_deletes_re_resolve() {
        let (mut s, h) = setup();
        let work = s.create_workspace("Work", Some("#f00"), None, None).unwrap();
        let home = s.create_workspace("Home", None, Some("house"), None).unwrap();
        let a = s.create_doc("A", None, h).unwrap().id;
        let b = s.create_doc("B", Some(a), h).unwrap().id;
        let c = s.create_doc("C", Some(b), h).unwrap().id;
        let other = s.create_doc("Other", None, h).unwrap().id;
        assert_eq!(s.doc_workspace(c).unwrap(), None, "unlabelled = Unsorted");

        s.set_doc_workspace(a, Some(work.id), h).unwrap();
        assert_eq!(s.doc_workspace(c).unwrap(), Some(work.id), "inherited from the nearest labelled ancestor");
        s.set_doc_workspace(b, Some(home.id), h).unwrap();
        assert_eq!(s.doc_workspace(a).unwrap(), Some(work.id));
        assert_eq!(s.doc_workspace(b).unwrap(), Some(home.id), "a deeper label overrides");
        assert_eq!(s.doc_workspace(c).unwrap(), Some(home.id));
        let map = s.workspace_map().unwrap();
        assert_eq!(map[&c], Some(home.id));
        assert_eq!(map[&other], None);
        assert_eq!(s.workspace_doc_ids(WorkspaceFilter::Id(work.id)).unwrap(), HashSet::from([a]));
        assert_eq!(s.workspace_doc_ids(WorkspaceFilter::Unsorted).unwrap(), HashSet::from([other]));

        // a move re-resolves, and journals the descendants whose answer changed
        s.set_doc_workspace(b, None, h).unwrap();
        let seq = s.latest_change_seq().unwrap();
        s.move_doc(b, Some(other), None).unwrap();
        assert_eq!(s.doc_workspace(c).unwrap(), None);
        let rows = tree_rows_after(&s, seq);
        assert!(rows.contains(&b.to_string()) && rows.contains(&c.to_string()), "{rows:?}");

        s.set_doc_workspace(other, Some(work.id), h).unwrap();
        assert_eq!(s.doc_workspace(c).unwrap(), Some(work.id));
        assert_eq!(s.get_workspace(work.id).unwrap().doc_ids.len(), 2);

        // deleting a workspace un-labels; docs stay
        let n_docs = s.list_docs().unwrap().len();
        assert_eq!(s.delete_workspace(work.id).unwrap(), 2);
        assert_eq!(s.list_docs().unwrap().len(), n_docs);
        assert_eq!(s.doc_workspace(c).unwrap(), None);
        assert_eq!(s.doc_label(a).unwrap(), None);
        assert!(s.get_workspace(work.id).is_err());
    }

    #[test]
    fn label_and_workspace_changes_journal_the_subtree() {
        let (mut s, h) = setup();
        let w = s.create_workspace("Work", None, None, None).unwrap();
        let a = s.create_doc("A", None, h).unwrap().id;
        let b = s.create_doc("B", Some(a), h).unwrap().id;
        let seq = s.latest_change_seq().unwrap();
        s.set_doc_workspace(a, Some(w.id), h).unwrap();
        assert_eq!(tree_rows_after(&s, seq), vec![a.to_string(), b.to_string()]);
        let page = s.changes_since(seq, 10).unwrap();
        assert_eq!(page.changes[1].doc.as_ref().unwrap().workspace_id, Some(w.id.to_string()), "the summary carries the resolved workspace");

        let seq = s.latest_change_seq().unwrap();
        s.set_doc_workspace(a, Some(w.id), h).unwrap();
        assert!(tree_rows_after(&s, seq).is_empty(), "a no-op label journals nothing");

        let seq = s.latest_change_seq().unwrap();
        s.update_workspace(w.id, WorkspacePatch { name: Some("Job".into()), ..Default::default() }).unwrap();
        assert_eq!(tree_rows_after(&s, seq).len(), 2, "a rename re-journals its docs");

        let seq = s.latest_change_seq().unwrap();
        s.delete_workspace(w.id).unwrap();
        assert_eq!(tree_rows_after(&s, seq).len(), 2);
        assert_eq!(s.changes_since(seq, 10).unwrap().changes[0].doc.as_ref().unwrap().workspace_id, None);
    }

    #[test]
    fn names_are_unique_case_insensitively_and_unsorted_is_reserved() {
        let (mut s, _) = setup();
        let w = s.create_workspace("  Work ", None, None, None).unwrap();
        assert_eq!(w.name, "Work");
        assert!(s.create_workspace("work", None, None, None).is_err());
        assert!(s.create_workspace("Unsorted", None, None, None).is_err());
        assert!(s.create_workspace("", None, None, None).is_err());
        assert_eq!(s.find_workspace("WORK").unwrap().unwrap().id, w.id);
        assert_eq!(s.find_workspace(&w.id.to_string()).unwrap().unwrap().id, w.id);
        assert_eq!(s.parse_workspace_filter("unsorted").unwrap(), WorkspaceFilter::Unsorted);
        assert!(s.parse_workspace_filter("nope").is_err());
        let h = s.create_workspace("Home", None, None, None).unwrap();
        assert!(h.sort_key > w.sort_key, "appended");
        assert_eq!(s.list_workspaces().unwrap().iter().map(|w| w.name.as_str()).collect::<Vec<_>>(), ["Work", "Home"]);
        assert!(s.update_workspace(h.id, WorkspacePatch { name: Some("WORK".into()), ..Default::default() }).is_err());
        let h = s.update_workspace(h.id, WorkspacePatch { color: Some(Some("#0f0".into())), ..Default::default() }).unwrap();
        assert_eq!(h.color.as_deref(), Some("#0f0"));
        assert_eq!(h.name, "Home");
    }
}
