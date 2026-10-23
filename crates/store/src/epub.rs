//! Docs sent to e-readers (`Taisce › EPUB proposal`, Phase 2): which docs
//! are books in a person's OPDS feed, and the diagram pictures their EPUBs
//! carry (books.sql `epub_image`).
//!
//! A doc is *sent* for a person when it carries the frontmatter tag
//! [`EREADER_TAG`] (doc_tags) AND that person set it: the ledger op that
//! last brought the tag into the doc's frontmatter was made by the person's
//! own principal (their human, or one of their agents). So a doc in a shared
//! workspace shows up on exactly the e-readers of whoever tagged it, never on
//! every member's. System and Local (LOCAL mode's one person) take the tag
//! alone.
//!
//! Every read runs in the store's scope: a doc the scope cannot see is
//! NotFound (by id) or absent (lists), exactly like every other doc path.
//! Diagram images are kept per uploader (`owner_key`): a render for one
//! person only ever uses pictures that person uploaded, so nobody can put a
//! picture into somebody else's book.

use crate::sqlite::{SqliteStore, block_by_id_conn, frontmatter_tags};
use crate::tenancy::INSTANCE_OWNER_SQL;
use crate::{Block, BlockStore, Doc, OpKind, Result, StoreError};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

/// The tag that sends a doc (and, for a folder, its whole subtree as one
/// book) to the person's e-readers.
pub const EREADER_TAG: &str = "ereader";

/// One stored diagram picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpubImage {
    pub doc_id: Uuid,
    pub block_id: Uuid,
    /// SHA-256 hex of the fence it was drawn from (the daemon's
    /// `epub::source_hash`); a fence whose source moved on no longer matches
    pub source_hash: String,
    pub png: Vec<u8>,
    pub width: i64,
    pub height: i64,
}

impl SqliteStore {
    /// The `owner_key` of the scope's diagram pictures: the signed-in user,
    /// or '' for LOCAL mode's one person (and System).
    fn image_owner_key(&self) -> Result<String> {
        self.deny_public()?;
        Ok(self.scope.user().map(|u| u.to_string()).unwrap_or_default())
    }

    /// Does `doc` carry `tag` (any of its live frontmatter blocks)?
    /// NotFound when the scope cannot see it.
    pub fn doc_has_tag(&self, doc: Uuid, tag: &str) -> Result<bool> {
        self.see(doc)?;
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM doc_tags t JOIN blocks b ON b.id = t.block_id AND b.deleted = 0
                 WHERE t.doc_id = ?1 AND t.tag = ?2)",
            params![doc.to_string(), tag.to_lowercase()],
            |r| r.get::<_, bool>(0),
        )?)
    }

    /// The auth user a principal acts for: a person's own human principal
    /// (auth_users.principal_id), else the principal's owner (an agent's
    /// `owner_user`; NULL = the instance owner). None for a database with
    /// no users or a principal nobody owns.
    fn principal_user(&self, principal: Uuid) -> Result<Option<Uuid>> {
        let id: Option<String> = self
            .conn
            .query_row(
                &format!(
                    "SELECT COALESCE(
                         (SELECT id FROM auth_users WHERE principal_id = ?1 ORDER BY created_at, id LIMIT 1),
                         (SELECT CASE WHEN kind IN ('human', 'agent') THEN COALESCE(owner_user, {INSTANCE_OWNER_SQL}) END
                            FROM principals WHERE id = ?1))"
                ),
                params![principal.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(id.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    /// Who put the [`EREADER_TAG`] into the frontmatter block `block`: the
    /// principal of the newest applied op that took the block from untagged
    /// (or absent) to tagged. Later edits that keep the tag (another field
    /// changed) do not move it. None when the ledger holds no such op.
    fn ereader_tagger(&self, block: Uuid) -> Result<Option<Uuid>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT op_type, payload, prior, principal FROM ops
             WHERE target_block = ?1 AND epoch_applied IS NOT NULL ORDER BY rowid DESC",
        )?;
        let rows = stmt.query_map(params![block.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?))
        })?;
        let tagged = |content: &str| frontmatter_tags(content).iter().any(|t| t == EREADER_TAG);
        for row in rows {
            let (op_type, payload, prior, principal) = row?;
            let principal = Uuid::parse_str(&principal).ok();
            match op_type.as_str() {
                "insert" => return Ok(principal),
                "replace" => {
                    let now_tagged = match serde_json::from_str::<OpKind>(&payload) {
                        Ok(OpKind::Replace { content, .. }) => tagged(&content),
                        _ => false,
                    };
                    let was_tagged = prior
                        .as_deref()
                        .and_then(|p| serde_json::from_str::<Block>(p).ok())
                        .is_some_and(|b| tagged(&b.content));
                    if now_tagged && !was_tagged {
                        return Ok(principal);
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// Is `doc` a book in the scope's e-reader feed: tagged [`EREADER_TAG`]
    /// and (for a signed-in user) tagged by that user? NotFound when the
    /// scope cannot see the doc.
    pub fn ereader_sent(&self, doc: Uuid) -> Result<bool> {
        self.see(doc)?;
        let blocks: Vec<String> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT t.block_id FROM doc_tags t JOIN blocks b ON b.id = t.block_id AND b.deleted = 0
                 WHERE t.doc_id = ?1 AND t.tag = ?2 ORDER BY t.block_id",
            )?;
            stmt.query_map(params![doc.to_string(), EREADER_TAG], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        if blocks.is_empty() {
            return Ok(false);
        }
        let Some(user) = self.scope.user() else {
            // System and Local: the tag is the whole rule
            return Ok(true);
        };
        for b in blocks {
            let Ok(block) = Uuid::parse_str(&b) else { continue };
            let tagger = match self.ereader_tagger(block)? {
                Some(p) => Some(p),
                // no ledger op (an imported row): the block's creator
                None => block_by_id_conn(&self.conn, block)?.map(|b| b.created_by),
            };
            if let Some(p) = tagger
                && self.principal_user(p)? == Some(user)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every doc that is a book in the scope's e-reader feed (see
    /// [`Self::ereader_sent`]), by title.
    pub fn ereader_sent_docs(&self) -> Result<Vec<Doc>> {
        let mut out = Vec::new();
        for d in self.docs_by_tag(EREADER_TAG)? {
            if self.ereader_sent(d.id)? {
                out.push(d);
            }
        }
        Ok(out)
    }

    /// Store (or replace) the scope's picture of one diagram fence. The
    /// block must be a live block of `doc`, which the scope must see;
    /// whether the hash is the fence's CURRENT one is the daemon's check.
    #[allow(clippy::too_many_arguments)]
    pub fn epub_image_put(
        &mut self,
        doc: Uuid,
        block: Uuid,
        source_hash: &str,
        png: &[u8],
        width: i64,
        height: i64,
        now: i64,
    ) -> Result<()> {
        self.see(doc)?;
        let owner = self.image_owner_key()?;
        match block_by_id_conn(&self.conn, block)? {
            Some(b) if b.doc_id == doc && !b.deleted => {}
            _ => return Err(StoreError::NotFound(format!("block {block}"))),
        }
        let tx = self.conn.unchecked_transaction()?;
        // one picture per fence: an older drawing of the block goes
        tx.execute(
            "DELETE FROM epub_image WHERE owner_key = ?1 AND block_id = ?2",
            params![owner, block.to_string()],
        )?;
        tx.execute(
            "INSERT INTO epub_image (owner_key, doc_id, block_id, source_hash, png, width, height, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![owner, doc.to_string(), block.to_string(), source_hash, png, width, height, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The scope's own pictures for these docs (any the scope cannot see
    /// are skipped), by block id.
    pub fn epub_images(&self, docs: &[Uuid]) -> Result<Vec<EpubImage>> {
        let owner = self.image_owner_key()?;
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare_cached(
            "SELECT block_id, source_hash, png, width, height FROM epub_image
             WHERE owner_key = ?1 AND doc_id = ?2 ORDER BY block_id",
        )?;
        for d in docs {
            if self.see(*d).is_err() {
                continue;
            }
            let rows = stmt.query_map(params![owner, d.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?))
            })?;
            for row in rows {
                let (block, source_hash, png, width, height) = row?;
                let Ok(block_id) = Uuid::parse_str(&block) else { continue };
                out.push(EpubImage { doc_id: *d, block_id, source_hash, png, width, height });
            }
        }
        Ok(out)
    }

    /// The newest applied change among these docs, as the ledger stamps it
    /// (`YYYY-MM-DDTHH:MM:SS.sssZ`), else the newest creation time; None
    /// when none is visible. Docs the scope cannot see are skipped.
    pub fn docs_last_change(&self, docs: &[Uuid]) -> Result<Option<String>> {
        let mut best: Option<String> = None;
        for d in docs {
            if self.see(*d).is_err() {
                continue;
            }
            let t: Option<String> = self.conn.query_row(
                "SELECT COALESCE(
                     (SELECT max(created_at) FROM ops WHERE doc_id = ?1 AND epoch_applied IS NOT NULL),
                     (SELECT created_at FROM docs WHERE id = ?1))",
                params![d.to_string()],
                |r| r.get(0),
            )?;
            if let Some(t) = t
                && best.as_ref().is_none_or(|b| t > *b)
            {
                best = Some(t);
            }
        }
        Ok(best)
    }

    /// The name of the workspace `doc` resolves to (None = Unsorted).
    pub fn doc_workspace_name(&self, doc: Uuid) -> Result<Option<String>> {
        let Some(ws) = self.doc_workspace(doc)? else { return Ok(None) };
        Ok(self
            .conn
            .query_row("SELECT name FROM workspaces WHERE id = ?1", params![ws.to_string()], |r| r.get(0))
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockType, OpInput, PrincipalKind, Role, Scope};

    fn insert(content: &str) -> OpInput {
        OpInput {
            kind: OpKind::Insert {
                block_id: Uuid::now_v7(),
                parent_id: None,
                order_key: String::new(),
                block_type: BlockType::Paragraph,
                content: content.into(),
                refers_to: None,
            },
            source_refs: vec![],
        }
    }

    const FM: &str = "---\ntags:\n  - ereader\n---";

    /// A owns a workspace shared with B (editor); each has a principal.
    fn two() -> (SqliteStore, Uuid, Uuid, Uuid, Uuid, Uuid) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let a_h = s.create_principal(PrincipalKind::Human, "Tom", None).unwrap().id;
        let a = s.auth_ensure_owner(a_h, "Tom", 1).unwrap().id;
        let bu = s.auth_add_user("Aoife", 1).unwrap();
        let ws = s.with_scope_for_test(Scope::User(a), |s| {
            let ws = s.create_workspace("Family", None, None, None).unwrap().id;
            s.share_workspace(ws, bu.id, Role::Editor).unwrap();
            ws
        });
        (s, a, a_h, bu.id, bu.principal_id, ws)
    }

    #[test]
    fn a_tag_sends_only_for_whoever_set_it() {
        let (mut s, a, a_h, b, b_h, ws) = two();
        let (shared, mine) = s.with_scope_for_test(Scope::User(a), |s| {
            let (d, _) = s.create_doc_with_ops("Shared plan", None, a_h, vec![insert("body")]).unwrap();
            s.set_doc_workspace(d.id, Some(ws), a_h).unwrap();
            let (m, _) = s.create_doc_with_ops("Mine", None, a_h, vec![insert(FM), insert("x")]).unwrap();
            (d.id, m.id)
        });
        // B tags the shared doc
        s.with_scope_for_test(Scope::User(b), |s| {
            let e = s.get_doc(shared).unwrap().current_epoch;
            s.propose(shared, e, b_h, vec![insert(FM)]).unwrap();
            assert!(s.doc_has_tag(shared, EREADER_TAG).unwrap());
            assert!(s.ereader_sent(shared).unwrap(), "B tagged it");
            assert_eq!(s.ereader_sent_docs().unwrap().iter().map(|d| d.id).collect::<Vec<_>>(), vec![shared]);
            // A's own doc is invisible to B: NotFound, never false
            assert!(matches!(s.ereader_sent(mine), Err(StoreError::NotFound(_))));
        });
        s.with_scope_for_test(Scope::User(a), |s| {
            assert!(!s.ereader_sent(shared).unwrap(), "A sees the tag but did not set it");
            assert_eq!(s.ereader_sent_docs().unwrap().iter().map(|d| d.id).collect::<Vec<_>>(), vec![mine]);
        });
        // Local/System: the tag alone
        assert!(s.ereader_sent(shared).unwrap() && s.ereader_sent(mine).unwrap());
    }

    #[test]
    fn an_edit_that_keeps_the_tag_keeps_the_tagger() {
        let (mut s, a, a_h, b, b_h, ws) = two();
        let (d, fm) = s.with_scope_for_test(Scope::User(a), |s| {
            let op = insert(FM);
            let fm = op.kind.target_block().unwrap();
            let (d, _) = s.create_doc_with_ops("Plan", None, a_h, vec![op]).unwrap();
            s.set_doc_workspace(d.id, Some(ws), a_h).unwrap();
            (d.id, fm)
        });
        s.with_scope_for_test(Scope::User(b), |s| {
            let e = s.get_doc(d).unwrap().current_epoch;
            let new = "---\ntags:\n  - ereader\n  - plans\n---".to_string();
            s.propose(d, e, b_h, vec![OpInput { kind: OpKind::Replace { target: fm, content: new }, source_refs: vec![] }]).unwrap();
            assert!(!s.ereader_sent(d).unwrap(), "B only added another tag");
        });
        s.with_scope_for_test(Scope::User(a), |s| assert!(s.ereader_sent(d).unwrap()));
    }

    #[test]
    fn images_are_the_uploaders_and_follow_visibility() {
        let (mut s, a, a_h, b, _b_h, ws) = two();
        let (private, shared, pb, sb) = s.with_scope_for_test(Scope::User(a), |s| {
            let op = insert("```mermaid\ngraph TD\n```");
            let pb = op.kind.target_block().unwrap();
            let (p, _) = s.create_doc_with_ops("Private", None, a_h, vec![op]).unwrap();
            let op = insert("```mermaid\ngraph LR\n```");
            let sb = op.kind.target_block().unwrap();
            let (d, _) = s.create_doc_with_ops("Shared", None, a_h, vec![op]).unwrap();
            s.set_doc_workspace(d.id, Some(ws), a_h).unwrap();
            s.epub_image_put(p.id, pb, "h1", b"png-a", 10, 5, 1).unwrap();
            s.epub_image_put(d.id, sb, "h2", b"png-a2", 10, 5, 1).unwrap();
            // a block of another doc is refused
            assert!(s.epub_image_put(p.id, sb, "h", b"x", 1, 1, 1).is_err());
            (p.id, d.id, pb, sb)
        });
        s.with_scope_for_test(Scope::User(b), |s| {
            assert!(matches!(s.epub_image_put(private, pb, "h", b"x", 1, 1, 1), Err(StoreError::NotFound(_))));
            assert!(s.epub_images(&[private, shared]).unwrap().is_empty(), "A's pictures are A's");
            s.epub_image_put(shared, sb, "h3", b"png-b", 10, 5, 2).unwrap();
            let got = s.epub_images(&[private, shared]).unwrap();
            assert_eq!(got.len(), 1);
            assert_eq!((got[0].png.as_slice(), got[0].source_hash.as_str()), (&b"png-b"[..], "h3"));
        });
        s.with_scope_for_test(Scope::User(a), |s| {
            let got = s.epub_images(&[private, shared]).unwrap();
            assert_eq!(got.iter().map(|i| i.png.clone()).collect::<Vec<_>>(), vec![b"png-a".to_vec(), b"png-a2".to_vec()]);
            assert!(s.docs_last_change(&[private, shared]).unwrap().is_some());
            assert_eq!(s.doc_workspace_name(shared).unwrap().as_deref(), Some("Family"));
            assert_eq!(s.doc_workspace_name(private).unwrap(), None);
        });
        s.with_scope_for_test(Scope::Public, |s| assert!(s.epub_images(&[private]).is_err()));
    }
}
