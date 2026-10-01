//! Workspaces (workspaces.sql): a label on a doc, inherited by its subtree.
//! A doc's workspace is its nearest labelled ancestor's (itself included);
//! none = Unsorted. Resolution is always a query, so moves re-resolve with
//! no cache to invalidate. Every label or workspace change journals a `tree`
//! row for each doc whose answer (or whose workspace's metadata) changed, so
//! a syncing client re-reads them.

use crate::sqlite::SqliteStore;
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

impl SqliteStore {
    pub fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let mut labels: HashMap<String, Vec<Uuid>> = HashMap::new();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT l.workspace_id, l.doc_id FROM doc_workspace l JOIN docs d ON d.id = l.doc_id
                 WHERE d.deleted = 0 ORDER BY l.doc_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (ws, doc) = row?;
                labels.entry(ws).or_default().push(parse_id(doc)?);
            }
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, name, color, icon, sort_key, created_at FROM workspaces
             ORDER BY sort_key IS NULL, sort_key, name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, color, icon, sort_key, created_at) = row?;
            let doc_ids = labels.remove(&id).unwrap_or_default();
            out.push(Workspace { id: parse_id(id)?, name, color, icon, sort_key, created_at, doc_ids });
        }
        Ok(out)
    }

    pub fn get_workspace(&self, id: Uuid) -> Result<Workspace> {
        self.list_workspaces()?
            .into_iter()
            .find(|w| w.id == id)
            .ok_or_else(|| StoreError::NotFound(format!("workspace {id}")))
    }

    /// By id, or by name case-insensitively.
    pub fn find_workspace(&self, name_or_id: &str) -> Result<Option<Workspace>> {
        let key = name_or_id.trim();
        let id = Uuid::parse_str(key).ok();
        Ok(self
            .list_workspaces()?
            .into_iter()
            .find(|w| Some(w.id) == id || w.name.to_lowercase() == key.to_lowercase()))
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

    /// Create a workspace; no sort key appends it after the last one.
    pub fn create_workspace(&mut self, name: &str, color: Option<&str>, icon: Option<&str>, sort_key: Option<&str>) -> Result<Workspace> {
        let name = clean_name(name)?;
        let color = clean_attr("color", color)?;
        let icon = clean_attr("icon", icon)?;
        let sort_key = match clean_sort_key(sort_key)? {
            Some(k) => k,
            None => {
                let last: Option<String> = self.conn.query_row("SELECT max(sort_key) FROM workspaces", [], |r| r.get(0))?;
                crate::order_key::between(last.as_deref(), None)
            }
        };
        let id = Uuid::now_v7();
        self.conn
            .execute(
                "INSERT INTO workspaces (id, name, color, icon, sort_key) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id.to_string(), name, color, icon, sort_key],
            )
            .map_err(|e| name_taken(e, &name))?;
        self.get_workspace(id)
    }

    /// Update a workspace's fields; journals its docs (their chips change).
    pub fn update_workspace(&mut self, id: Uuid, patch: WorkspacePatch) -> Result<Workspace> {
        let cur = self.get_workspace(id)?;
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
        let docs = self.workspace_doc_ids(WorkspaceFilter::Id(id))?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE workspaces SET name = ?1, color = ?2, icon = ?3, sort_key = ?4 WHERE id = ?5",
            params![name, color, icon, sort_key, id.to_string()],
        )
        .map_err(|e| name_taken(e, &name))?;
        emit_tree_conn(&tx, &docs.into_iter().collect::<Vec<_>>())?;
        tx.commit()?;
        self.get_workspace(id)
    }

    /// Delete a workspace: its labels go (CASCADE), its docs go Unsorted or
    /// to an outer workspace; no doc is touched. Returns the labels removed.
    pub fn delete_workspace(&mut self, id: Uuid) -> Result<usize> {
        self.get_workspace(id)?;
        let docs = self.workspace_doc_ids(WorkspaceFilter::Id(id))?;
        let tx = self.conn.unchecked_transaction()?;
        let labels = tx.execute("DELETE FROM doc_workspace WHERE workspace_id = ?1", params![id.to_string()])?;
        tx.execute("DELETE FROM workspaces WHERE id = ?1", params![id.to_string()])?;
        emit_tree_conn(&tx, &docs.into_iter().collect::<Vec<_>>())?;
        tx.commit()?;
        Ok(labels)
    }

    /// Label `doc` (Some) or clear its label (None). Journals the doc and
    /// its live subtree. Returns the doc's resolved workspace afterwards.
    pub fn set_doc_workspace(&mut self, doc: Uuid, workspace: Option<Uuid>) -> Result<Option<Uuid>> {
        crate::BlockStore::get_doc(self, doc)?;
        if let Some(w) = workspace {
            self.get_workspace(w)?;
        }
        let tx = self.conn.unchecked_transaction()?;
        let before: Option<String> = tx
            .query_row("SELECT workspace_id FROM doc_workspace WHERE doc_id = ?1", params![doc.to_string()], |r| r.get(0))
            .optional()?;
        if before.as_deref() != workspace.map(|w| w.to_string()).as_deref() {
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
        let out = resolve_conn(&tx, doc)?;
        tx.commit()?;
        Ok(out)
    }

    /// The doc's explicit label, if any.
    pub fn doc_label(&self, doc: Uuid) -> Result<Option<Uuid>> {
        let ws: Option<String> = self
            .conn
            .query_row("SELECT workspace_id FROM doc_workspace WHERE doc_id = ?1", params![doc.to_string()], |r| r.get(0))
            .optional()?;
        ws.map(parse_id).transpose()
    }

    /// The doc's resolved workspace (None = Unsorted).
    pub fn doc_workspace(&self, doc: Uuid) -> Result<Option<Uuid>> {
        resolve_conn(&self.conn, doc)
    }

    /// Every live doc's resolved workspace.
    pub fn workspace_map(&self) -> Result<HashMap<Uuid, Option<Uuid>>> {
        map_conn(&self.conn)
    }

    /// The live docs a filter keeps (subtree semantics).
    pub fn workspace_doc_ids(&self, filter: WorkspaceFilter) -> Result<HashSet<Uuid>> {
        let want = filter.as_option();
        Ok(map_conn(&self.conn)?.into_iter().filter(|(_, ws)| *ws == want).map(|(id, _)| id).collect())
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

        s.set_doc_workspace(a, Some(work.id)).unwrap();
        assert_eq!(s.doc_workspace(c).unwrap(), Some(work.id), "inherited from the nearest labelled ancestor");
        s.set_doc_workspace(b, Some(home.id)).unwrap();
        assert_eq!(s.doc_workspace(a).unwrap(), Some(work.id));
        assert_eq!(s.doc_workspace(b).unwrap(), Some(home.id), "a deeper label overrides");
        assert_eq!(s.doc_workspace(c).unwrap(), Some(home.id));
        let map = s.workspace_map().unwrap();
        assert_eq!(map[&c], Some(home.id));
        assert_eq!(map[&other], None);
        assert_eq!(s.workspace_doc_ids(WorkspaceFilter::Id(work.id)).unwrap(), HashSet::from([a]));
        assert_eq!(s.workspace_doc_ids(WorkspaceFilter::Unsorted).unwrap(), HashSet::from([other]));

        // a move re-resolves, and journals the descendants whose answer changed
        s.set_doc_workspace(b, None).unwrap();
        let seq = s.latest_change_seq().unwrap();
        s.move_doc(b, Some(other), None).unwrap();
        assert_eq!(s.doc_workspace(c).unwrap(), None);
        let rows = tree_rows_after(&s, seq);
        assert!(rows.contains(&b.to_string()) && rows.contains(&c.to_string()), "{rows:?}");

        s.set_doc_workspace(other, Some(work.id)).unwrap();
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
        s.set_doc_workspace(a, Some(w.id)).unwrap();
        assert_eq!(tree_rows_after(&s, seq), vec![a.to_string(), b.to_string()]);
        let page = s.changes_since(seq, 10).unwrap();
        assert_eq!(page.changes[1].doc.as_ref().unwrap().workspace_id, Some(w.id.to_string()), "the summary carries the resolved workspace");

        let seq = s.latest_change_seq().unwrap();
        s.set_doc_workspace(a, Some(w.id)).unwrap();
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
