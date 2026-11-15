//! Share links (shares.sql): a read-only snapshot of a doc at `/s/<token>`.
//!
//! The owner's side runs in the person's `Scope::User`: creating one needs
//! the doc to be visible (`see`), and every other call finds only the
//! caller's own shares (someone else's is `NotFound`, never 403). The
//! public side (`share_by_token_hash` and the `share_public_*` calls) runs
//! in `Scope::Public` and reads ONLY the share tables: a snapshot is what
//! the owner uploaded, never the live doc.

use crate::scope::Scope;
use crate::sqlite::SqliteStore;
use crate::{Result, StoreError};
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

/// One image of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareAsset {
    pub name: String,
    pub content_type: String,
    pub data: Vec<u8>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

/// One sub-page of a snapshot: a descendant of the link's doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePage {
    /// the doc it was taken from (the caller must be able to write it, and
    /// it must sit under the link's doc)
    pub doc_id: Uuid,
    /// its name in the page's URL (the daemon derives it; never the doc id)
    pub slug: String,
    /// the page above it; None = directly under the link's root
    pub parent_slug: Option<String>,
    pub title: String,
    pub markdown: String,
}

/// A sub-page as the page's navigation needs it (no markdown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePageMeta {
    pub slug: String,
    pub parent_slug: Option<String>,
    pub title: String,
}

/// What the owner publishes, validated by the daemon. The page is rendered
/// from `markdown` when it is served.
#[derive(Debug, Clone)]
pub struct ShareSnapshot {
    pub title: String,
    pub markdown: String,
    /// light | dark | auto
    pub theme: String,
    /// every page's images, in one namespace
    pub assets: Vec<ShareAsset>,
    /// the doc's sub-pages, in tree order (a parent before its children);
    /// empty = a single-doc link
    pub pages: Vec<SharePage>,
}

impl ShareSnapshot {
    /// What it costs against the owner's cap: markdown (every page's) and
    /// image bytes.
    pub fn bytes(&self) -> i64 {
        (self.markdown.len()
            + self.pages.iter().map(|p| p.markdown.len()).sum::<usize>()
            + self.assets.iter().map(|a| a.data.len()).sum::<usize>()) as i64
    }
}

/// A person's live links (not revoked, not expired) and their snapshot bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareUsage {
    pub live: i64,
    pub bytes: i64,
}

/// What rendering a link's page needs: its markdown and its images' names
/// and sizes (no bytes).
#[derive(Debug, Clone)]
pub struct RenderInput {
    pub markdown: String,
    pub assets: Vec<(String, Option<i64>, Option<i64>)>,
}

/// A share as its owner sees it (the public side uses the same row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub doc_id: Uuid,
    pub title: String,
    pub theme: String,
    pub revision: i64,
    pub comments_enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub snapshot_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub views: i64,
    pub last_viewed_at: Option<i64>,
    pub comment_count: i64,
    pub unread_comments: i64,
    /// the owner's display name (a reader cannot comment under it)
    pub owner_name: String,
    /// sub-pages published with the doc (0 = a single-doc link)
    pub page_count: i64,
}

impl Share {
    /// Revoked, or past its expiry: the link answers 410.
    pub fn is_gone(&self, now: i64) -> bool {
        self.revoked_at.is_some() || self.expires_at.is_some_and(|e| e <= now)
    }
}

/// An absent field is kept; `expires_at: Some(None)` clears the expiry.
#[derive(Debug, Clone, Default)]
pub struct SharePatch {
    pub snapshot: Option<ShareSnapshot>,
    pub expires_at: Option<Option<i64>>,
    pub comments_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShareComment {
    pub id: Uuid,
    pub share_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author: String,
    pub is_owner: bool,
    pub body: String,
    pub anchor: Option<serde_json::Value>,
    pub created_at: i64,
    pub revision: i64,
    /// the sub-page (its slug) the comment is on; None = the root page
    pub page: Option<String>,
}

/// A new comment, validated by the daemon.
#[derive(Debug, Clone)]
pub struct NewShareComment {
    pub author: String,
    pub body: String,
    pub parent_id: Option<Uuid>,
    pub anchor: Option<serde_json::Value>,
    /// SHA-256 of (share, client ip); None for the owner's replies
    pub ip_hash: Option<String>,
    /// the sub-page (slug) it is on; None = the root page. A reply takes
    /// its thread's page whatever this says.
    pub page: Option<String>,
}

/// Recent public comments on a share, for the rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentCounts {
    /// from this ip in the last hour / day
    pub ip_hour: i64,
    pub ip_day: i64,
    /// from anyone in the last day
    pub share_day: i64,
}

const SHARE_COLS: &str = "s.id, s.owner_id, s.doc_id, s.title, s.theme, s.revision, s.comments_enabled,
    s.created_at, s.updated_at, s.snapshot_at, s.expires_at, s.revoked_at, s.views, s.last_viewed_at,
    (SELECT count(*) FROM share_link_comments c WHERE c.share_id = s.id),
    (SELECT count(*) FROM share_link_comments c WHERE c.share_id = s.id AND c.is_owner = 0 AND c.read_at IS NULL),
    (SELECT u.name FROM auth_users u WHERE u.id = s.owner_id),
    (SELECT count(*) FROM share_link_pages p WHERE p.share_id = s.id)";

fn uuid_at(r: &Row, i: usize) -> rusqlite::Result<Uuid> {
    let s: String = r.get(i)?;
    Uuid::parse_str(&s).map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e)))
}

fn share_row(r: &Row) -> rusqlite::Result<Share> {
    Ok(Share {
        id: uuid_at(r, 0)?,
        owner_id: uuid_at(r, 1)?,
        doc_id: uuid_at(r, 2)?,
        title: r.get(3)?,
        theme: r.get(4)?,
        revision: r.get(5)?,
        comments_enabled: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        snapshot_at: r.get(9)?,
        expires_at: r.get(10)?,
        revoked_at: r.get(11)?,
        views: r.get(12)?,
        last_viewed_at: r.get(13)?,
        comment_count: r.get(14)?,
        unread_comments: r.get(15)?,
        owner_name: r.get::<_, Option<String>>(16)?.unwrap_or_default(),
        page_count: r.get(17)?,
    })
}

const COMMENT_COLS: &str = "id, share_id, parent_id, author, is_owner, body, anchor, created_at, revision, page";

fn comment_row(r: &Row) -> rusqlite::Result<ShareComment> {
    let parent: Option<String> = r.get(2)?;
    let anchor: Option<String> = r.get(6)?;
    Ok(ShareComment {
        id: uuid_at(r, 0)?,
        share_id: uuid_at(r, 1)?,
        parent_id: parent.and_then(|p| Uuid::parse_str(&p).ok()),
        author: r.get(3)?,
        is_owner: r.get(4)?,
        body: r.get(5)?,
        anchor: anchor.and_then(|a| serde_json::from_str(&a).ok()),
        created_at: r.get(7)?,
        revision: r.get(8)?,
        page: r.get(9)?,
    })
}

/// When `user` leaves workspace `ws`: revoke their live links to docs that
/// now resolve to it (they can no longer publish what they cannot read).
pub(crate) fn revoke_on_unshare(conn: &rusqlite::Connection, ws: Uuid, user: Uuid) -> Result<usize> {
    let links: Vec<(String, String)> = {
        let mut st = conn.prepare("SELECT id, doc_id FROM share_links WHERE owner_id = ?1 AND revoked_at IS NULL")?;
        st.query_map(params![user.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mut n = 0;
    for (id, doc) in links {
        // the link's doc, and every sub-page's (a sub-page can carry a
        // workspace label of its own)
        let mut docs = vec![doc];
        {
            let mut st = conn.prepare_cached("SELECT doc_id FROM share_link_pages WHERE share_id = ?1")?;
            let pages = st.query_map(params![id], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            docs.extend(pages);
        }
        let in_ws = docs.iter().filter_map(|d| Uuid::parse_str(d).ok()).any(|d| {
            crate::tenancy::space_conn(conn, d).ok() == Some(crate::tenancy::Space::Workspace(ws))
        });
        if in_ws {
            n += revoke_conn(conn, "id = ?1", &id, now)?;
        }
    }
    Ok(n)
}

/// A dead link is deleted outright this long after it was revoked or expired.
pub const PURGE_AFTER: i64 = 30 * 86400;
/// A commenter's IP hash is kept this long (the longest rate-limit window).
pub const IP_HASH_TTL: i64 = 86400;

/// Revoke the links matching `pred` (one `?1` parameter): the snapshot's
/// markdown, sub-pages and images go now, the row stays (410) until the purge.
pub(crate) fn revoke_conn(conn: &rusqlite::Connection, pred: &str, arg: &str, now: i64) -> Result<usize> {
    conn.execute(
        &format!(
            "DELETE FROM share_link_assets WHERE share_id IN (SELECT id FROM share_links WHERE {pred} AND revoked_at IS NULL)"
        ),
        params![arg],
    )?;
    conn.execute(
        &format!(
            "DELETE FROM share_link_pages WHERE share_id IN (SELECT id FROM share_links WHERE {pred} AND revoked_at IS NULL)"
        ),
        params![arg],
    )?;
    Ok(conn.execute(
        &format!(
            "UPDATE share_links SET revoked_at = ?2, updated_at = ?2, markdown = '', snapshot_bytes = 0
             WHERE {pred} AND revoked_at IS NULL"
        ),
        params![arg, now],
    )?)
}

fn not_found(id: Uuid) -> StoreError {
    StoreError::NotFound(format!("share {id}"))
}

impl SqliteStore {
    /// The person a share belongs to: the scope's user. LOCAL mode has no
    /// share links (the routes answer 404), so neither does a userless scope.
    fn share_owner(&self) -> Result<Uuid> {
        self.scope.user().ok_or_else(|| StoreError::InvalidOp("share links need a signed-in user".into()))
    }

    /// One of the caller's own shares; anyone else's is NotFound.
    pub fn share_get(&self, id: Uuid) -> Result<Share> {
        let owner = match self.scope {
            Scope::System => None,
            _ => Some(self.share_owner().map_err(|_| not_found(id))?),
        };
        self.conn
            .query_row(
                &format!("SELECT {SHARE_COLS} FROM share_links s WHERE s.id = ?1 AND (?2 IS NULL OR s.owner_id = ?2)"),
                params![id.to_string(), owner.map(|o| o.to_string())],
                share_row,
            )
            .optional()?
            .ok_or_else(|| not_found(id))
    }

    /// The caller's live links and their bytes, leaving out `except` (the
    /// link a PATCH replaces).
    pub fn share_usage(&self, now: i64, except: Option<Uuid>) -> Result<ShareUsage> {
        let owner = self.share_owner()?;
        let (live, bytes) = self.conn.query_row(
            "SELECT count(*), COALESCE(sum(snapshot_bytes), 0) FROM share_links
             WHERE owner_id = ?1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?2)
               AND (?3 IS NULL OR id != ?3)",
            params![owner.to_string(), now, except.map(|e| e.to_string())],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(ShareUsage { live, bytes })
    }

    /// The bytes a link's snapshot holds (the caller's own link, else NotFound).
    pub fn share_snapshot_bytes(&self, id: Uuid) -> Result<i64> {
        self.share_get(id)?;
        Ok(self.conn.query_row("SELECT snapshot_bytes FROM share_links WHERE id = ?1", [id.to_string()], |r| r.get(0))?)
    }

    /// The caller's shares, newest first; of one doc when `doc` is given.
    pub fn shares_list(&self, doc: Option<Uuid>) -> Result<Vec<Share>> {
        let owner = self.share_owner()?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {SHARE_COLS} FROM share_links s WHERE s.owner_id = ?1 AND (?2 IS NULL OR s.doc_id = ?2)
             ORDER BY s.created_at DESC, s.id DESC"
        ))?;
        let rows = st.query_map(params![owner.to_string(), doc.map(|d| d.to_string())], share_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn put_assets(tx: &rusqlite::Transaction, share: Uuid, assets: &[ShareAsset]) -> Result<()> {
        tx.execute("DELETE FROM share_link_assets WHERE share_id = ?1", params![share.to_string()])?;
        let mut ins = tx.prepare(
            "INSERT INTO share_link_assets (share_id, name, content_type, data, width, height) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for a in assets {
            ins.execute(params![share.to_string(), a.name, a.content_type, a.data, a.width, a.height])?;
        }
        Ok(())
    }

    /// Every sub-page is a doc the caller may write, under the link's doc:
    /// a link publishes nothing its maker could not have changed, and
    /// nothing from outside the tree it names.
    fn check_pages(&self, root: Uuid, pages: &[SharePage]) -> Result<()> {
        if pages.is_empty() {
            return Ok(());
        }
        let under: std::collections::HashSet<Uuid> = crate::workspaces::descendants_conn(&self.conn, root)?.into_iter().collect();
        let (mut docs, mut slugs) = (std::collections::HashSet::new(), std::collections::HashSet::new());
        for p in pages {
            self.may_write(p.doc_id)?;
            if !under.contains(&p.doc_id) {
                return Err(StoreError::InvalidOp(format!("page {}: not a sub-page of the shared doc", p.doc_id)));
            }
            if !docs.insert(p.doc_id) || !slugs.insert(p.slug.as_str()) {
                return Err(StoreError::InvalidOp(format!("page {}: appears twice", p.doc_id)));
            }
        }
        Ok(())
    }

    fn put_pages(tx: &rusqlite::Transaction, share: Uuid, pages: &[SharePage]) -> Result<()> {
        tx.execute("DELETE FROM share_link_pages WHERE share_id = ?1", params![share.to_string()])?;
        let mut ins = tx.prepare(
            "INSERT INTO share_link_pages (share_id, slug, doc_id, parent_slug, position, title, markdown) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (i, p) in pages.iter().enumerate() {
            ins.execute(params![share.to_string(), p.slug, p.doc_id.to_string(), p.parent_slug, i as i64, p.title, p.markdown])?;
        }
        Ok(())
    }

    /// Publish a snapshot of `doc` (which the caller must be able to see)
    /// as link `id`; `token_hash` is the SHA-256 of the token derived from
    /// `id` (the token itself is never stored).
    #[allow(clippy::too_many_arguments)]
    pub fn share_create(
        &mut self,
        id: Uuid,
        doc: Uuid,
        token_hash: &str,
        snap: &ShareSnapshot,
        expires_at: Option<i64>,
        comments_enabled: bool,
        now: i64,
    ) -> Result<Share> {
        let owner = self.share_owner()?;
        // owner or editor of the doc's workspace (Unsorted: its owner):
        // invisible is NotFound, read-only is Forbidden
        self.may_write(doc)?;
        self.check_pages(doc, &snap.pages)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO share_links (id, owner_id, doc_id, token_hash, title, markdown, theme,
                 revision, comments_enabled, created_at, updated_at, snapshot_at, expires_at, snapshot_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, ?9, ?9, ?10, ?11)",
            params![
                id.to_string(),
                owner.to_string(),
                doc.to_string(),
                token_hash,
                snap.title,
                snap.markdown,
                snap.theme,
                comments_enabled,
                now,
                expires_at,
                snap.bytes()
            ],
        )?;
        Self::put_assets(&tx, id, &snap.assets)?;
        Self::put_pages(&tx, id, &snap.pages)?;
        tx.commit()?;
        self.share_get(id)
    }

    /// Change one of the caller's live shares. A new snapshot bumps the
    /// revision (the token stays). A revoked share cannot be changed.
    pub fn share_update(&mut self, id: Uuid, patch: SharePatch, now: i64) -> Result<Share> {
        let cur = self.share_get(id)?;
        if cur.revoked_at.is_some() {
            return Err(StoreError::InvalidOp("this link was revoked; make a new one".into()));
        }
        // still owner or editor of the doc: a link is not a way to keep
        // publishing a doc you can no longer change
        self.may_write(cur.doc_id)?;
        if let Some(snap) = &patch.snapshot {
            self.check_pages(cur.doc_id, &snap.pages)?;
        }
        let tx = self.conn.unchecked_transaction()?;
        if let Some(snap) = &patch.snapshot {
            tx.execute(
                "UPDATE share_links SET title = ?2, markdown = ?3, theme = ?4, revision = revision + 1,
                     snapshot_at = ?5, updated_at = ?5, snapshot_bytes = ?6 WHERE id = ?1",
                params![id.to_string(), snap.title, snap.markdown, snap.theme, now, snap.bytes()],
            )?;
            Self::put_assets(&tx, id, &snap.assets)?;
            Self::put_pages(&tx, id, &snap.pages)?;
        }
        if let Some(exp) = patch.expires_at {
            tx.execute("UPDATE share_links SET expires_at = ?2, updated_at = ?3 WHERE id = ?1", params![id.to_string(), exp, now])?;
        }
        if let Some(on) = patch.comments_enabled {
            tx.execute("UPDATE share_links SET comments_enabled = ?2, updated_at = ?3 WHERE id = ?1", params![id.to_string(), on, now])?;
        }
        tx.commit()?;
        self.share_get(id)
    }

    /// Revoke for good: the row stays (with `revoked_at`), the link is dead.
    /// The snapshot (markdown and images) goes at once; the row stays so the
    /// link answers 410, and the comments until the purge.
    pub fn share_revoke(&mut self, id: Uuid, now: i64) -> Result<()> {
        self.share_get(id)?;
        let tx = self.conn.unchecked_transaction()?;
        revoke_conn(&tx, "id = ?1", &id.to_string(), now)?;
        tx.commit()?;
        Ok(())
    }

    /// Housekeeping (System): links revoked or expired more than 30 days
    /// ago go entirely (comments included); comment IP hashes older than a
    /// day are cleared (the day limit no longer needs them). Returns
    /// (links purged, hashes cleared).
    pub fn share_maintenance(&mut self, now: i64) -> Result<(usize, usize)> {
        if self.scope != Scope::System {
            return Err(StoreError::InvalidOp("share maintenance runs as System".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        let purged = tx.execute(
            "DELETE FROM share_links WHERE revoked_at < ?1 OR expires_at < ?1",
            params![now - PURGE_AFTER],
        )?;
        let cleared = tx.execute(
            "UPDATE share_link_comments SET ip_hash = NULL WHERE ip_hash IS NOT NULL AND created_at < ?1",
            params![now - IP_HASH_TTL],
        )?;
        tx.commit()?;
        Ok((purged, cleared))
    }

    fn comments_of(&self, id: Uuid) -> Result<Vec<ShareComment>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {COMMENT_COLS} FROM share_link_comments WHERE share_id = ?1 ORDER BY created_at, id"))?;
        let rows = st.query_map(params![id.to_string()], comment_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn pages_of(&self, id: Uuid) -> Result<Vec<SharePageMeta>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT slug, parent_slug, title FROM share_link_pages WHERE share_id = ?1 ORDER BY position, slug")?;
        let rows = st.query_map(params![id.to_string()], |r| Ok(SharePageMeta { slug: r.get(0)?, parent_slug: r.get(1)?, title: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The sub-pages of one of the caller's shares, in tree order.
    pub fn share_pages(&self, id: Uuid) -> Result<Vec<SharePageMeta>> {
        self.share_get(id)?;
        self.pages_of(id)
    }

    /// The comments on one of the caller's shares (a read: nothing changes).
    pub fn share_comments_for_owner(&self, id: Uuid) -> Result<Vec<ShareComment>> {
        self.share_get(id)?;
        self.comments_of(id)
    }

    /// Mark every comment on one of the caller's shares read; how many were
    /// unread.
    pub fn share_mark_read(&mut self, id: Uuid, now: i64) -> Result<usize> {
        self.share_get(id)?;
        Ok(self.conn.execute(
            "UPDATE share_link_comments SET read_at = ?2 WHERE share_id = ?1 AND read_at IS NULL",
            params![id.to_string(), now],
        )?)
    }

    /// A reply is threaded under its root: a parent that is itself a reply
    /// resolves to that reply's parent. Unknown parents are refused.
    /// Also the page the thread is on (a reply is on its thread's page).
    fn thread_root(&self, share: Uuid, parent: Option<Uuid>) -> Result<Option<(Uuid, Option<String>)>> {
        let Some(p) = parent else { return Ok(None) };
        let row: Option<(Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT parent_id, page FROM share_link_comments WHERE id = ?1 AND share_id = ?2",
                params![p.to_string(), share.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            None => Err(StoreError::InvalidOp("parent_id: no such comment on this link".into())),
            Some((Some(root), page)) => Ok(Some((Uuid::parse_str(&root).unwrap_or(p), page))),
            Some((None, page)) => Ok(Some((p, page))),
        }
    }

    fn insert_comment(&mut self, share: &Share, c: &NewShareComment, is_owner: bool, now: i64) -> Result<ShareComment> {
        let (parent, page) = match self.thread_root(share.id, c.parent_id)? {
            Some((root, page)) => (Some(root), page),
            None => (None, c.page.clone()),
        };
        if let Some(slug) = &page {
            // a new thread names a page the link has (a reply's thread may
            // sit on a page a later snapshot dropped)
            let known: bool = c.parent_id.is_some()
                || self.conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM share_link_pages WHERE share_id = ?1 AND slug = ?2)",
                    params![share.id.to_string(), slug],
                    |r| r.get(0),
                )?;
            if !known {
                return Err(StoreError::InvalidOp("page: no such page on this link".into()));
            }
        }
        let id = Uuid::now_v7();
        let anchor = c.anchor.as_ref().map(|a| a.to_string());
        let read_at = is_owner.then_some(now);
        self.conn.execute(
            "INSERT INTO share_link_comments (id, share_id, parent_id, author, is_owner, body, anchor, created_at, revision, ip_hash, read_at, page)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id.to_string(),
                share.id.to_string(),
                parent.map(|p| p.to_string()),
                c.author,
                is_owner,
                c.body,
                anchor,
                now,
                share.revision,
                c.ip_hash,
                read_at,
                page
            ],
        )?;
        Ok(ShareComment {
            id,
            share_id: share.id,
            parent_id: parent,
            author: c.author.clone(),
            is_owner,
            body: c.body.clone(),
            anchor: c.anchor.clone(),
            created_at: now,
            revision: share.revision,
            page,
        })
    }

    /// The owner's reply on one of their shares, signed with their name.
    pub fn share_owner_reply(&mut self, id: Uuid, mut c: NewShareComment, now: i64) -> Result<ShareComment> {
        let share = self.share_get(id)?;
        let owner = self.share_owner()?;
        c.author = self.auth_user(owner)?.map(|u| u.name).unwrap_or_else(|| "Owner".into());
        c.ip_hash = None;
        self.insert_comment(&share, &c, true, now)
    }

    /// Delete a comment (and its replies) on one of the caller's shares.
    pub fn share_delete_comment(&mut self, id: Uuid, comment: Uuid) -> Result<()> {
        self.share_get(id)?;
        let n = self.conn.execute(
            "DELETE FROM share_link_comments WHERE share_id = ?1 AND (id = ?2 OR parent_id = ?2)",
            params![id.to_string(), comment.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("comment {comment}")));
        }
        Ok(())
    }

    // ---- the public side: share tables only ----

    fn public_only(&self) -> Result<()> {
        match self.scope {
            Scope::Public | Scope::System => Ok(()),
            _ => Err(StoreError::InvalidOp("the public share reads run in Scope::Public".into())),
        }
    }

    /// The share a link's token names (by the token's SHA-256), live or not.
    pub fn share_by_token_hash(&self, hash: &str) -> Result<Option<Share>> {
        self.public_only()?;
        Ok(self
            .conn
            .query_row(&format!("SELECT {SHARE_COLS} FROM share_links s WHERE s.token_hash = ?1"), params![hash], share_row)
            .optional()?)
    }

    /// A link's sub-pages (slug, parent, title), in tree order.
    pub fn share_public_pages(&self, id: Uuid) -> Result<Vec<SharePageMeta>> {
        self.public_only()?;
        self.pages_of(id)
    }

    /// What a page is rendered from: the root's (`page` None) or a
    /// sub-page's markdown, and the link's images.
    pub fn share_public_render_input(&self, id: Uuid, page: Option<&str>) -> Result<RenderInput> {
        self.public_only()?;
        let markdown: Option<String> = match page {
            None => self.conn.query_row("SELECT markdown FROM share_links WHERE id = ?1", params![id.to_string()], |r| r.get(0)).optional()?,
            Some(slug) => self
                .conn
                .query_row("SELECT markdown FROM share_link_pages WHERE share_id = ?1 AND slug = ?2", params![id.to_string(), slug], |r| r.get(0))
                .optional()?,
        };
        let markdown = markdown.ok_or_else(|| not_found(id))?;
        let mut st = self.conn.prepare("SELECT name, width, height FROM share_link_assets WHERE share_id = ?1")?;
        let assets = st
            .query_map(params![id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(RenderInput { markdown, assets })
    }

    /// Count a page view: at most one a minute per link, so a busy (or
    /// hammered) link costs one write a minute, not one per request.
    pub fn share_public_viewed(&mut self, id: Uuid, now: i64) -> Result<bool> {
        self.public_only()?;
        let n = self.conn.execute(
            "UPDATE share_links SET views = views + 1, last_viewed_at = ?2
             WHERE id = ?1 AND (last_viewed_at IS NULL OR last_viewed_at < ?2 - 60)",
            params![id.to_string(), now],
        )?;
        Ok(n > 0)
    }

    pub fn share_public_asset(&self, id: Uuid, name: &str) -> Result<Option<ShareAsset>> {
        self.public_only()?;
        Ok(self
            .conn
            .query_row(
                "SELECT name, content_type, data, width, height FROM share_link_assets WHERE share_id = ?1 AND name = ?2",
                params![id.to_string(), name],
                |r| Ok(ShareAsset { name: r.get(0)?, content_type: r.get(1)?, data: r.get(2)?, width: r.get(3)?, height: r.get(4)? }),
            )
            .optional()?)
    }

    /// The comments on one page of a share (`page` None = the root page),
    /// oldest first.
    pub fn share_public_comments(&self, id: Uuid, page: Option<&str>) -> Result<Vec<ShareComment>> {
        self.public_only()?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {COMMENT_COLS} FROM share_link_comments WHERE share_id = ?1 AND page IS ?2 ORDER BY created_at, id"
        ))?;
        let rows = st.query_map(params![id.to_string(), page], comment_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How many public comments `ip_hash` (and everyone) left lately.
    pub fn share_comment_counts(&self, id: Uuid, ip_hash: &str, now: i64) -> Result<CommentCounts> {
        self.public_only()?;
        let (ip_hour, ip_day, share_day) = self.conn.query_row(
            "SELECT
                 count(CASE WHEN ip_hash = ?2 AND created_at > ?3 THEN 1 END),
                 count(CASE WHEN ip_hash = ?2 AND created_at > ?4 THEN 1 END),
                 count(CASE WHEN ip_hash IS NOT NULL AND created_at > ?4 THEN 1 END)
             FROM share_link_comments WHERE share_id = ?1 AND created_at > ?4",
            params![id.to_string(), ip_hash, now - 3600, now - 86400],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(CommentCounts { ip_hour, ip_day, share_day })
    }

    /// A reader's comment on a live share whose comments are on (the daemon
    /// checks; this re-checks so a race cannot slip one past a revoke).
    pub fn share_public_comment(&mut self, id: Uuid, c: NewShareComment, now: i64) -> Result<ShareComment> {
        self.public_only()?;
        let share: Share = self
            .conn
            .query_row(&format!("SELECT {SHARE_COLS} FROM share_links s WHERE s.id = ?1"), params![id.to_string()], share_row)
            .optional()?
            .ok_or_else(|| not_found(id))?;
        if share.is_gone(now) {
            return Err(StoreError::NotFound(format!("share {id}")));
        }
        if !share.comments_enabled {
            return Err(StoreError::Forbidden("comments are off for this link".into()));
        }
        self.insert_comment(&share, &c, false, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockStore, PrincipalKind};

    /// Federation's old `shares` table is dropped on every open; share
    /// links live under another name and survive a reopen.
    #[test]
    fn share_links_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ks.db");
        let id = {
            let mut s = SqliteStore::open(&path).unwrap();
            let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
            let a = s.auth_ensure_owner(p, "Tom", 1).unwrap().id;
            let doc = s.create_doc_with_ops("D", None, p, vec![]).unwrap().0.id;
            s.with_scope_for_test(Scope::User(a), |s| s.share_create(Uuid::now_v7(), doc, "hash", &snap("D"), None, true, 10).unwrap().id)
        };
        let s = SqliteStore::open(&path).unwrap();
        assert_eq!(s.share_get(id).unwrap().title, "D");
        assert!(s.share_public_asset(id, "d1.svg").unwrap().is_some());
    }

    fn snap(title: &str) -> ShareSnapshot {
        ShareSnapshot {
            title: title.into(),
            markdown: "# hi".into(),
            theme: "auto".into(),
            assets: vec![ShareAsset { name: "d1.svg".into(), content_type: "image/svg+xml".into(), data: b"<svg/>".to_vec(), width: Some(10), height: None }],
            pages: vec![],
        }
    }

    /// A doc deleted for good (there is no such path today; the trigger
    /// covers any future one) revokes its links; trashing does not.
    #[test]
    fn a_purged_doc_revokes_its_links() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let a = s.auth_ensure_owner(p, "Tom", 1).unwrap().id;
        let doc = s.create_doc_with_ops("D", None, p, vec![]).unwrap().0.id;
        let id = s.with_scope_for_test(Scope::User(a), |s| s.share_create(Uuid::now_v7(), doc, "h", &snap("D"), None, true, 0).unwrap().id);
        s.delete_doc(doc).unwrap();
        assert!(s.share_get(id).unwrap().revoked_at.is_none(), "the trash keeps the link");
        s.conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        s.conn.execute("DELETE FROM docs WHERE id = ?1", [doc.to_string()]).unwrap();
        let sh = s.share_get(id).unwrap();
        assert!(sh.revoked_at.is_some(), "purged doc → revoked link");
        assert!(s.with_scope_for_test(Scope::Public, |s| s.share_public_asset(id, "d1.svg")).unwrap().is_none());
    }

    #[test]
    fn revoke_drops_the_snapshot_and_the_purge_drops_the_rest() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let a = s.auth_ensure_owner(p, "Tom", 1).unwrap().id;
        let doc = s.create_doc_with_ops("D", None, p, vec![]).unwrap().0.id;
        let day = 86400;
        let mk = |s: &mut SqliteStore, hash: &str, exp: Option<i64>| {
            s.with_scope_for_test(Scope::User(a), |s| s.share_create(Uuid::now_v7(), doc, hash, &snap("D"), exp, true, 0).unwrap().id)
        };
        let (old, recent, expired, live) = (mk(&mut s, "h1", None), mk(&mut s, "h2", None), mk(&mut s, "h3", Some(1)), mk(&mut s, "h4", None));
        let c = NewShareComment { author: "R".into(), body: "b".into(), parent_id: None, anchor: None, ip_hash: Some("ip".into()), page: None };
        s.with_scope_for_test(Scope::Public, |s| {
            s.share_public_comment(old, c.clone(), 5).unwrap();
            s.share_public_comment(live, c.clone(), 39 * day).unwrap();
            s.share_public_comment(live, c.clone(), 40 * day + day / 2).unwrap();
        });
        s.with_scope_for_test(Scope::User(a), |s| {
            s.share_revoke(old, 10).unwrap();
            s.share_revoke(recent, 39 * day).unwrap();
        });
        // revoked: no images, no markdown, the row (and its comments) kept
        s.with_scope_for_test(Scope::Public, |s| {
            assert!(s.share_public_asset(old, "d1.svg").unwrap().is_none());
            assert_eq!(s.share_public_render_input(old, None).unwrap().markdown, "");
            assert_eq!(s.share_public_comments(old, None).unwrap().len(), 1);
        });
        assert!(s.with_scope_for_test(Scope::User(a), |s| s.share_maintenance(41 * day)).is_err(), "System only");
        let (purged, cleared) = s.share_maintenance(41 * day).unwrap();
        assert_eq!(purged, 2, "revoked and expired more than 30 days ago");
        assert!(s.share_get(old).is_err() && s.share_get(expired).is_err());
        assert!(s.share_get(recent).is_ok() && s.share_get(live).is_ok());
        let left: i64 = s.conn.query_row("SELECT count(*) FROM share_link_comments WHERE share_id = ?1", [old.to_string()], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "the comments went with it");
        assert_eq!(cleared, 1, "only the hash older than a day");
        let hashes: i64 = s.conn.query_row("SELECT count(ip_hash) FROM share_link_comments", [], |r| r.get(0)).unwrap();
        assert_eq!(hashes, 1);
    }

    fn page(doc: Uuid, slug: &str, parent: Option<&str>, title: &str) -> SharePage {
        SharePage { doc_id: doc, slug: slug.into(), parent_slug: parent.map(Into::into), title: title.into(), markdown: format!("text of {title}") }
    }

    /// A link to a doc and its sub-pages: only docs under it that the maker
    /// may write; each page reads on its own; comments are per page; a new
    /// snapshot replaces the pages; revoking drops them.
    #[test]
    fn a_link_carries_the_docs_sub_pages() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let a = s.auth_ensure_owner(p, "Tom", 1).unwrap().id;
        let b = s.auth_add_user("Aoife", 1).unwrap().id;
        let (root, kid, grandkid, other, hers) = s.with_scope_for_test(Scope::User(a), |s| {
            let root = s.create_doc_with_ops("Root", None, p, vec![]).unwrap().0.id;
            let kid = s.create_doc_with_ops("Kid", Some(root), p, vec![]).unwrap().0.id;
            let grandkid = s.create_doc_with_ops("Grandkid", Some(kid), p, vec![]).unwrap().0.id;
            let other = s.create_doc_with_ops("Elsewhere", None, p, vec![]).unwrap().0.id;
            (root, kid, grandkid, other, ())
        });
        let _ = hers;
        let b_doc = s.with_scope_for_test(Scope::User(b), |s| s.create_doc_with_ops("Hers", None, p, vec![]).unwrap().0.id);
        let with = |pages: Vec<SharePage>| ShareSnapshot { pages, ..snap("Root") };
        s.with_scope_for_test(Scope::User(a), |s| {
            // not under the root, someone else's, the root itself, a repeat
            for bad in [
                vec![page(other, "s1", None, "Elsewhere")],
                vec![page(root, "s1", None, "Root")],
                vec![page(kid, "s1", None, "Kid"), page(kid, "s2", None, "Kid")],
                vec![page(kid, "s1", None, "Kid"), page(grandkid, "s1", None, "Grandkid")],
            ] {
                let e = s.share_create(Uuid::now_v7(), root, "h-bad", &with(bad), None, true, 0);
                assert!(matches!(e, Err(StoreError::InvalidOp(_))), "{e:?}");
            }
            let e = s.share_create(Uuid::now_v7(), root, "h-bad", &with(vec![page(b_doc, "s1", None, "Hers")]), None, true, 0);
            assert!(matches!(e, Err(StoreError::NotFound(_))), "a doc she cannot see is not there: {e:?}");
            assert!(s.shares_list(None).unwrap().is_empty(), "a refused link stores nothing");
        });
        let snap2 = with(vec![page(kid, "kid", None, "Kid"), page(grandkid, "grand", Some("kid"), "Grandkid")]);
        let sh = s.with_scope_for_test(Scope::User(a), |s| s.share_create(Uuid::now_v7(), root, "hash", &snap2, None, true, 10).unwrap());
        assert_eq!(sh.page_count, 2);
        assert_eq!(snap2.bytes(), ("# hi".len() + "<svg/>".len() + "text of Kid".len() + "text of Grandkid".len()) as i64);
        s.with_scope_for_test(Scope::User(b), |s| assert!(s.share_pages(sh.id).is_err(), "B sees none of A's pages"));
        s.with_scope_for_test(Scope::Public, |s| {
            let pages = s.share_public_pages(sh.id).unwrap();
            assert_eq!(pages.iter().map(|p| (p.slug.as_str(), p.parent_slug.as_deref(), p.title.as_str())).collect::<Vec<_>>(), [
                ("kid", None, "Kid"),
                ("grand", Some("kid"), "Grandkid")
            ]);
            assert_eq!(s.share_public_render_input(sh.id, None).unwrap().markdown, "# hi");
            assert_eq!(s.share_public_render_input(sh.id, Some("grand")).unwrap().markdown, "text of Grandkid");
            assert!(s.share_public_render_input(sh.id, Some("nope")).is_err());
            // a comment per page; a reply joins its thread's page
            let c = |page: Option<&str>, parent| NewShareComment { author: "R".into(), body: "b".into(), parent_id: parent, anchor: None, ip_hash: None, page: page.map(Into::into) };
            let on_kid = s.share_public_comment(sh.id, c(Some("kid"), None), 20).unwrap();
            s.share_public_comment(sh.id, c(None, None), 21).unwrap();
            let reply = s.share_public_comment(sh.id, c(None, Some(on_kid.id)), 22).unwrap();
            assert_eq!(reply.page.as_deref(), Some("kid"));
            assert!(matches!(s.share_public_comment(sh.id, c(Some("nope"), None), 23), Err(StoreError::InvalidOp(_))));
            assert_eq!(s.share_public_comments(sh.id, Some("kid")).unwrap().len(), 2);
            assert_eq!(s.share_public_comments(sh.id, None).unwrap().len(), 1);
            assert!(s.share_public_comments(sh.id, Some("grand")).unwrap().is_empty());
        });
        s.with_scope_for_test(Scope::User(a), |s| {
            assert_eq!(s.share_comments_for_owner(sh.id).unwrap().len(), 3, "the owner reads every page's");
            // a new snapshot without the grandkid: the page is gone
            let up = s.share_update(sh.id, SharePatch { snapshot: Some(with(vec![page(kid, "kid", None, "Kid 2")])), ..Default::default() }, 30).unwrap();
            assert_eq!((up.page_count, up.revision), (1, 2));
            assert_eq!(s.share_pages(sh.id).unwrap()[0].title, "Kid 2");
            // a sub-page moved out from under the root is refused on the next snapshot
            let e = s.share_update(sh.id, SharePatch { snapshot: Some(with(vec![page(other, "o", None, "Elsewhere")])), ..Default::default() }, 31);
            assert!(matches!(e, Err(StoreError::InvalidOp(_))), "{e:?}");
            s.share_revoke(sh.id, 40).unwrap();
            assert_eq!(s.share_get(sh.id).unwrap().page_count, 0, "revoking drops the pages");
        });
    }

    /// A sub-page's doc deleted for good leaves the link: the page goes, the
    /// link stays at a new revision.
    #[test]
    fn a_purged_sub_page_leaves_the_link() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let a = s.auth_ensure_owner(p, "Tom", 1).unwrap().id;
        let root = s.create_doc_with_ops("Root", None, p, vec![]).unwrap().0.id;
        let kid = s.create_doc_with_ops("Kid", Some(root), p, vec![]).unwrap().0.id;
        let snap = ShareSnapshot { pages: vec![page(kid, "kid", None, "Kid")], ..snap("Root") };
        let id = s.with_scope_for_test(Scope::User(a), |s| s.share_create(Uuid::now_v7(), root, "h", &snap, None, true, 0).unwrap().id);
        s.conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        s.conn.execute("DELETE FROM docs WHERE id = ?1", [kid.to_string()]).unwrap();
        let sh = s.share_get(id).unwrap();
        assert_eq!((sh.page_count, sh.revision, sh.revoked_at), (0, 2, None));
        let bytes: i64 = s.conn.query_row("SELECT snapshot_bytes FROM share_links WHERE id = ?1", [id.to_string()], |r| r.get(0)).unwrap();
        assert_eq!(bytes, ("# hi".len() + "<svg/>".len()) as i64);
    }

    #[test]
    fn owner_only_and_public_reads_only_the_share() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let a = s.auth_ensure_owner(p, "Tom", 1).unwrap().id;
        let b = s.auth_add_user("Aoife", 1).unwrap().id;
        let doc = s.with_scope_for_test(Scope::User(a), |s| s.create_doc_with_ops("D", None, p, vec![]).unwrap().0.id);
        // B cannot share A's doc
        let e = s.with_scope_for_test(Scope::User(b), |s| s.share_create(Uuid::now_v7(), doc, "h", &snap("D"), None, true, 10));
        assert!(matches!(e, Err(StoreError::NotFound(_))), "{e:?}");
        let sh = s.with_scope_for_test(Scope::User(a), |s| s.share_create(Uuid::now_v7(), doc, "hash", &snap("D"), None, true, 10).unwrap());
        assert_eq!(sh.revision, 1);
        // B sees nothing of it
        s.with_scope_for_test(Scope::User(b), |s| {
            assert!(s.shares_list(None).unwrap().is_empty());
            assert!(matches!(s.share_get(sh.id), Err(StoreError::NotFound(_))));
            assert!(s.share_revoke(sh.id, 20).is_err());
            assert!(s.share_comments_for_owner(sh.id).is_err());
            assert!(s.share_mark_read(sh.id, 20).is_err());
            assert!(s.share_by_token_hash("hash").is_err(), "a user scope is not the public side");
        });
        // the public side by hash; a comment; the owner reads and it is read
        s.with_scope_for_test(Scope::Public, |s| {
            let got = s.share_by_token_hash("hash").unwrap().unwrap();
            assert_eq!(got.id, sh.id);
            assert!(s.share_public_asset(sh.id, "d1.svg").unwrap().is_some());
            assert!(s.get_doc(doc).is_err(), "Scope::Public sees no doc");
            let c = NewShareComment { author: "R".into(), body: "nice".into(), parent_id: None, anchor: None, ip_hash: Some("ip".into()), page: None };
            s.share_public_comment(sh.id, c, 30).unwrap();
            assert_eq!(s.share_comment_counts(sh.id, "ip", 31).unwrap(), CommentCounts { ip_hour: 1, ip_day: 1, share_day: 1 });
        });
        s.with_scope_for_test(Scope::User(a), |s| {
            assert_eq!(s.share_get(sh.id).unwrap().unread_comments, 1);
            assert_eq!(s.share_comments_for_owner(sh.id).unwrap().len(), 1);
            assert_eq!(s.share_get(sh.id).unwrap().unread_comments, 1, "reading is not marking");
            assert_eq!(s.share_mark_read(sh.id, 40).unwrap(), 1);
            assert_eq!(s.share_get(sh.id).unwrap().unread_comments, 0);
            let up = s.share_update(sh.id, SharePatch { snapshot: Some(snap("D2")), ..Default::default() }, 50).unwrap();
            assert_eq!((up.revision, up.title.as_str(), up.id), (2, "D2", sh.id));
            s.share_revoke(sh.id, 60).unwrap();
            assert!(s.share_get(sh.id).unwrap().is_gone(61));
            assert!(s.share_update(sh.id, SharePatch::default(), 70).is_err());
        });
    }
}
