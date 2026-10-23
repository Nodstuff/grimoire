//! The EPUB library's daemon half: `/api/books` CRUD, the multipart ingest
//! pipeline with S3 write-through, and the download entry points the OPDS
//! router mounts.
//!
//! - `GET /api/books?search=&before=<cursor>&limit=` → `{books, next_before}`
//! - `POST /api/books` (multipart, `files` fields) → `{results}`, one per
//!   file, multipart order preserved: `added | updated | already_here |
//!   clash | error`
//! - `PATCH /api/books/{id}` `{title?, author?, series?, series_index?}` → Book
//! - `DELETE /api/books/{id}` → 204 (the S3 object stays; the bucket's
//!   30-day versioned retention is the undo)
//! - `book_file` / `book_cover` are NOT wired here: opds.rs resolves the
//!   ereader from its basic auth and calls them (see their docs).
//!
//! Everything 404s when `LibraryState.s3` is None — LOCAL mode (no signed-
//! in user to shelve to) or no bucket configured — mirroring how
//! `shares::ServerSide` turns the share routes off.
//!
//! Memory discipline (the box has 1 GB): uploads stream field-by-field to
//! spool files and are processed one at a time; EPUB parsing reads only the
//! small metadata entries whole (bounded); S3 PUTs stream the spool;
//! downloads either 302 to S3 or proxy it chunk-by-chunk. Nowhere is a
//! whole book held in memory.

use crate::s3sign::{self, S3Config};
use crate::store_ext::{blocking, with_store};
use crate::viewer::Viewer;
use axum::extract::multipart::Field;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Multipart, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Digest as _;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use taisce_store::books::{Book, BookPage, Ereader, NewBook};
use taisce_store::{Scope, SharedStore, SqliteStore, StoreError};
use uuid::Uuid;

/// Per-file cap on an uploaded EPUB, enforced while spooling: a bigger
/// field stops writing here and answers `error` for that file.
const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// Whole-request cap (every file plus multipart framing): bounds the spool
/// disk one request can hold at once.
const MAX_BODY_BYTES: usize = 1024 * 1024 * 1024;
/// Files per request: each costs a store lock and two S3 PUTs, so a
/// 10 000-field body is a denial of service, not an upload.
const MAX_FILES: usize = 100;
/// Caps on the zip entries read whole into memory (container, OPF, cover).
/// Reads are additionally `take`-bounded, so a lying central directory
/// cannot inflate them.
const MAX_CONTAINER_BYTES: u64 = 64 * 1024;
const MAX_OPF_BYTES: u64 = 2 * 1024 * 1024;
const MAX_COVER_BYTES: u64 = 16 * 1024 * 1024;
/// Entries compressing beyond this are zip bombs until proven otherwise
/// (real EPUB content stays under ~20:1).
const MAX_ZIP_RATIO: u64 = 100;

const EPUB_CONTENT_TYPE: &str = "application/epub+zip";

#[derive(Clone)]
pub struct LibraryState {
    pub store: SharedStore,
    /// None = LOCAL mode or no bucket configured: every route here 404s.
    pub s3: Option<Arc<S3Config>>,
    pub server_mode: bool,
    /// LOCAL mode's human (for the Viewer of a LOCAL request).
    pub local_human: Uuid,
    /// Upload spools (beside the db): created on demand, each file removed
    /// as soon as its ingest finishes, stale ones swept hourly.
    pub spool_dir: PathBuf,
}

impl FromRequestParts<LibraryState> for Viewer {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &LibraryState) -> Result<Self, Response> {
        Self::from_parts(parts, st.server_mode, st.local_human)
    }
}

fn err(status: StatusCode, msg: impl std::fmt::Display) -> Response {
    (status, Json(json!({"error": msg.to_string()}))).into_response()
}

fn not_found() -> Response {
    err(StatusCode::NOT_FOUND, "not found")
}

fn fail(e: StoreError) -> Response {
    let code = match &e {
        StoreError::NotFound(_) => StatusCode::NOT_FOUND,
        StoreError::Forbidden(_) => StatusCode::FORBIDDEN,
        StoreError::InvalidOp(_) | StoreError::StaleBase { .. } => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(code, e)
}

fn book_json(b: &Book) -> Value {
    json!({
        "id": b.id,
        "title": b.title.as_str(),
        "author": b.author.as_str(),
        "series": b.series.as_deref(),
        "series_index": b.series_index,
        "book_identifier": b.book_identifier.as_deref(),
        "original_filename": b.original_filename.as_str(),
        "size": b.size,
        "has_cover": b.cover_s3_key.is_some(),
        "added_at": b.added_at,
        "updated_at": b.updated_at,
    })
}

fn file_result(
    filename: &str,
    status: &str,
    book: Option<&Book>,
    message: Option<String>,
) -> Value {
    let mut v = json!({"filename": filename, "status": status});
    if let Some(b) = book {
        v["book"] = book_json(b);
    }
    if let Some(m) = message {
        v["message"] = json!(m);
    }
    v
}

// ---- list ----

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// The keyset cursor: `<added_at>:<id>` of the last book of a page.
fn cursor(added_at: i64, id: Uuid) -> String {
    format!("{added_at}:{id}")
}

fn parse_cursor(v: Option<&str>) -> Result<Option<(i64, Uuid)>, Response> {
    let Some(v) = v.filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    let Some((at, id)) = v.split_once(':') else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "before: a cursor from a previous page (added_at:id)",
        ));
    };
    let at = at
        .parse::<i64>()
        .map_err(|_| err(StatusCode::BAD_REQUEST, "before: bad cursor"))?;
    let id = Uuid::parse_str(id).map_err(|_| err(StatusCode::BAD_REQUEST, "before: bad cursor"))?;
    Ok(Some((at, id)))
}

async fn list_books(
    State(st): State<LibraryState>,
    v: Viewer,
    Query(q): Query<ListQuery>,
) -> Response {
    if st.s3.is_none() {
        return not_found();
    }
    let Some(user) = v.user else {
        return not_found();
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let before = match parse_cursor(q.before.as_deref()) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let search = q.search.clone();
    // one extra row says whether another page exists
    let found = with_store(&st.store, v.scope, move |s| {
        s.book_list(
            user,
            &BookPage {
                before,
                after: None,
            },
            limit + 1,
            search.as_deref(),
        )
    })
    .await;
    match found {
        Ok(mut books) => {
            let next_before = if books.len() > limit {
                books.truncate(limit);
                books.last().map(|b| cursor(b.added_at, b.id))
            } else {
                None
            };
            Json(json!({
                "books": books.iter().map(book_json).collect::<Vec<_>>(),
                "next_before": next_before,
            }))
            .into_response()
        }
        Err(e) => fail(e),
    }
}

// ---- metadata patch + delete ----

/// Absent → None (untouched), `null` → Some(None) (cleared), a value →
/// Some(Some(v)): PATCH over the store's full-field update.
fn nullable<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<T>::deserialize(d)?))
}

#[derive(Deserialize)]
struct PatchReq {
    title: Option<String>,
    author: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    series: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    series_index: Option<Option<f64>>,
}

fn json_or_400<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    match body {
        Ok(Json(b)) => Ok(b),
        Err(JsonRejection::BytesRejection(e)) => Err(err(e.status(), e.body_text())),
        Err(e) => Err(err(StatusCode::BAD_REQUEST, e.body_text())),
    }
}

/// A set title/author: trimmed, present, bounded.
fn clean_label(field: &str, v: &str) -> Result<String, Response> {
    let v = v.trim();
    if v.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, format!("{field}: empty")));
    }
    if v.chars().count() > 1024 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("{field}: at most 1024 characters"),
        ));
    }
    Ok(v.to_string())
}

async fn patch_book(
    State(st): State<LibraryState>,
    v: Viewer,
    Path(id): Path<Uuid>,
    body: Result<Json<PatchReq>, JsonRejection>,
) -> Response {
    if st.s3.is_none() {
        return not_found();
    }
    let req = match json_or_400(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let title = match req
        .title
        .as_deref()
        .map(|t| clean_label("title", t))
        .transpose()
    {
        Ok(t) => t,
        Err(r) => return r,
    };
    let author = match req
        .author
        .as_deref()
        .map(|a| clean_label("author", a))
        .transpose()
    {
        Ok(a) => a,
        Err(r) => return r,
    };
    let series = match req.series {
        None => None,
        // an explicit null clears, and so does an empty string
        Some(v) => Some(v.and_then(|s| {
            let s = s.trim().to_string();
            (!s.is_empty() && s.chars().count() <= 1024).then_some(s)
        })),
    };
    let series_index = match req.series_index {
        None => None,
        Some(v) => Some(v.filter(|n| n.is_finite() && *n >= 0.0 && *n <= 1_000_000.0)),
    };
    let now = crate::auth::now();
    let done = with_store(&st.store, v.scope, move |s| {
        let cur = s.book_get(id)?;
        let title = title.unwrap_or(cur.title);
        let author = author.unwrap_or(cur.author);
        let series = series.unwrap_or(cur.series);
        let series_index = series_index.unwrap_or(cur.series_index);
        s.book_update_metadata(id, &title, &author, series.as_deref(), series_index, now)
    })
    .await;
    match done {
        Ok(b) => Json(book_json(&b)).into_response(),
        Err(e) => fail(e),
    }
}

async fn delete_book(State(st): State<LibraryState>, v: Viewer, Path(id): Path<Uuid>) -> Response {
    if st.s3.is_none() {
        return not_found();
    }
    match with_store(&st.store, v.scope, move |s| {
        s.book_soft_delete(id, crate::auth::now())
    })
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => fail(e),
    }
}

// ---- the upload ----

/// A spooled upload: the file is removed on drop, so every path out of an
/// ingest — success, parse error, S3 failure, oversized — leaves nothing
/// on the box's small disk.
struct Spool {
    path: PathBuf,
}

impl Spool {
    async fn create(dir: &FsPath) -> std::io::Result<(Spool, tokio::fs::File)> {
        let path = dir.join(format!("taisce-book-{}.epub", Uuid::now_v7()));
        let file = tokio::fs::File::create(&path).await?;
        Ok((Spool { path }, file))
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

enum SpoolError {
    TooBig,
    Io(String),
}

/// Stream one multipart field into a spool file, stopping at the per-file
/// cap. Never a `Vec<u8>` of the whole thing: uploads can be 100 MB and
/// the box has 1 GB of RAM for everything.
async fn spool_field(dir: &FsPath, field: &mut Field<'_>) -> Result<Spool, SpoolError> {
    use tokio::io::AsyncWriteExt as _;
    let (spool, mut file) = Spool::create(dir)
        .await
        .map_err(|e| SpoolError::Io(format!("creating the spool: {e}")))?;
    let mut written: u64 = 0;
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                written += chunk.len() as u64;
                if written > MAX_FILE_BYTES {
                    // the guard drops, and the partial spool with it
                    return Err(SpoolError::TooBig);
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|e| SpoolError::Io(format!("writing the spool: {e}")))?;
            }
            Ok(None) => break,
            Err(e) => {
                return Err(SpoolError::Io(format!(
                    "reading the upload: {}",
                    e.body_text()
                )));
            }
        }
    }
    file.flush()
        .await
        .map_err(|e| SpoolError::Io(format!("writing the spool: {e}")))?;
    drop(file);
    Ok(spool)
}

/// Create the spool dir on demand and sweep stale spools (a crash
/// mid-upload leaves them; they hold disk the box doesn't have). The sweep
/// is best-effort: it never blocks an upload.
async fn prepare_spool_dir(dir: &FsPath) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    if let Ok(rd) = std::fs::read_dir(dir) {
        let cutoff = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(3600));
        for e in rd.flatten() {
            let stale = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .zip(cutoff)
                .is_some_and(|(t, c)| t < c);
            if stale {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// A multipart filename is metadata, not a path: browsers send a basename,
/// a script could send anything.
fn clean_filename(name: &str) -> String {
    let base = name
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(name);
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "book.epub".into()
    } else {
        cleaned.into()
    }
}

async fn upload_books(
    State(st): State<LibraryState>,
    v: Viewer,
    mp: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Response {
    let Some(s3) = st.s3.clone() else {
        return not_found();
    };
    // LOCAL mode has no signed-in user to shelve a book to
    let Some(user) = v.user else {
        return not_found();
    };
    let mut mp = match mp {
        Ok(m) => m,
        Err(e) => return err(e.status(), e.body_text()),
    };
    if let Err(e) = prepare_spool_dir(&st.spool_dir).await {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the spool directory: {e}"),
        );
    }
    let mut results: Vec<Value> = Vec::new();
    let mut files = 0usize;
    // fields are processed one at a time, in multipart order: each spool
    // is gone before the next lands, and one S3 write happens at a time
    loop {
        let mut field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                results.push(file_result(
                    "(stream)",
                    "error",
                    None,
                    Some(format!(
                        "the upload stream broke partway: {}; the files before it were processed",
                        e.body_text()
                    )),
                ));
                break;
            }
        };
        // only "files" fields carry books; the rest are drained and ignored
        if field.name() != Some("files") {
            continue;
        }
        files += 1;
        let filename = field
            .file_name()
            .map(clean_filename)
            .unwrap_or_else(|| "book.epub".into());
        if files > MAX_FILES {
            results.push(file_result(
                &filename,
                "error",
                None,
                Some(format!(
                    "too many files in one upload (at most {MAX_FILES})"
                )),
            ));
            break;
        }
        match spool_field(&st.spool_dir, &mut field).await {
            Err(SpoolError::TooBig) => results.push(file_result(
                &filename,
                "error",
                None,
                Some(format!("over {} MB", MAX_FILE_BYTES / 1024 / 1024)),
            )),
            Err(SpoolError::Io(m)) => results.push(file_result(&filename, "error", None, Some(m))),
            Ok(spool) => results.push(ingest_one(&st, &s3, user, &filename, &spool).await),
        }
    }
    Json(json!({"results": results})).into_response()
}

// ---- the ingest pipeline ----

/// What `inspect_spool` learned about one spooled EPUB.
#[derive(Debug)]
struct Inspected {
    sha256: String,
    size: u64,
    title: String,
    author: String,
    series: Option<String>,
    series_index: Option<f64>,
    identifier: Option<String>,
    /// Cover bytes + extension, already read out of the zip (bounded).
    cover: Option<(Vec<u8>, &'static str)>,
}

/// What the identity rules say about an incoming file.
enum Decision {
    /// (a) the same bytes are already shelved: no S3 write at all.
    AlreadyHere(Book),
    /// (b)/(c) same identifier, or same title+author: new bytes for a book.
    Updated(Book),
    /// Another book already derives the filename this one would.
    Clash(String),
    /// (d) a new book.
    Add,
}

async fn ingest_one(
    st: &LibraryState,
    s3: &Arc<S3Config>,
    user: Uuid,
    filename: &str,
    spool: &Spool,
) -> Value {
    // hashing and EPUB parsing are disk + CPU: off the async workers, and
    // never under the store lock
    let path = spool.path.clone();
    let inspected = match blocking(move || inspect_spool(&path)).await {
        Ok(i) => i,
        Err(reason) => return file_result(filename, "error", None, Some(reason)),
    };
    let decision = {
        let (sha, ident, title, author) = (
            inspected.sha256.clone(),
            inspected.identifier.clone(),
            inspected.title.clone(),
            inspected.author.clone(),
        );
        with_store(&st.store, Scope::User(user), move |s| {
            decide(s, user, &sha, ident.as_deref(), &title, &author)
        })
        .await
    };
    let decision = match decision {
        Ok(d) => d,
        Err(e) => return file_result(filename, "error", None, Some(e.to_string())),
    };
    match decision {
        Decision::AlreadyHere(book) => file_result(filename, "already_here", Some(&book), None),
        Decision::Clash(msg) => file_result(filename, "clash", None, Some(msg)),
        Decision::Updated(existing) => {
            update_book(st, s3, user, filename, spool, inspected, existing).await
        }
        Decision::Add => add_book(st, s3, user, filename, spool, inspected).await,
    }
}

/// The identity rules, IN ORDER, then the derived-name clash check for a
/// genuinely new book. Runs in the caller's scope inside the store lock.
fn decide(
    s: &mut SqliteStore,
    user: Uuid,
    sha: &str,
    identifier: Option<&str>,
    title: &str,
    author: &str,
) -> taisce_store::Result<Decision> {
    // (a) the exact same file is here already
    if let Some(b) = s.book_find_by_sha256(user, sha)? {
        return Ok(Decision::AlreadyHere(b));
    }
    // (b) the same book identifier (when the file carries one)
    if let Some(id) = identifier
        && let Some(b) = s.book_find_by_identifier(user, id)?
    {
        return Ok(Decision::Updated(b));
    }
    // (c) the same title + author (the store matches NOCASE)
    if let Some(b) = s.book_find_by_title_author(user, title, author)? {
        return Ok(Decision::Updated(b));
    }
    // (d) a new book — unless its derived name is already taken. The
    // derived name is what every OPDS client saves the download as (and
    // kosync keys on it): two books may never share one, and we never
    // auto-rename, because the rename would desync both devices.
    // Compared case-insensitively: e-readers save to FAT, where "Dune.epub"
    // and "dune.epub" are one file.
    let name = crate::booknaming::derived_book_filename(title, author);
    let key = name.to_lowercase();
    let mut page = BookPage::default();
    loop {
        let books = s.book_list(user, &page, 500, None)?;
        for b in &books {
            if crate::booknaming::derived_book_filename(&b.title, &b.author).to_lowercase() == key {
                return Ok(Decision::Clash(format!(
                    "would be saved as \"{name}\", the same name as \"{}\" by {} already on the shelf — rename one of them first",
                    b.title, b.author
                )));
            }
        }
        // a short page is the last page
        let Some(last) = books.last().filter(|_| books.len() == 500) else {
            break;
        };
        page = BookPage {
            before: Some((last.added_at, last.id)),
            after: None,
        };
    }
    Ok(Decision::Add)
}

enum Added {
    New(Book),
    Existing(Book),
}

/// Rule (d): a new book. Write-through: the request is not answered until
/// the EPUB and its cover are in the bucket, so a shelved book is never a
/// broken download.
async fn add_book(
    st: &LibraryState,
    s3: &Arc<S3Config>,
    user: Uuid,
    filename: &str,
    spool: &Spool,
    inspected: Inspected,
) -> Value {
    let id = Uuid::now_v7();
    let file_key = format!("library/{user}/{id}.epub");
    if let Err(e) = s3.put_file(&file_key, &spool.path, EPUB_CONTENT_TYPE).await {
        return file_result(
            filename,
            "error",
            None,
            Some(format!("storing the file: {e:#}")),
        );
    }
    let cover_key = match upload_cover(s3, user, id, &inspected).await {
        Ok(k) => k,
        Err(e) => {
            return file_result(
                filename,
                "error",
                None,
                Some(format!("storing the cover: {e:#}")),
            );
        }
    };
    let now = crate::auth::now();
    let sha = inspected.sha256.clone();
    let new_book = NewBook {
        title: inspected.title,
        author: inspected.author,
        series: inspected.series,
        series_index: inspected.series_index,
        book_identifier: inspected.identifier,
        original_filename: filename.to_string(),
        file_sha256: inspected.sha256,
        size: inspected.size as i64,
        file_s3_key: file_key,
        cover_s3_key: cover_key,
    };
    let written = with_store(&st.store, Scope::User(user), move |s| -> taisce_store::Result<Added> {
        // a concurrent upload of the same file may have landed while S3 was
        // being written: converge on the winner instead of double-shelving
        // (the loser's object sits at an unreferenced key; bucket lifecycle
        // sweeps it)
        if let Some(b) = s.book_find_by_sha256(user, &sha)? {
            return Ok(Added::Existing(b));
        }
        Ok(Added::New(s.book_insert(id, &new_book, now)?))
    })
    .await;
    match written {
        Ok(Added::New(b)) => file_result(filename, "added", Some(&b), None),
        Ok(Added::Existing(b)) => file_result(filename, "already_here", Some(&b), None),
        Err(e) => file_result(filename, "error", None, Some(e.to_string())),
    }
}

/// Rules (b)/(c): the same book, new bytes. The object is replaced at the
/// SAME key (bucket versioning keeps the previous one for the 30-day
/// undo) and the row follows the new FILE only — hash, size, original
/// filename, cover (`book_replace_file`), so a later re-upload of these
/// same bytes dedups by rule (a). Title, author and series never change
/// here: they are the person's (a PATCH may have edited them), and they
/// derive the filename the devices saved and the kosync key, which a
/// re-upload must not move.
async fn update_book(
    st: &LibraryState,
    s3: &Arc<S3Config>,
    user: Uuid,
    filename: &str,
    spool: &Spool,
    inspected: Inspected,
    existing: Book,
) -> Value {
    if let Err(e) = s3
        .put_file(&existing.file_s3_key, &spool.path, EPUB_CONTENT_TYPE)
        .await
    {
        return file_result(
            filename,
            "error",
            None,
            Some(format!("storing the file: {e:#}")),
        );
    }
    let cover_key = match upload_cover(s3, user, existing.id, &inspected).await {
        Ok(k) => k,
        Err(e) => {
            return file_result(
                filename,
                "error",
                None,
                Some(format!("storing the cover: {e:#}")),
            );
        }
    };
    let now = crate::auth::now();
    let sha = inspected.sha256.clone();
    let size = inspected.size as i64;
    let original_filename = filename.to_string();
    let written = with_store(&st.store, Scope::User(user), move |s| -> taisce_store::Result<Book> {
        let b = s.book_replace_file(existing.id, &sha, size, &original_filename, now)?;
        // a re-upload with no embedded cover keeps the old one; a cover
        // under a new extension orphans the old object (lifecycle sweeps)
        let b = match &cover_key {
            Some(k) => s.book_set_cover(b.id, k, now)?,
            None => b,
        };
        Ok(b)
    })
    .await;
    match written {
        Ok(b) => file_result(filename, "updated", Some(&b), None),
        Err(e) => file_result(filename, "error", None, Some(e.to_string())),
    }
}

/// The cover bytes of an inspected EPUB, at the book's key. `None` when
/// the file carries none we can use — never a failed ingest: plenty of
/// EPUBs have no usable cover.
async fn upload_cover(
    s3: &Arc<S3Config>,
    user: Uuid,
    id: Uuid,
    inspected: &Inspected,
) -> anyhow::Result<Option<String>> {
    let Some((bytes, ext)) = &inspected.cover else {
        return Ok(None);
    };
    let key = format!("library/{user}/{id}.cover.{ext}");
    s3.put_bytes(&key, bytes, cover_content_type(ext)).await?;
    Ok(Some(key))
}

fn cover_content_type(ext: &str) -> &'static str {
    match ext {
        "jpg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        // extract_cover only ever produces the five above
        _ => "application/octet-stream",
    }
}

// ---- reading the EPUB itself (sync, on the blocking pool) ----

/// One zip entry, capped: the declared-size guard refuses a monster up
/// front, the ratio guard refuses an absurd one, and the `take` bound
/// means a central directory that lies about sizes still can't inflate
/// the read past `cap`.
fn read_entry<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    cap: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let entry = zip.by_name(name).map_err(|e| match e {
        zip::result::ZipError::FileNotFound => format!("no {name:?} in the archive"),
        e => format!("opening {name:?}: {e}"),
    })?;
    let size = entry.size();
    if size > cap {
        return Err(format!("{name:?} is {size} bytes, over the {cap} cap"));
    }
    let comp = entry.compressed_size();
    if comp > 0 && size > 4096 && size / comp > MAX_ZIP_RATIO {
        return Err(format!(
            "{name:?} compresses {}:1, beyond plausible",
            size / comp
        ));
    }
    let mut buf = Vec::with_capacity(size.min(cap) as usize);
    let n = entry
        .take(cap + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("reading {name:?}: {e}"))?;
    if n as u64 > cap {
        return Err(format!("{name:?} is over the {cap} cap"));
    }
    Ok(buf)
}

/// Hash the spool, then read its metadata out of the zip without ever
/// unpacking to disk: container.xml → the OPF → dc fields, series and the
/// cover (EPUB2 and EPUB3). Any unreadable step is an `error` for that
/// file, not the request.
fn inspect_spool(path: &FsPath) -> Result<Inspected, String> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path).map_err(|e| format!("reading the spool: {e}"))?;
    let size = f
        .metadata()
        .map_err(|e| format!("reading the spool: {e}"))?
        .len();
    let mut sha = sha2::Sha256::new();
    {
        // the whole file, 64 KiB at a time — never whole in memory
        let mut buf = [0u8; 65536];
        loop {
            let n = f
                .read(&mut buf)
                .map_err(|e| format!("hashing the file: {e}"))?;
            if n == 0 {
                break;
            }
            sha.update(&buf[..n]);
        }
    }
    let sha256 = hex::encode(sha.finalize());
    drop(f);
    let f = std::fs::File::open(path).map_err(|e| format!("reading the spool: {e}"))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| format!("not an EPUB ({e})"))?;
    let container = read_entry(&mut zip, "META-INF/container.xml", MAX_CONTAINER_BYTES)
        .map_err(|e| format!("bad container: {e}"))?;
    let opf_path = container_rootfile(&container)?;
    let opf_bytes = read_entry(&mut zip, &opf_path, MAX_OPF_BYTES)
        .map_err(|e| format!("unreadable OPF {opf_path:?}: {e}"))?;
    let opf = String::from_utf8(opf_bytes).map_err(|_| "the OPF is not UTF-8".to_string())?;
    let meta = parse_opf(&opf)?;
    // a cover that can't be read or resolved is no cover, not a failed book
    let cover = meta
        .cover
        .as_ref()
        .and_then(|c| extract_cover(&mut zip, &opf_path, c));
    let title = meta
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "the OPF has no dc:title".to_string())?;
    Ok(Inspected {
        sha256,
        size,
        title,
        author: meta
            .creator
            .map(|c| c.trim().to_string())
            .unwrap_or_default(),
        series: meta.series,
        series_index: meta.series_index,
        identifier: meta.identifier,
        cover,
    })
}

fn attr(
    e: &quick_xml::events::BytesStart<'_>,
    decoder: quick_xml::Decoder,
    name: &str,
) -> Option<String> {
    e.try_get_attribute(name)
        .ok()
        .flatten()
        .and_then(|a| {
            // the deprecated helper desugars to exactly this call
            a.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder).ok()
        })
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The text of the element whose Start we just read (read_text consumes
/// through its End), entities unescaped, trimmed.
fn element_text(
    r: &mut quick_xml::Reader<&[u8]>,
    end: quick_xml::name::QName<'_>,
) -> Result<String, String> {
    let t = r.read_text(end).map_err(|e| format!("OPF: {e}"))?;
    let decoded = t.decode().map_err(|e| format!("OPF text: {e}"))?;
    let unescaped = quick_xml::escape::unescape(&decoded).map_err(|e| format!("OPF text: {e}"))?;
    Ok(unescaped.trim().to_string())
}

/// container.xml → the OPF's path inside the zip.
fn container_rootfile(xml: &[u8]) -> Result<String, String> {
    let xml = std::str::from_utf8(xml).map_err(|_| "container.xml is not UTF-8".to_string())?;
    let mut r = quick_xml::Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                if e.local_name().as_ref() == b"rootfile"
                    && let Some(v) = attr(&e, r.decoder(), "full-path")
                {
                    return Ok(v);
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => return Err(format!("container.xml: {e}")),
            _ => {}
        }
    }
    Err("container.xml names no rootfile".into())
}

/// One manifest `<item>` we might care about.
struct ManifestItem {
    id: String,
    href: String,
    media_type: String,
    cover_image: bool,
}

/// A cover candidate: the href (relative to the OPF) and declared type.
struct CoverRef {
    href: String,
    media_type: String,
}

/// One manifest `<item>`: id + href (+ declared type, and whether it is
/// the EPUB3 cover image).
fn read_manifest_item(
    e: &quick_xml::events::BytesStart<'_>,
    d: quick_xml::Decoder,
    items: &mut Vec<ManifestItem>,
) {
    let (Some(id), Some(href)) = (attr(e, d, "id"), attr(e, d, "href")) else {
        return;
    };
    let media_type = attr(e, d, "media-type").unwrap_or_default();
    let cover_image =
        attr(e, d, "properties").is_some_and(|p| p.split(' ').any(|t| t == "cover-image"));
    items.push(ManifestItem {
        id,
        href,
        media_type,
        cover_image,
    });
}

#[derive(Default)]
struct OpfMeta {
    title: Option<String>,
    creator: Option<String>,
    identifier: Option<String>,
    series: Option<String>,
    series_index: Option<f64>,
    cover: Option<CoverRef>,
}

/// The OPF: dc:title / dc:creator / dc:identifier, the series (EPUB3
/// belongs-to-collection + group-position, or calibre's meta), and the
/// cover (EPUB3 `properties="cover-image"`, else EPUB2 `<meta
/// name="cover">` — both are tried). Namespaces are matched by local
/// name: prefixes are the publisher's choice.
fn parse_opf(xml: &str) -> Result<OpfMeta, String> {
    use quick_xml::events::Event;
    let mut r = quick_xml::Reader::from_str(xml);
    let mut unique_id: Option<String> = None;
    let mut identifiers: Vec<(Option<String>, String)> = Vec::new();
    let mut items: Vec<ManifestItem> = Vec::new();
    let mut cover_id: Option<String> = None;
    let mut calibre_series: Option<String> = None;
    let mut calibre_index: Option<f64> = None;
    // EPUB3: (the collection's name, its meta id) and (position, refines)
    let mut collection: Option<(String, Option<String>)> = None;
    let mut positions: Vec<(String, Option<String>)> = Vec::new();
    let mut title: Option<String> = None;
    let mut creator: Option<String> = None;
    loop {
        match r.read_event() {
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("OPF: {e}")),
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"package" => unique_id = attr(&e, r.decoder(), "unique-identifier"),
                b"title" if title.is_none() => title = Some(element_text(&mut r, e.name())?),
                b"creator" if creator.is_none() => creator = Some(element_text(&mut r, e.name())?),
                b"identifier" => {
                    let id = attr(&e, r.decoder(), "id");
                    let text = element_text(&mut r, e.name())?;
                    identifiers.push((id, text));
                }
                b"meta" => {
                    // meta carries its value in content= (EPUB2/calibre) or
                    // as its text (EPUB3 properties)
                    let text = element_text(&mut r, e.name())?;
                    match (
                        attr(&e, r.decoder(), "name"),
                        attr(&e, r.decoder(), "property"),
                    ) {
                        (Some(n), _) if n.eq_ignore_ascii_case("cover") => {
                            cover_id = attr(&e, r.decoder(), "content")
                        }
                        (Some(n), _) if n.eq_ignore_ascii_case("calibre:series") => {
                            calibre_series = attr(&e, r.decoder(), "content")
                        }
                        (Some(n), _) if n.eq_ignore_ascii_case("calibre:series_index") => {
                            calibre_index = attr(&e, r.decoder(), "content")
                                .and_then(|v| v.parse::<f64>().ok())
                                .filter(|v| v.is_finite());
                        }
                        (_, Some(p)) if p.eq_ignore_ascii_case("belongs-to-collection") => {
                            if collection.is_none() && !text.is_empty() {
                                collection = Some((text, attr(&e, r.decoder(), "id")));
                            }
                        }
                        (_, Some(p)) if p.eq_ignore_ascii_case("group-position") => {
                            if !text.is_empty() {
                                positions.push((text, attr(&e, r.decoder(), "refines")));
                            }
                        }
                        _ => {}
                    }
                }
                b"item" => read_manifest_item(&e, r.decoder(), &mut items),
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"package" => unique_id = attr(&e, r.decoder(), "unique-identifier"),
                b"meta" => match (
                    attr(&e, r.decoder(), "name"),
                    attr(&e, r.decoder(), "property"),
                ) {
                    (Some(n), _) if n.eq_ignore_ascii_case("cover") => {
                        cover_id = attr(&e, r.decoder(), "content")
                    }
                    (Some(n), _) if n.eq_ignore_ascii_case("calibre:series") => {
                        calibre_series = attr(&e, r.decoder(), "content")
                    }
                    (Some(n), _) if n.eq_ignore_ascii_case("calibre:series_index") => {
                        calibre_index = attr(&e, r.decoder(), "content")
                            .and_then(|v| v.parse::<f64>().ok())
                            .filter(|v| v.is_finite());
                    }
                    _ => {}
                },
                b"item" => read_manifest_item(&e, r.decoder(), &mut items),
                _ => {}
            },
            _ => {}
        }
    }
    // the package's unique-identifier names THE identifier when it points
    // at one; otherwise the first dc:identifier stands for the book
    let identifier = unique_id
        .as_deref()
        .and_then(|u| identifiers.iter().find(|(id, _)| id.as_deref() == Some(u)))
        .or_else(|| identifiers.first())
        .map(|(_, t)| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let (series, series_index) = match (&collection, &calibre_series) {
        (Some((name, coll_id)), _) => {
            // the group-position refining THIS collection, else any one
            let wanted = coll_id.as_deref().map(|id| format!("#{id}"));
            let pos = positions
                .iter()
                .find(|(_, refines)| refines.is_some() && *refines == wanted)
                .or_else(|| positions.first());
            let idx = pos
                .and_then(|(t, _)| t.parse::<f64>().ok())
                .filter(|v| v.is_finite());
            (Some(name.clone()), idx)
        }
        (None, Some(name)) => (Some(name.clone()), calibre_index),
        (None, None) => (None, None),
    };
    let cover = items
        .iter()
        .find(|i| i.cover_image)
        .or_else(|| cover_id.and_then(|cid| items.iter().find(move |i| i.id == cid)))
        .map(|i| CoverRef {
            href: i.href.clone(),
            media_type: i.media_type.clone(),
        });
    Ok(OpfMeta {
        title,
        creator,
        identifier,
        series,
        series_index,
        cover,
    })
}

/// The cover href resolved against the OPF's directory, percent-decoded.
/// Zip-slip guard: we never extract, but a resolved `..` or an absolute
/// path would read the WRONG entry and shelve it as a cover — refuse them.
fn resolve_href(opf_path: &str, href: &str) -> Option<String> {
    let href = href.split(['#', '?']).next().unwrap_or(href);
    let decoded = percent_decode(href)?;
    let mut parts: Vec<&str> = match opf_path.rsplit_once('/') {
        Some((dir, _)) => dir.split('/').filter(|c| !c.is_empty()).collect(),
        None => Vec::new(),
    };
    if decoded.starts_with('/') {
        parts.clear();
    }
    for comp in decoded.split('/') {
        match comp {
            "" | "." => {}
            ".." => return None,
            c if c.contains('\\') => return None,
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// `%XX` decoding (hrefs are URLs: "cover%20image.jpg" names the entry
/// "cover image.jpg"). Bad escapes or non-UTF-8 are no cover, not an error.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let h = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())?;
            out.push(h);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn cover_ext_from_type(media_type: &str) -> Option<&'static str> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/svg+xml" => Some("svg"),
        _ => None,
    }
}

fn cover_ext_from_name(name: &str) -> Option<&'static str> {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => Some("jpg"),
        "png" => Some("png"),
        "webp" => Some("webp"),
        "gif" => Some("gif"),
        "svg" => Some("svg"),
        _ => None,
    }
}

/// Read the cover entry out of the zip (bounded). The declared media-type
/// wins; the entry name's extension is the fallback; neither → no cover.
fn extract_cover<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    opf_path: &str,
    cover: &CoverRef,
) -> Option<(Vec<u8>, &'static str)> {
    let name = resolve_href(opf_path, &cover.href)?;
    let ext = cover_ext_from_type(&cover.media_type).or_else(|| cover_ext_from_name(&name))?;
    let bytes = read_entry(zip, &name, MAX_COVER_BYTES).ok()?;
    Some((bytes, ext))
}

// ---- downloads (mounted by opds.rs, implemented here) ----

/// INTEGRATION (opds.rs owns the routes): mount
/// `/opds/book/{id}/file` and `/opds/book/{id}/cover` as GET|HEAD; the
/// handler resolves the ereader from the request's basic auth
/// (`ereader_find_by_secret_hash`) and calls these:
///
/// ```ignore
/// library::book_file(&state, &ereader, id, req.method()).await
/// ```
///
/// `state` is a clone of the same `LibraryState` the `/api/books` router
/// uses. Both return a full axum Response (302, stream, or an error).

/// A book download. `ereader` is pre-resolved by the caller (its user_id
/// scopes the book lookup: someone else's book is 404, never 403) and its
/// row picks the transport: the default is a 302 to a 5-minute presigned
/// URL carrying `Content-Disposition: attachment; filename="<derived>"`
/// (per-method: a HEAD signs its own); `stream_downloads` proxies S3
/// through the daemon instead, chunk by chunk.
pub async fn book_file(
    st: &LibraryState,
    ereader: &Ereader,
    book_id: Uuid,
    method: &Method,
) -> Response {
    if *method != Method::GET && *method != Method::HEAD {
        return err(StatusCode::METHOD_NOT_ALLOWED, "GET or HEAD");
    }
    let Some(s3) = st.s3.clone() else {
        return not_found();
    };
    let book = match resolve_book(st, ereader, book_id).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let filename = crate::booknaming::derived_book_filename(&book.title, &book.author);
    let disposition = content_disposition(&filename);
    if ereader.stream_downloads {
        stream_object(
            &s3,
            &book.file_s3_key,
            method,
            EPUB_CONTENT_TYPE,
            Some(disposition),
        )
        .await
    } else {
        redirect_object(&s3, &book.file_s3_key, method, Some(disposition)).await
    }
}

/// `attachment` with the derived name (RFC 6266): an ASCII `filename`
/// fallback (anything else → `_`; the name never holds a quote or
/// backslash, booknaming replaced them) plus the exact UTF-8 name in
/// `filename*`. Every byte is visible ASCII, so it is always a valid header.
fn content_disposition(filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| if c.is_ascii() && !c.is_ascii_control() { c } else { '_' })
        .collect();
    let mut enc = String::with_capacity(filename.len() * 3);
    for b in filename.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => enc.push(b as char),
            b => enc.push_str(&format!("%{b:02X}")),
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{enc}")
}

/// A cover download: same transport choice, no disposition, and a plain
/// 404 for a book without one.
pub async fn book_cover(
    st: &LibraryState,
    ereader: &Ereader,
    book_id: Uuid,
    method: &Method,
) -> Response {
    if *method != Method::GET && *method != Method::HEAD {
        return err(StatusCode::METHOD_NOT_ALLOWED, "GET or HEAD");
    }
    let Some(s3) = st.s3.clone() else {
        return not_found();
    };
    let book = match resolve_book(st, ereader, book_id).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Some(key) = book.cover_s3_key else {
        return not_found();
    };
    let content_type = cover_content_type(key.rsplit('.').next().unwrap_or(""));
    if ereader.stream_downloads {
        stream_object(&s3, &key, method, content_type, None).await
    } else {
        redirect_object(&s3, &key, method, None).await
    }
}

/// The book, in the ereader's user's scope, and a use stamp for the
/// ereader (throttled by the store; a missed stamp never fails a read).
async fn resolve_book(
    st: &LibraryState,
    ereader: &Ereader,
    book_id: Uuid,
) -> Result<Book, Response> {
    let (user, ereader_id) = (ereader.user_id, ereader.id);
    let now = crate::auth::now();
    with_store(&st.store, Scope::User(user), move |s| {
        let book = s.book_get(book_id)?;
        let _ = s.ereader_touch_last_used(ereader_id, now);
        Ok(book)
    })
    .await
    .map_err(fail)
}

/// The web UI's own cover route (`coverUrl` in ui/src/books.ts): the
/// person's Viewer scopes the book (someone else's is 404), then the same
/// presigned 302 an ereader gets. No `stream_downloads` here — a browser
/// follows the redirect like any image.
async fn person_cover(
    State(st): State<LibraryState>,
    v: Viewer,
    Path(id): Path<Uuid>,
    method: Method,
) -> Response {
    let Some(s3) = st.s3.clone() else {
        return not_found();
    };
    if v.user.is_none() {
        return not_found();
    }
    let book = match with_store(&st.store, v.scope, move |s| s.book_get(id)).await {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let Some(key) = book.cover_s3_key else {
        return not_found();
    };
    redirect_object(&s3, &key, &method, None).await
}

fn s3_unavailable(e: anyhow::Error) -> Response {
    tracing::warn!("library: object storage: {e:#}");
    err(StatusCode::BAD_GATEWAY, "object storage is unavailable")
}

/// 302 to a presigned URL (a HEAD request signs a HEAD URL). The URL
/// expires in minutes, so the redirect itself must never be cached.
async fn redirect_object(
    s3: &S3Config,
    key: &str,
    method: &Method,
    disposition: Option<String>,
) -> Response {
    let m = if *method == Method::HEAD {
        s3sign::Method::Head
    } else {
        s3sign::Method::Get
    };
    match s3
        .presign_get(key, m, disposition.as_deref(), s3sign::PRESIGN_TTL)
        .await
    {
        Ok(url) => match HeaderValue::from_str(&url) {
            Ok(v) => {
                let mut r = StatusCode::FOUND.into_response();
                let h = r.headers_mut();
                h.insert(header::LOCATION, v);
                h.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, no-store"),
                );
                r
            }
            Err(_) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not build the download URL",
            ),
        },
        Err(e) => s3_unavailable(e),
    }
}

/// The `stream_downloads` transport: S3 → daemon → client, chunk by chunk
/// (never the whole object in memory, whatever its size — 1 GB of RAM
/// here), forwarding the content type, length and disposition the ereader
/// downloads against.
async fn stream_object(
    s3: &S3Config,
    key: &str,
    method: &Method,
    content_type: &str,
    disposition: Option<String>,
) -> Response {
    let m = if *method == Method::HEAD {
        s3sign::Method::Head
    } else {
        s3sign::Method::Get
    };
    let url = match s3.presign_get(key, m, None, s3sign::PRESIGN_TTL).await {
        Ok(u) => u,
        Err(e) => return s3_unavailable(e),
    };
    let req_method = if *method == Method::HEAD {
        reqwest::Method::HEAD
    } else {
        reqwest::Method::GET
    };
    let upstream = match s3.http().request(req_method, url).send().await {
        Ok(u) => u,
        Err(e) => return s3_unavailable(e.into()),
    };
    let status = upstream.status();
    if !status.is_success() {
        let body = upstream.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(300).collect();
        tracing::warn!(%status, "library: S3 GET refused: {excerpt}");
        return err(StatusCode::BAD_GATEWAY, "object storage refused the read");
    }
    // headers first: the body stream consumes the response
    let content_length = upstream
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut r = if *method == Method::HEAD {
        Response::new(axum::body::Body::empty())
    } else {
        Response::new(axum::body::Body::from_stream(upstream.bytes_stream()))
    };
    *r.status_mut() = status;
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(content_type) {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Some(len) = content_length.and_then(|l| HeaderValue::from_str(&l).ok()) {
        h.insert(header::CONTENT_LENGTH, len);
    }
    if let Some(d) = disposition.and_then(|d| HeaderValue::from_str(&d).ok()) {
        h.insert(header::CONTENT_DISPOSITION, d);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    r
}

pub fn router(state: LibraryState) -> Router {
    Router::new()
        .route("/api/books", get(list_books))
        .route(
            "/api/books",
            post(upload_books).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route("/api/books/{id}", patch(patch_book).delete(delete_book))
        .route("/api/books/{id}/cover", get(person_cover))
        // the library is a person's shelf, not a connector surface (ADR
        // 0004); this travels with the router wherever it is merged
        .layer(axum::middleware::from_fn(
            crate::viewer::refuse_connector_writes,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An EPUB in memory: named entries, deflated (as real ones are).
    fn build_epub(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write as _;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    const CONTAINER: &str = r##"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"##;

    const EPUB3_OPF: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="pub-id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Dune &amp; Dust</dc:title>
    <dc:creator>Frank Herbert</dc:creator>
    <dc:identifier id="calibre-id">calibre:42</dc:identifier>
    <dc:identifier id="pub-id">urn:isbn:9780441172719</dc:identifier>
    <meta property="belongs-to-collection" id="c01">Dune Chronicles</meta>
    <meta refines="#c01" property="collection-type">series</meta>
    <meta refines="#c01" property="group-position">1.5</meta>
  </metadata>
  <manifest>
    <item id="opf" href="content.opf" media-type="application/oebps-package+xml"/>
    <item id="cover" href="images/cover.jpg" media-type="image/jpeg" properties="cover-image"/>
  </manifest>
</package>"##;

    const EPUB2_OPF: &str = r##"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>The Hobbit</dc:title>
    <dc:creator>J. R. R. Tolkien</dc:creator>
    <dc:identifier scheme="ISBN">isbn:123</dc:identifier>
    <meta name="cover" content="cover-img"/>
    <meta name="calibre:series" content="Middle-earth"/>
    <meta name="calibre:series_index" content="0.5"/>
  </metadata>
  <manifest>
    <item id="cover-img" href="cover.png" media-type="image/png"/>
  </manifest>
</package>"##;

    const ROOT_CONTAINER: &str = r##"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"##;

    fn spool_of(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.epub");
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn container_names_the_opf() {
        assert_eq!(
            container_rootfile(CONTAINER.as_bytes()).unwrap(),
            "OEBPS/content.opf"
        );
        assert!(container_rootfile(b"<container/>").is_err());
    }

    #[test]
    fn epub3_metadata_parses() {
        let m = parse_opf(EPUB3_OPF).unwrap();
        assert_eq!(
            m.title.as_deref(),
            Some("Dune & Dust"),
            "entities unescaped"
        );
        assert_eq!(m.creator.as_deref(), Some("Frank Herbert"));
        // the unique-identifier wins over the earlier calibre id
        assert_eq!(m.identifier.as_deref(), Some("urn:isbn:9780441172719"));
        assert_eq!(
            (m.series.as_deref(), m.series_index),
            (Some("Dune Chronicles"), Some(1.5))
        );
        let c = m.cover.unwrap();
        assert_eq!(
            (c.href.as_str(), c.media_type.as_str()),
            ("images/cover.jpg", "image/jpeg")
        );
    }

    #[test]
    fn epub2_metadata_parses() {
        let m = parse_opf(EPUB2_OPF).unwrap();
        assert_eq!(m.title.as_deref(), Some("The Hobbit"));
        assert_eq!(m.identifier.as_deref(), Some("isbn:123"));
        assert_eq!(
            (m.series.as_deref(), m.series_index),
            (Some("Middle-earth"), Some(0.5))
        );
        let c = m.cover.unwrap();
        assert_eq!(
            (c.href.as_str(), c.media_type.as_str()),
            ("cover.png", "image/png")
        );
    }

    #[test]
    fn hrefs_resolve_within_the_opf_directory_and_refuse_traversal() {
        assert_eq!(
            resolve_href("OEBPS/content.opf", "images/cover.jpg").as_deref(),
            Some("OEBPS/images/cover.jpg")
        );
        assert_eq!(
            resolve_href("content.opf", "cover.png").as_deref(),
            Some("cover.png")
        );
        assert_eq!(
            resolve_href("OEBPS/content.opf", "cover%20image.jpg").as_deref(),
            Some("OEBPS/cover image.jpg")
        );
        assert_eq!(
            resolve_href("OEBPS/content.opf", "cover.jpg#frag").as_deref(),
            Some("OEBPS/cover.jpg")
        );
        assert_eq!(
            resolve_href("OEBPS/content.opf", "/abs/cover.jpg").as_deref(),
            Some("abs/cover.jpg")
        );
        assert_eq!(
            resolve_href("OEBPS/content.opf", "../secret"),
            None,
            "zip-slip refused"
        );
        assert_eq!(
            resolve_href("OEBPS/content.opf", "..%2fsecret"),
            None,
            "encoded traversal refused"
        );
    }

    #[test]
    fn inspect_reads_metadata_and_cover_without_unpacking() {
        let cover_bytes: &[u8] = b"\xff\xd8\xff\xe0not-really-a-jpeg";
        let epub = build_epub(&[
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", EPUB3_OPF.as_bytes()),
            ("OEBPS/images/cover.jpg", cover_bytes),
        ]);
        let (_dir, path) = spool_of(&epub);
        let i = inspect_spool(&path).unwrap();
        assert_eq!(
            (i.title.as_str(), i.author.as_str()),
            ("Dune & Dust", "Frank Herbert")
        );
        assert_eq!(i.identifier.as_deref(), Some("urn:isbn:9780441172719"));
        assert_eq!(
            (i.series.as_deref(), i.series_index),
            (Some("Dune Chronicles"), Some(1.5))
        );
        assert_eq!(i.size as usize, epub.len());
        assert_eq!(i.sha256, hex::encode(sha2::Sha256::digest(&epub)));
        let (bytes, ext) = i.cover.unwrap();
        assert_eq!(ext, "jpg");
        assert_eq!(bytes, cover_bytes);
    }

    #[test]
    fn inspect_an_epub2_book_at_the_zip_root() {
        let epub = build_epub(&[
            ("META-INF/container.xml", ROOT_CONTAINER.as_bytes()),
            ("content.opf", EPUB2_OPF.as_bytes()),
            ("cover.png", b"\x89PNG\r\n\x1a\n"),
        ]);
        let (_dir, path) = spool_of(&epub);
        let i = inspect_spool(&path).unwrap();
        assert_eq!(
            (i.title.as_str(), i.author.as_str()),
            ("The Hobbit", "J. R. R. Tolkien")
        );
        assert_eq!(i.cover.unwrap().1, "png");
    }

    #[test]
    fn inspect_rejects_a_non_epub_and_a_missing_title() {
        let (_d1, junk) = spool_of(b"this is not a zip");
        assert!(inspect_spool(&junk).unwrap_err().contains("not an EPUB"));
        let no_title = EPUB3_OPF.replace("<dc:title>Dune &amp; Dust</dc:title>", "");
        let epub = build_epub(&[
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", no_title.as_bytes()),
        ]);
        let (_d2, path) = spool_of(&epub);
        assert_eq!(inspect_spool(&path).unwrap_err(), "the OPF has no dc:title");
        // a book without a cover is still a book
        let no_cover = build_epub(&[
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", EPUB3_OPF.as_bytes()),
        ]);
        let (_d3, path) = spool_of(&no_cover);
        assert!(inspect_spool(&path).unwrap().cover.is_none());
    }

    #[test]
    fn read_entry_enforces_its_cap() {
        let epub = build_epub(&[("big.txt", &[b'x'; 100])]);
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(epub)).unwrap();
        assert!(read_entry(&mut zip, "big.txt", 10).is_err());
        assert_eq!(read_entry(&mut zip, "big.txt", 100).unwrap().len(), 100);
    }

    #[test]
    fn percent_decode_rejects_garbage() {
        assert_eq!(percent_decode("a%20b").as_deref(), Some("a b"));
        assert_eq!(percent_decode("%C3%A9").as_deref(), Some("é"));
        assert_eq!(percent_decode("plain").as_deref(), Some("plain"));
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%4"), None);
    }

    #[test]
    fn cursors_round_trip() {
        let id = Uuid::now_v7();
        assert_eq!(
            parse_cursor(Some(&cursor(123, id))).unwrap(),
            Some((123, id))
        );
        assert_eq!(parse_cursor(None).unwrap(), None);
        assert_eq!(parse_cursor(Some("")).unwrap(), None);
        assert!(parse_cursor(Some("junk")).is_err());
        assert!(parse_cursor(Some("12:not-a-uuid")).is_err());
    }

    /// A SERVER-mode library on the in-memory bucket, with one signed-in
    /// person, plus a request builder that looks like require_auth ran.
    struct TestLib {
        app: Router,
        store: SharedStore,
        s3: Arc<S3Config>,
        user: Uuid,
        human: Uuid,
        _dir: tempfile::TempDir,
    }

    fn test_lib() -> TestLib {
        use taisce_store::{BlockStore, PrincipalKind};
        let mut raw = SqliteStore::open_in_memory().unwrap();
        let human = raw.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let user = raw.auth_ensure_owner(human, "Tom", 1).unwrap().id;
        let store = SharedStore::new(raw);
        let dir = tempfile::tempdir().unwrap();
        let s3 = Arc::new(S3Config::for_test());
        let app = router(LibraryState {
            store: store.clone(),
            s3: Some(s3.clone()),
            server_mode: true,
            local_human: human,
            spool_dir: dir.path().join("spool"),
        });
        TestLib { app, store, s3, user, human, _dir: dir }
    }

    impl TestLib {
        async fn send(&self, method: &str, uri: &str, content_type: Option<&str>, body: Vec<u8>) -> (StatusCode, Value) {
            use tower::ServiceExt as _;
            let mut b = axum::http::Request::builder().method(method).uri(uri).extension(crate::auth::Authenticated {
                user_id: self.user,
                grant_id: Uuid::now_v7(),
                client_id: "test".into(),
                principal: "claude:test".into(),
                owner_app: true,
                human: self.human,
                instance_owner: true,
                web_session: false,
            });
            if let Some(ct) = content_type {
                b = b.header("content-type", ct);
            }
            let res = self.app.clone().oneshot(b.body(axum::body::Body::from(body)).unwrap()).await.unwrap();
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 24).await.unwrap();
            (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
        }

        async fn upload(&self, files: &[(&str, Vec<u8>)]) -> Value {
            let boundary = "testboundary42";
            let mut body = Vec::new();
            for (name, bytes) in files {
                body.extend_from_slice(
                    format!("--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{name}\"\r\nContent-Type: application/epub+zip\r\n\r\n").as_bytes(),
                );
                body.extend_from_slice(bytes);
                body.extend_from_slice(b"\r\n");
            }
            body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            let (st, v) = self.send("POST", "/api/books", Some(&format!("multipart/form-data; boundary={boundary}")), body).await;
            assert_eq!(st, StatusCode::OK, "{v}");
            v
        }
    }

    fn epub_of(title: &str, author: &str, identifier: &str, series: &str, extra: &str, cover: bool) -> Vec<u8> {
        let manifest = if cover { r#"<item id="c" href="cover.jpg" media-type="image/jpeg" properties="cover-image"/>"# } else { "" };
        let opf = format!(
            r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title><dc:creator>{author}</dc:creator><dc:identifier id="id">{identifier}</dc:identifier><meta name="calibre:series" content="{series}"/><meta name="x" content="{extra}"/></metadata><manifest>{manifest}</manifest></package>"#
        );
        let mut entries: Vec<(&str, &[u8])> = vec![("META-INF/container.xml", ROOT_CONTAINER.as_bytes()), ("content.opf", opf.as_bytes())];
        if cover {
            entries.push(("cover.jpg", b"\xff\xd8\xffjpeg"));
        }
        build_epub(&entries)
    }

    /// An updated upload changes the FILE only: a title the person edited
    /// survives a re-upload that still carries the old one (the edited
    /// title is what the devices named their copy after).
    #[tokio::test]
    async fn an_updated_upload_never_touches_title_author_or_series() {
        let t = test_lib();
        let v = t.upload(&[("dune.epub", epub_of("Dune", "Frank Herbert", "urn:isbn:1", "Dune Chronicles", "v1", false))]).await;
        assert_eq!(v["results"][0]["status"], "added", "{v}");
        let id = v["results"][0]["book"]["id"].as_str().unwrap().to_string();
        let (st, b) = t
            .send("PATCH", &format!("/api/books/{id}"), Some("application/json"), br#"{"title":"Dune (my edit)","author":"F. Herbert","series":"Mine"}"#.to_vec())
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        // new bytes, same identifier, the OLD metadata inside
        let v = t.upload(&[("dune-v2.epub", epub_of("Dune", "Frank Herbert", "urn:isbn:1", "Dune Chronicles", "v2", true))]).await;
        assert_eq!(v["results"][0]["status"], "updated", "{v}");
        let b = t.store.lock(Scope::User(t.user)).book_get(Uuid::parse_str(&id).unwrap()).unwrap();
        assert_eq!((b.title.as_str(), b.author.as_str(), b.series.as_deref()), ("Dune (my edit)", "F. Herbert", Some("Mine")));
        assert_eq!(b.original_filename, "dune-v2.epub", "the file did move");
        assert!(b.cover_s3_key.is_some(), "and its new cover");
        assert_eq!(t.s3.memory.keys().len(), 2, "epub + cover: {:?}", t.s3.memory.keys());
    }

    #[test]
    fn disposition_carries_an_ascii_fallback_and_the_exact_utf8_name() {
        assert_eq!(
            content_disposition("Frank Herbert - Dune_ Messiah.epub"),
            "attachment; filename=\"Frank Herbert - Dune_ Messiah.epub\"; filename*=UTF-8''Frank%20Herbert%20-%20Dune_%20Messiah.epub"
        );
        let d = content_disposition("Seán Ó Faoláin - Stories.epub");
        assert!(d.starts_with("attachment; filename=\"Se_n _ Faol_in - Stories.epub\"; "), "{d}");
        assert!(d.ends_with("filename*=UTF-8''Se%C3%A1n%20%C3%93%20Faol%C3%A1in%20-%20Stories.epub"), "{d}");
        assert!(HeaderValue::from_str(&d).is_ok(), "always a valid header");
    }

    /// FAT is case-insensitive: two books whose derived names differ only in
    /// case would overwrite each other on the device. SQLite's NOCASE (rule
    /// (c)) folds ASCII only, so "Éire"/"ÉIRE" reaches the clash check.
    #[test]
    fn the_clash_check_compares_derived_names_case_insensitively() {
        use taisce_store::{BlockStore, PrincipalKind};
        let mut s = SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let user = s.auth_ensure_owner(p, "Tom", 1).unwrap().id;
        s.with_scope_for_test(Scope::User(user), |s| {
            let nb = NewBook {
                title: "Éire: A History".into(),
                author: "Ann".into(),
                series: None,
                series_index: None,
                book_identifier: None,
                original_filename: "x.epub".into(),
                file_sha256: "sha".into(),
                size: 1,
                file_s3_key: "k".into(),
                cover_s3_key: None,
            };
            s.book_insert(Uuid::now_v7(), &nb, 10).unwrap();
            assert!(matches!(decide(s, user, "other", None, "ÉIRE_ A HISTORY", "ann").unwrap(), Decision::Clash(_)));
            // ':' and '_' derive the same name too
            assert!(matches!(decide(s, user, "other", None, "Éire_ A History", "Ann").unwrap(), Decision::Clash(_)));
            assert!(matches!(decide(s, user, "other", None, "Éire: A Future", "Ann").unwrap(), Decision::Add));
        });
    }

    #[test]
    fn multipart_filenames_lose_their_paths() {
        assert_eq!(clean_filename("/tmp/evil/Dune.epub"), "Dune.epub");
        assert_eq!(clean_filename("C:\\books\\Dune.epub"), "Dune.epub");
        assert_eq!(clean_filename("  "), "book.epub");
    }
}
