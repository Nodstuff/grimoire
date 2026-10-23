//! The EPUB library (books.sql): each person's shelf (`book`), their
//! paired ereaders (`ereader`) and KOSYNC reading progress
//! (`kosync_progress`). File bytes and covers live in object storage; the
//! db keeps metadata and hashes only — the store never sees a secret (the
//! daemon passes SHA-256 hashes) nor a file. Times are unix seconds,
//! passed in by the caller (tests drive the clock).
//!
//! The owner's side runs in the person's `Scope::User`: inserts are keyed
//! to the caller, id-keyed calls find only the caller's own rows (someone
//! else's is `NotFound`, never 403), and user-keyed calls must name the
//! caller — System (and Local) may act for anyone, for maintenance and
//! the daemon's internal calls. The auth-resolution reads
//! (`ereader_find_by_secret_hash`, `ereader_find_kosync`) run before a
//! user scope exists: the hash IS the credential.

use crate::scope::Scope;
use crate::sqlite::SqliteStore;
use crate::{Result, StoreError};
use rusqlite::{OptionalExtension, Row, params, params_from_iter, types::Value};
use uuid::Uuid;

/// A book on someone's shelf (a live row; soft-deleted ones never read).
#[derive(Debug, Clone, PartialEq)]
pub struct Book {
    pub id: Uuid,
    pub user_id: Uuid,
    pub title: String,
    pub author: String,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub book_identifier: Option<String>,
    pub original_filename: String,
    pub file_sha256: String,
    /// file size in bytes
    pub size: i64,
    pub file_s3_key: String,
    pub cover_s3_key: Option<String>,
    pub added_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

/// What the daemon validated about a new upload; the store keys it to the
/// caller (`Scope::User`) and stamps it with `now`.
#[derive(Debug, Clone)]
pub struct NewBook {
    pub title: String,
    pub author: String,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub book_identifier: Option<String>,
    pub original_filename: String,
    pub file_sha256: String,
    pub size: i64,
    pub file_s3_key: String,
    pub cover_s3_key: Option<String>,
}

/// Keyset cursors for [`SqliteStore::book_list`]: `before` pages towards
/// older books, `after` towards newer ones; each is the exclusive
/// (added_at, id) of the book to continue from. Both `None` = first page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BookPage {
    pub before: Option<(i64, Uuid)>,
    pub after: Option<(i64, Uuid)>,
}

/// A paired ereader (revoked rows included: they stay listed, they just
/// no longer authenticate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ereader {
    pub id: Uuid,
    pub user_id: Uuid,
    pub label: String,
    pub secret_hash: String,
    pub kosync_key_hash: String,
    pub stream_downloads: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

/// Seconds between `last_used_at` writes for one ereader: a busy reader
/// costs one UPDATE a minute, not one per request.
pub const EREADER_TOUCH_EVERY: i64 = 60;

/// One person's progress in one document.
#[derive(Debug, Clone, PartialEq)]
pub struct KosyncProgress {
    pub user_id: Uuid,
    pub document: String,
    pub progress: String,
    pub percentage: f64,
    pub device: String,
    pub device_id: String,
    pub updated_at: i64,
}

const BOOK_COLS: &str = "id, user_id, title, author, series, series_index, book_identifier,
    original_filename, file_sha256, size, file_s3_key, cover_s3_key, added_at, updated_at, deleted_at";

const EREADER_COLS: &str = "e.id, e.user_id, e.label, e.secret_hash, e.kosync_key_hash,
    e.stream_downloads, e.created_at, e.last_used_at, e.revoked_at";

const KOSYNC_COLS: &str = "user_id, document, progress, percentage, device, device_id, updated_at";

fn uuid_at(r: &Row, i: usize) -> rusqlite::Result<Uuid> {
    let s: String = r.get(i)?;
    Uuid::parse_str(&s).map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e)))
}

fn book_row(r: &Row) -> rusqlite::Result<Book> {
    Ok(Book {
        id: uuid_at(r, 0)?,
        user_id: uuid_at(r, 1)?,
        title: r.get(2)?,
        author: r.get(3)?,
        series: r.get(4)?,
        series_index: r.get(5)?,
        book_identifier: r.get(6)?,
        original_filename: r.get(7)?,
        file_sha256: r.get(8)?,
        size: r.get(9)?,
        file_s3_key: r.get(10)?,
        cover_s3_key: r.get(11)?,
        added_at: r.get(12)?,
        updated_at: r.get(13)?,
        deleted_at: r.get(14)?,
    })
}

fn ereader_row(r: &Row) -> rusqlite::Result<Ereader> {
    Ok(Ereader {
        id: uuid_at(r, 0)?,
        user_id: uuid_at(r, 1)?,
        label: r.get(2)?,
        secret_hash: r.get(3)?,
        kosync_key_hash: r.get(4)?,
        stream_downloads: r.get(5)?,
        created_at: r.get(6)?,
        last_used_at: r.get(7)?,
        revoked_at: r.get(8)?,
    })
}

fn kosync_row(r: &Row) -> rusqlite::Result<KosyncProgress> {
    Ok(KosyncProgress {
        user_id: uuid_at(r, 0)?,
        document: r.get(1)?,
        progress: r.get(2)?,
        percentage: r.get(3)?,
        device: r.get(4)?,
        device_id: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

fn book_not_found(id: Uuid) -> StoreError {
    StoreError::NotFound(format!("book {id}"))
}

fn ereader_not_found(id: Uuid) -> StoreError {
    StoreError::NotFound(format!("ereader {id}"))
}

impl SqliteStore {
    /// The person a library write is keyed to: the scope's user. The
    /// library exists only for signed-in users (LOCAL mode has no
    /// auth_users rows to key shelves to).
    fn library_owner(&self) -> Result<Uuid> {
        self.deny_public()?;
        self.scope.user().ok_or_else(|| StoreError::InvalidOp("the library needs a signed-in user".into()))
    }

    /// The owner filter for an id-keyed call: System sees every row, a
    /// signed-in user only their own (anyone else's is NotFound).
    fn owner_scoped(&self, not_found: StoreError) -> Result<Option<Uuid>> {
        self.deny_public()?;
        match self.scope {
            Scope::System => Ok(None),
            _ => self.library_owner().map(Some).map_err(|_| not_found),
        }
    }

    /// The owner a user-keyed call runs for: System/Local may act for
    /// anyone (maintenance, the daemon's internal calls); a signed-in
    /// user only for themselves — anyone else's shelf is NotFound, never
    /// a permission error.
    fn user_scoped(&self, user: Uuid) -> Result<Uuid> {
        self.deny_public()?;
        match self.scope.user() {
            Some(u) if u == user => Ok(u),
            None if self.scope.sees_all() => Ok(user),
            _ => Err(StoreError::NotFound("no such book".into())),
        }
    }

    // ---- books ----

    /// Shelve a new book as the caller's own; `id` is minted by the daemon.
    pub fn book_insert(&mut self, id: Uuid, book: &NewBook, now: i64) -> Result<Book> {
        let owner = self.library_owner()?;
        self.conn.execute(
            "INSERT INTO book (id, user_id, title, author, series, series_index, book_identifier,
                 original_filename, file_sha256, size, file_s3_key, cover_s3_key, added_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
            params![
                id.to_string(),
                owner.to_string(),
                book.title,
                book.author,
                book.series,
                book.series_index,
                book.book_identifier,
                book.original_filename,
                book.file_sha256,
                book.size,
                book.file_s3_key,
                book.cover_s3_key,
                now
            ],
        )?;
        self.book_get(id)
    }

    /// One of the caller's own live books; anyone else's is NotFound.
    pub fn book_get(&self, id: Uuid) -> Result<Book> {
        let owner = self.owner_scoped(book_not_found(id))?;
        self.conn
            .query_row(
                &format!("SELECT {BOOK_COLS} FROM book WHERE id = ?1 AND deleted_at IS NULL AND (?2 IS NULL OR user_id = ?2)"),
                params![id.to_string(), owner.map(|o| o.to_string())],
                book_row,
            )
            .optional()?
            .ok_or_else(|| book_not_found(id))
    }

    /// A page of `user`'s live books, newest first (added_at desc, id
    /// desc). `search` matches title, author or series case-insensitively.
    pub fn book_list(&self, user: Uuid, page: &BookPage, limit: usize, search: Option<&str>) -> Result<Vec<Book>> {
        let owner = self.user_scoped(user)?;
        let mut sql = format!("SELECT {BOOK_COLS} FROM book WHERE user_id = ?1 AND deleted_at IS NULL");
        let mut args: Vec<Value> = vec![owner.to_string().into()];
        let mut next = 2;
        if let Some((at, id)) = page.before {
            sql.push_str(&format!(" AND (added_at < ?{0} OR (added_at = ?{0} AND id < ?{1}))", next, next + 1));
            args.push(at.into());
            args.push(id.to_string().into());
            next += 2;
        }
        if let Some((at, id)) = page.after {
            sql.push_str(&format!(" AND (added_at > ?{0} OR (added_at = ?{0} AND id > ?{1}))", next, next + 1));
            args.push(at.into());
            args.push(id.to_string().into());
            next += 2;
        }
        if let Some(q) = search.map(str::trim).filter(|q| !q.is_empty()) {
            let escaped = q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            sql.push_str(&format!(
                " AND (title LIKE ?{0} ESCAPE '\\' OR author LIKE ?{0} ESCAPE '\\' OR series LIKE ?{0} ESCAPE '\\')",
                next
            ));
            args.push(format!("%{escaped}%").into());
            next += 1;
        }
        sql.push_str(&format!(" ORDER BY added_at DESC, id DESC LIMIT ?{next}"));
        args.push((limit as i64).into());
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args), book_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replace one of the caller's live books' metadata (full-field, not a
    /// patch: `None` clears series/series_index); bumps `updated_at`.
    pub fn book_update_metadata(
        &mut self,
        id: Uuid,
        title: &str,
        author: &str,
        series: Option<&str>,
        series_index: Option<f64>,
        now: i64,
    ) -> Result<Book> {
        self.book_get(id)?;
        self.conn.execute(
            "UPDATE book SET title = ?2, author = ?3, series = ?4, series_index = ?5, updated_at = ?6 WHERE id = ?1",
            params![id.to_string(), title, author, series, series_index, now],
        )?;
        self.book_get(id)
    }

    /// Re-key one of the caller's live books to a freshly uploaded file:
    /// the hash, size and original filename follow the new bytes (the s3
    /// key stays — the object was replaced in place under bucket
    /// versioning), and `updated_at` moves. Without this a re-upload left
    /// the row pointing at the FIRST file's hash, so the dedup (rule (a),
    /// `book_find_by_sha256`) never matched the new bytes again.
    pub fn book_replace_file(&mut self, id: Uuid, sha256: &str, size: i64, original_filename: &str, now: i64) -> Result<Book> {
        self.book_get(id)?;
        self.conn.execute(
            "UPDATE book SET file_sha256 = ?2, size = ?3, original_filename = ?4, updated_at = ?5 WHERE id = ?1",
            params![id.to_string(), sha256, size, original_filename, now],
        )?;
        self.book_get(id)
    }

    /// Soft-delete one of the caller's live books: the row (and its s3
    /// keys) stays until the object-storage sweep; every read stops
    /// seeing it now.
    pub fn book_soft_delete(&mut self, id: Uuid, now: i64) -> Result<()> {
        self.book_get(id)?;
        self.conn
            .execute("UPDATE book SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1", params![id.to_string(), now])?;
        Ok(())
    }

    /// The newest live book on `user`'s shelf with this file hash (the
    /// re-upload dedup), if any.
    pub fn book_find_by_sha256(&self, user: Uuid, sha: &str) -> Result<Option<Book>> {
        let owner = self.user_scoped(user)?;
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {BOOK_COLS} FROM book WHERE user_id = ?1 AND file_sha256 = ?2 AND deleted_at IS NULL
                     ORDER BY added_at DESC, id DESC"
                ),
                params![owner.to_string(), sha],
                book_row,
            )
            .optional()?)
    }

    /// The newest live book on `user`'s shelf carrying this identifier.
    pub fn book_find_by_identifier(&self, user: Uuid, identifier: &str) -> Result<Option<Book>> {
        let owner = self.user_scoped(user)?;
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {BOOK_COLS} FROM book WHERE user_id = ?1 AND book_identifier = ?2 AND deleted_at IS NULL
                     ORDER BY added_at DESC, id DESC"
                ),
                params![owner.to_string(), identifier],
                book_row,
            )
            .optional()?)
    }

    /// The newest live book on `user`'s shelf with this title and author,
    /// matched case-insensitively.
    pub fn book_find_by_title_author(&self, user: Uuid, title: &str, author: &str) -> Result<Option<Book>> {
        let owner = self.user_scoped(user)?;
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {BOOK_COLS} FROM book
                     WHERE user_id = ?1 AND title = ?2 COLLATE NOCASE AND author = ?3 COLLATE NOCASE AND deleted_at IS NULL
                     ORDER BY added_at DESC, id DESC"
                ),
                params![owner.to_string(), title, author],
                book_row,
            )
            .optional()?)
    }

    /// Set (or replace) the cover of one of the caller's live books; the
    /// image bytes are uploaded to object storage by the daemon.
    pub fn book_set_cover(&mut self, id: Uuid, cover_s3_key: &str, now: i64) -> Result<Book> {
        self.book_get(id)?;
        self.conn
            .execute("UPDATE book SET cover_s3_key = ?2, updated_at = ?3 WHERE id = ?1", params![id.to_string(), cover_s3_key, now])?;
        self.book_get(id)
    }

    // ---- ereaders ----

    /// Pair a new ereader for the caller. `secret_hash`/`kosync_key_hash`
    /// are computed by the daemon; the store never sees the secrets.
    pub fn ereader_insert(
        &mut self,
        id: Uuid,
        label: &str,
        secret_hash: &str,
        kosync_key_hash: &str,
        stream_downloads: bool,
        now: i64,
    ) -> Result<Ereader> {
        let owner = self.library_owner()?;
        self.conn.execute(
            "INSERT INTO ereader (id, user_id, label, secret_hash, kosync_key_hash, stream_downloads, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id.to_string(), owner.to_string(), label, secret_hash, kosync_key_hash, stream_downloads, now],
        )?;
        self.ereader_get(id)
    }

    /// One of the caller's own ereaders (revoked included); anyone else's
    /// is NotFound.
    pub fn ereader_get(&self, id: Uuid) -> Result<Ereader> {
        let owner = self.owner_scoped(ereader_not_found(id))?;
        self.conn
            .query_row(
                &format!("SELECT {EREADER_COLS} FROM ereader e WHERE e.id = ?1 AND (?2 IS NULL OR e.user_id = ?2)"),
                params![id.to_string(), owner.map(|o| o.to_string())],
                ereader_row,
            )
            .optional()?
            .ok_or_else(|| ereader_not_found(id))
    }

    /// `user`'s ereaders, oldest first, revoked ones included.
    pub fn ereader_list(&self, user: Uuid) -> Result<Vec<Ereader>> {
        let owner = self.user_scoped(user)?;
        let mut st = self
            .conn
            .prepare(&format!("SELECT {EREADER_COLS} FROM ereader e WHERE e.user_id = ?1 ORDER BY e.created_at, e.id"))?;
        let rows = st.query_map(params![owner.to_string()], ereader_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record a use of one of the caller's live ereaders, unless one was
    /// recorded in the last minute. True when it wrote.
    pub fn ereader_touch_last_used(&mut self, id: Uuid, now: i64) -> Result<bool> {
        let owner = self.owner_scoped(ereader_not_found(id))?;
        let n = self.conn.execute(
            "UPDATE ereader SET last_used_at = ?2
             WHERE id = ?1 AND revoked_at IS NULL AND (?3 IS NULL OR user_id = ?3)
               AND (last_used_at IS NULL OR last_used_at <= ?2 - ?4)",
            params![id.to_string(), now, owner.map(|o| o.to_string()), EREADER_TOUCH_EVERY],
        )?;
        Ok(n == 1)
    }

    /// Re-key one of the caller's live ereaders (a lost secret): both
    /// hashes change at once, so the old ones stop resolving immediately.
    pub fn ereader_rotate(&mut self, id: Uuid, new_secret_hash: &str, new_kosync_key_hash: &str) -> Result<Ereader> {
        let cur = self.ereader_get(id)?;
        if cur.revoked_at.is_some() {
            return Err(StoreError::InvalidOp("this ereader was revoked; pair a new one".into()));
        }
        self.conn.execute(
            "UPDATE ereader SET secret_hash = ?2, kosync_key_hash = ?3 WHERE id = ?1",
            params![id.to_string(), new_secret_hash, new_kosync_key_hash],
        )?;
        self.ereader_get(id)
    }

    /// Revoke one of the caller's ereaders: the row stays listed, every
    /// resolution stops. Idempotent.
    pub fn ereader_revoke(&mut self, id: Uuid, now: i64) -> Result<()> {
        self.ereader_get(id)?;
        self.conn
            .execute("UPDATE ereader SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL", params![id.to_string(), now])?;
        Ok(())
    }

    /// The live (unrevoked) ereader behind a basic-auth secret's hash, its
    /// user still present. Runs before a user scope exists (the daemon's
    /// auth resolution): the hash is the credential.
    pub fn ereader_find_by_secret_hash(&self, secret_hash: &str) -> Result<Option<Ereader>> {
        self.deny_public()?;
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {EREADER_COLS} FROM ereader e
                     WHERE e.secret_hash = ?1 AND e.revoked_at IS NULL AND e.user_id IN (SELECT id FROM auth_users)"
                ),
                [secret_hash],
                ereader_row,
            )
            .optional()?)
    }

    /// The live ereader behind a KOSYNC login: the person's name (matched
    /// case-insensitively, as auth finds users) plus the key's hash.
    pub fn ereader_find_kosync(&self, username: &str, key_hash: &str) -> Result<Option<Ereader>> {
        self.deny_public()?;
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {EREADER_COLS} FROM ereader e JOIN auth_users u ON u.id = e.user_id
                     WHERE u.name = ?1 COLLATE NOCASE AND e.kosync_key_hash = ?2 AND e.revoked_at IS NULL"
                ),
                params![username, key_hash],
                ereader_row,
            )
            .optional()?)
    }

    // ---- kosync progress ----

    /// Record `user`'s progress in `document`, newest-wins: an incoming
    /// stamp older than (or equal to) the stored one leaves the row
    /// untouched. Either way, returns the row's final `updated_at` (the
    /// KOSYNC answer to a push).
    #[allow(clippy::too_many_arguments)]
    pub fn kosync_upsert(
        &mut self,
        user: Uuid,
        document: &str,
        progress: &str,
        percentage: f64,
        device: &str,
        device_id: &str,
        ts: i64,
    ) -> Result<i64> {
        let owner = self.user_scoped(user)?;
        self.conn.execute(
            "INSERT INTO kosync_progress (user_id, document, progress, percentage, device, device_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (user_id, document) DO UPDATE SET
                 progress = excluded.progress, percentage = excluded.percentage,
                 device = excluded.device, device_id = excluded.device_id,
                 updated_at = excluded.updated_at
             WHERE excluded.updated_at > kosync_progress.updated_at",
            params![owner.to_string(), document, progress, percentage, device, device_id, ts],
        )?;
        Ok(self.conn.query_row(
            "SELECT updated_at FROM kosync_progress WHERE user_id = ?1 AND document = ?2",
            params![owner.to_string(), document],
            |r| r.get(0),
        )?)
    }

    /// `user`'s progress in `document`, if any was ever pushed.
    pub fn kosync_get(&self, user: Uuid, document: &str) -> Result<Option<KosyncProgress>> {
        let owner = self.user_scoped(user)?;
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {KOSYNC_COLS} FROM kosync_progress WHERE user_id = ?1 AND document = ?2"),
                params![owner.to_string(), document],
                kosync_row,
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockStore, PrincipalKind};

    fn store_with_users() -> (SqliteStore, Uuid, Uuid) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let a = s.auth_ensure_owner(p, "Tom", 1).unwrap().id;
        let b = s.auth_add_user("Aoife", 1).unwrap().id;
        (s, a, b)
    }

    fn new_book(title: &str, author: &str) -> NewBook {
        NewBook {
            title: title.into(),
            author: author.into(),
            series: None,
            series_index: None,
            book_identifier: None,
            original_filename: format!("{title}.epub"),
            file_sha256: format!("sha256:{title}"),
            size: 4096,
            file_s3_key: format!("u/{title}.epub"),
            cover_s3_key: None,
        }
    }

    fn add_book(s: &mut SqliteStore, user: Uuid, book: &NewBook, now: i64) -> Book {
        s.with_scope_for_test(Scope::User(user), |s| s.book_insert(Uuid::now_v7(), book, now).unwrap())
    }

    fn titles(page: &[Book]) -> Vec<&str> {
        page.iter().map(|b| b.title.as_str()).collect()
    }

    /// The library tables survive a reopen (created IF NOT EXISTS by init;
    /// none is named `shares`, which an open drops).
    #[test]
    fn books_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ks.db");
        let (book_id, ereader_id) = {
            let mut s = SqliteStore::open(&path).unwrap();
            let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
            let a = s.auth_ensure_owner(p, "Tom", 1).unwrap().id;
            let book = add_book(&mut s, a, &new_book("Dune", "Frank Herbert"), 10);
            let ereader = s.with_scope_for_test(Scope::User(a), |s| {
                s.ereader_insert(Uuid::now_v7(), "kindle", "sh1", "kh1", true, 10).unwrap()
            });
            s.with_scope_for_test(Scope::User(a), |s| {
                s.kosync_upsert(a, "0123456789abcdef0123456789abcdef", "p", 0.5, "kindle", "k1", 20).unwrap()
            });
            (book.id, ereader.id)
        };
        let s = SqliteStore::open(&path).unwrap();
        assert_eq!(s.book_get(book_id).unwrap().title, "Dune");
        assert_eq!(s.ereader_get(ereader_id).unwrap().label, "kindle");
        let a = s.ereader_get(ereader_id).unwrap().user_id;
        assert_eq!(s.kosync_get(a, "0123456789abcdef0123456789abcdef").unwrap().unwrap().updated_at, 20);
    }

    #[test]
    fn books_insert_list_update_and_soft_delete() {
        let (mut s, a, _b) = store_with_users();
        let dune = add_book(&mut s, a, &new_book("Dune", "Frank Herbert"), 10);
        add_book(&mut s, a, &new_book("Dune Messiah", "Frank Herbert"), 20);
        let mut hobbit = new_book("The Hobbit", "J. R. R. Tolkien");
        hobbit.series = Some("Middle-earth".into());
        hobbit.series_index = Some(0.5);
        hobbit.book_identifier = Some("isbn:123".into());
        let hobbit = add_book(&mut s, a, &hobbit, 30);
        assert_eq!(
            (hobbit.series.as_deref(), hobbit.series_index, hobbit.book_identifier.as_deref(), hobbit.size, hobbit.deleted_at),
            (Some("Middle-earth"), Some(0.5), Some("isbn:123"), 4096, None)
        );

        s.with_scope_for_test(Scope::User(a), |s| {
            // newest first; the first page needs no cursor
            let page = s.book_list(a, &BookPage::default(), 10, None).unwrap();
            assert_eq!(titles(&page), vec!["The Hobbit", "Dune Messiah", "Dune"]);

            // keyset paging: `before` walks towards older books, `after` back
            let p1 = s.book_list(a, &BookPage::default(), 2, None).unwrap();
            assert_eq!(titles(&p1), vec!["The Hobbit", "Dune Messiah"]);
            let older = s.book_list(a, &BookPage { before: Some((p1[1].added_at, p1[1].id)), after: None }, 2, None).unwrap();
            assert_eq!(titles(&older), vec!["Dune"]);
            let back = s.book_list(a, &BookPage { before: None, after: Some((older[0].added_at, older[0].id)) }, 10, None).unwrap();
            assert_eq!(titles(&back), vec!["The Hobbit", "Dune Messiah"]);

            // search matches title, author and series, case-insensitively
            assert_eq!(titles(&s.book_list(a, &BookPage::default(), 10, Some("herbert")).unwrap()), vec!["Dune Messiah", "Dune"]);
            assert_eq!(titles(&s.book_list(a, &BookPage::default(), 10, Some("middle-EARTH")).unwrap()), vec!["The Hobbit"]);
            assert!(s.book_list(a, &BookPage::default(), 10, Some("asimov")).unwrap().is_empty());
            // a literal % in the search is not a wildcard
            assert!(s.book_list(a, &BookPage::default(), 10, Some("100%")).unwrap().is_empty());

            // a full-field metadata edit: clears series, bumps updated_at
            let up = s.book_update_metadata(hobbit.id, "The Hobbit, or There and Back Again", "J.R.R. Tolkien", None, None, 40).unwrap();
            assert_eq!((up.title.as_str(), up.series, up.updated_at, up.added_at), ("The Hobbit, or There and Back Again", None, 40, 30));
            let up = s.book_set_cover(hobbit.id, "u/hobbit.jpg", 41).unwrap();
            assert_eq!((up.cover_s3_key.as_deref(), up.updated_at), (Some("u/hobbit.jpg"), 41));

            // soft delete: gone from get/list/finds; a second delete is NotFound
            s.book_soft_delete(dune.id, 50).unwrap();
            assert!(matches!(s.book_get(dune.id), Err(StoreError::NotFound(_))));
            assert!(matches!(s.book_soft_delete(dune.id, 51), Err(StoreError::NotFound(_))));
            assert_eq!(s.book_list(a, &BookPage::default(), 10, None).unwrap().len(), 2);
            assert!(s.book_find_by_sha256(a, "sha256:Dune").unwrap().is_none());
        });
        // System still sees any user's live rows
        assert_eq!(s.book_list(a, &BookPage::default(), 10, None).unwrap().len(), 2);
    }

    #[test]
    fn book_replace_file_rekeys_the_row() {
        let (mut s, a, b) = store_with_users();
        let dune = add_book(&mut s, a, &new_book("Dune", "Frank Herbert"), 10);
        s.with_scope_for_test(Scope::User(a), |s| {
            let b2 = s.book_replace_file(dune.id, "sha256:dune-v2", 8192, "Dune (revised).epub", 20).unwrap();
            assert_eq!(
                (b2.file_sha256.as_str(), b2.size, b2.original_filename.as_str(), b2.updated_at, b2.added_at),
                ("sha256:dune-v2", 8192, "Dune (revised).epub", 20, 10)
            );
            // the dedup now matches the NEW bytes; the old hash finds nothing
            assert_eq!(s.book_find_by_sha256(a, "sha256:dune-v2").unwrap().unwrap().id, dune.id);
            assert!(s.book_find_by_sha256(a, "sha256:Dune").unwrap().is_none());
        });
        // another person's book is NotFound; System may still re-key it
        s.with_scope_for_test(Scope::User(b), |s| {
            assert!(matches!(s.book_replace_file(dune.id, "x", 1, "x.epub", 30), Err(StoreError::NotFound(_))));
        });
        let b3 = s.book_replace_file(dune.id, "sha256:dune-v3", 9000, "Dune v3.epub", 40).unwrap();
        assert_eq!(b3.file_sha256, "sha256:dune-v3");
        // a soft-deleted book cannot be re-keyed
        s.with_scope_for_test(Scope::User(a), |s| s.book_soft_delete(dune.id, 50).unwrap());
        assert!(matches!(
            s.book_replace_file(dune.id, "x", 1, "x.epub", 60),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn books_identity_finds() {
        let (mut s, a, b) = store_with_users();
        let mut dune = new_book("Dune", "Frank Herbert");
        dune.book_identifier = Some("urn:isbn:9780441172719".into());
        add_book(&mut s, a, &dune, 10);

        s.with_scope_for_test(Scope::User(a), |s| {
            assert_eq!(s.book_find_by_sha256(a, "sha256:Dune").unwrap().unwrap().title, "Dune");
            assert!(s.book_find_by_sha256(a, "sha256:other").unwrap().is_none());
            assert!(s.book_find_by_identifier(a, "urn:isbn:9780441172719").unwrap().is_some());
            assert!(s.book_find_by_identifier(a, "urn:isbn:000").unwrap().is_none());
            assert!(s.book_find_by_title_author(a, "dune", "frank HERBERT").unwrap().is_some(), "case-insensitive");
            assert!(s.book_find_by_title_author(a, "Dune", "Brian Herbert").unwrap().is_none());
        });
        s.with_scope_for_test(Scope::User(b), |s| {
            // another person's shelf is NotFound; your own is simply empty
            assert!(matches!(s.book_find_by_sha256(a, "sha256:Dune"), Err(StoreError::NotFound(_))));
            assert!(s.book_find_by_sha256(b, "sha256:Dune").unwrap().is_none());
            assert!(s.book_list(a, &BookPage::default(), 10, None).is_err());
            assert!(s.book_list(b, &BookPage::default(), 10, None).unwrap().is_empty());
        });
        // System resolves anyone's (maintenance, the daemon's internal calls)
        assert!(s.book_find_by_sha256(a, "sha256:Dune").unwrap().is_some());
    }

    #[test]
    fn owner_isolation_system_and_public() {
        let (mut s, a, b) = store_with_users();
        let dune = add_book(&mut s, a, &new_book("Dune", "Frank Herbert"), 10);
        let kindle = s.with_scope_for_test(Scope::User(a), |s| {
            s.ereader_insert(Uuid::now_v7(), "kindle", "sh1", "kh1", true, 10).unwrap()
        });

        s.with_scope_for_test(Scope::User(b), |s| {
            assert!(matches!(s.book_get(dune.id), Err(StoreError::NotFound(_))));
            assert!(matches!(s.book_update_metadata(dune.id, "x", "y", None, None, 20), Err(StoreError::NotFound(_))));
            assert!(matches!(s.book_replace_file(dune.id, "x", 1, "x.epub", 20), Err(StoreError::NotFound(_))));
            assert!(matches!(s.book_soft_delete(dune.id, 20), Err(StoreError::NotFound(_))));
            assert!(matches!(s.book_set_cover(dune.id, "k", 20), Err(StoreError::NotFound(_))));
            assert!(matches!(s.ereader_get(kindle.id), Err(StoreError::NotFound(_))));
            assert!(matches!(s.ereader_rotate(kindle.id, "x", "y"), Err(StoreError::NotFound(_))));
            assert!(matches!(s.ereader_revoke(kindle.id, 20), Err(StoreError::NotFound(_))));
            assert!(!s.ereader_touch_last_used(kindle.id, 20).unwrap(), "a foreign touch writes nothing");
            assert!(s.ereader_list(a).is_err());
        });
        // System sees every row
        assert_eq!(s.book_get(dune.id).unwrap().title, "Dune");
        assert_eq!(s.ereader_get(kindle.id).unwrap().label, "kindle");
        // LOCAL has no signed-in user to key a shelf to
        s.with_scope_for_test(Scope::Local, |s| {
            assert!(matches!(s.book_insert(Uuid::now_v7(), &new_book("x", "y"), 1), Err(StoreError::InvalidOp(_))));
        });
        // a share-link reader is refused everywhere
        s.with_scope_for_test(Scope::Public, |s| {
            assert!(s.book_get(dune.id).is_err());
            assert!(s.book_list(a, &BookPage::default(), 10, None).is_err());
            assert!(s.book_insert(Uuid::now_v7(), &new_book("x", "y"), 1).is_err());
            assert!(s.book_update_metadata(dune.id, "x", "y", None, None, 1).is_err());
            assert!(s.book_replace_file(dune.id, "x", 1, "x.epub", 1).is_err());
            assert!(s.book_soft_delete(dune.id, 1).is_err());
            assert!(s.book_set_cover(dune.id, "k", 1).is_err());
            assert!(s.book_find_by_sha256(a, "sha256:Dune").is_err());
            assert!(s.ereader_get(kindle.id).is_err());
            assert!(s.ereader_list(a).is_err());
            assert!(s.ereader_insert(Uuid::now_v7(), "l", "s", "k", true, 1).is_err());
            assert!(s.ereader_touch_last_used(kindle.id, 1).is_err());
            assert!(s.ereader_rotate(kindle.id, "x", "y").is_err());
            assert!(s.ereader_revoke(kindle.id, 1).is_err());
            assert!(s.ereader_find_by_secret_hash("sh1").is_err());
            assert!(s.ereader_find_kosync("Tom", "kh1").is_err());
            assert!(s.kosync_upsert(a, "d", "p", 0.5, "d", "d1", 1).is_err());
            assert!(s.kosync_get(a, "d").is_err());
        });
    }

    #[test]
    fn ereader_lifecycle_touch_rotate_revoke() {
        let (mut s, a, _b) = store_with_users();
        let (id, kobo) = (Uuid::now_v7(), Uuid::now_v7());
        s.with_scope_for_test(Scope::User(a), |s| {
            let e = s.ereader_insert(id, "kindle", "sh1", "kh1", true, 100).unwrap();
            assert_eq!(
                (e.user_id, e.label.as_str(), e.stream_downloads, e.created_at, e.last_used_at, e.revoked_at),
                (a, "kindle", true, 100, None, None)
            );
            let e2 = s.ereader_insert(kobo, "kobo", "sh2", "kh2", false, 110).unwrap();
            assert!(!e2.stream_downloads);
            assert_eq!(s.ereader_list(a).unwrap().len(), 2);

            // last_used_at rolls forward at most once a minute
            assert!(s.ereader_touch_last_used(id, 200).unwrap(), "the first touch writes");
            assert!(!s.ereader_touch_last_used(id, 259).unwrap(), "inside the minute");
            assert!(s.ereader_touch_last_used(id, 260).unwrap());
            assert_eq!(s.ereader_get(id).unwrap().last_used_at, Some(260));

            // basic-auth resolution is by hash alone, live rows only
            assert_eq!(s.ereader_find_by_secret_hash("sh1").unwrap().unwrap().id, id);
            assert!(s.ereader_find_by_secret_hash("nope").unwrap().is_none());

            // rotation invalidates the old hashes at once
            let e = s.ereader_rotate(id, "sh1b", "kh1b").unwrap();
            assert_eq!((e.secret_hash.as_str(), e.kosync_key_hash.as_str()), ("sh1b", "kh1b"));
            assert!(s.ereader_find_by_secret_hash("sh1").unwrap().is_none());
            assert_eq!(s.ereader_find_by_secret_hash("sh1b").unwrap().unwrap().id, id);

            // revoke: no longer resolves, cannot rotate or be touched; the
            // row stays listed, and revoking twice is fine
            s.ereader_revoke(id, 300).unwrap();
            s.ereader_revoke(id, 301).unwrap();
            assert_eq!(s.ereader_get(id).unwrap().revoked_at, Some(300));
            assert!(s.ereader_find_by_secret_hash("sh1b").unwrap().is_none());
            assert!(matches!(s.ereader_rotate(id, "x", "y"), Err(StoreError::InvalidOp(_))));
            assert!(!s.ereader_touch_last_used(id, 400).unwrap());
            assert_eq!(s.ereader_list(a).unwrap().len(), 2, "revoked rows stay listed");
        });
    }

    #[test]
    fn ereader_find_kosync_resolves_name_and_key() {
        let (mut s, a, _b) = store_with_users();
        let id = Uuid::now_v7();
        s.with_scope_for_test(Scope::User(a), |s| {
            s.ereader_insert(id, "kindle", "sh1", "kh1", true, 100).unwrap();
            let e = s.ereader_find_kosync("tom", "kh1").unwrap().unwrap();
            assert_eq!((e.id, e.user_id), (id, a), "the name matches case-insensitively");
            assert!(s.ereader_find_kosync("tom", "wrong").unwrap().is_none());
            assert!(s.ereader_find_kosync("nobody", "kh1").unwrap().is_none());
            s.ereader_revoke(id, 200).unwrap();
            assert!(s.ereader_find_kosync("tom", "kh1").unwrap().is_none(), "revoked no longer resolves");
        });
    }

    #[test]
    fn kosync_newest_wins() {
        let (mut s, a, b) = store_with_users();
        let doc = "0123456789abcdef0123456789abcdef";
        s.with_scope_for_test(Scope::User(a), |s| {
            let ts = s.kosync_upsert(a, doc, "/body/DocFragment[11]", 0.42, "kindle", "k1", 100).unwrap();
            assert_eq!(ts, 100);
            let p = s.kosync_get(a, doc).unwrap().unwrap();
            assert_eq!((p.progress.as_str(), p.percentage, p.device.as_str(), p.updated_at), ("/body/DocFragment[11]", 0.42, "kindle", 100));

            // an older push does not move the row but reports the stored stamp
            let ts = s.kosync_upsert(a, doc, "/body/DocFragment[2]", 0.05, "kobo", "k2", 90).unwrap();
            assert_eq!(ts, 100);
            let p = s.kosync_get(a, doc).unwrap().unwrap();
            assert_eq!(
                (p.progress.as_str(), p.percentage, p.device.as_str(), p.device_id.as_str(), p.updated_at),
                ("/body/DocFragment[11]", 0.42, "kindle", "k1", 100)
            );

            // a newer one wins, device and all
            let ts = s.kosync_upsert(a, doc, "/body/DocFragment[20]", 0.80, "kobo", "k2", 150).unwrap();
            assert_eq!(ts, 150);
            let p = s.kosync_get(a, doc).unwrap().unwrap();
            assert_eq!((p.progress.as_str(), p.percentage, p.device.as_str(), p.device_id.as_str()), ("/body/DocFragment[20]", 0.80, "kobo", "k2"));

            assert!(s.kosync_get(a, "ffffffffffffffffffffffffffffffff").unwrap().is_none());
            // another person's progress is NotFound
            assert!(s.kosync_upsert(b, doc, "x", 0.1, "d", "d1", 200).is_err());
            assert!(s.kosync_get(b, doc).is_err());
        });
        // the same document hash is another person's own row
        s.with_scope_for_test(Scope::User(b), |s| {
            assert!(s.kosync_get(b, doc).unwrap().is_none());
            assert_eq!(s.kosync_upsert(b, doc, "p", 0.1, "d", "d1", 50).unwrap(), 50);
        });
        // and System may read anyone's (maintenance)
        assert_eq!(s.kosync_get(a, doc).unwrap().unwrap().updated_at, 150);
    }
}
