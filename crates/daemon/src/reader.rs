//! The web reader's daemon half (`Taisce › EPUB proposal`, *Reading in the
//! browser*): the bytes of a book for the browser, and the reading position
//! as one more kosync device.
//!
//! - `GET /api/books/{id}/file` (and HEAD) → the EPUB, streamed from object
//!   storage through the daemon. The OPDS route 302s to a presigned URL,
//!   which a browser under the UI's `connect-src 'self'` cannot fetch. A
//!   `Range` header is passed to S3 as is (206 / 416 come back). Sent docs
//!   need nothing new: `GET /api/docs/{id}/epub` already renders them.
//! - `GET /api/books/{id}/position` · `GET /api/docs/{id}/position` →
//!   `{document, position}`: the `kosync_progress` row the devices write,
//!   keyed exactly as they key it (MD5 of the derived filename: the book's
//!   `<Author> - <Title>.epub`, a doc's `Taisce - <Title>.epub`), or
//!   `position: null`. `percentage` is the protocol's 0..1 fraction and
//!   `timestamp` unix seconds, as a device would read them.
//! - `PUT …/position {progress, percentage, device_id, base}` → the same
//!   shape, written as device "Taisce web" with the server's clock, so
//!   newest-wins against the devices holds as it does for them. One guard
//!   on top: `base` is the timestamp of the row the reader last saw. When
//!   the stored row came from ANOTHER device and is newer than `base`, the
//!   write is refused (409 with that row), so a tab left open overnight
//!   cannot drag you back from where the Kindle got to. The reader then
//!   moves to the newer position and says so.
//!
//! All of it is the person's own surface (their app or web session, never a
//! connector, an agent principal or a PAT) in the caller's scope: someone
//! else's book or doc is the 404 of one that never existed. Positions need
//! a signed-in user (`kosync_progress` keys to `auth_users`), so LOCAL mode
//! answers 404 for them and the reader keeps its place in the browser; the
//! book file needs a bucket, like every library route.

use crate::booknaming::{kosync_document_digest, md5_hex};
use crate::s3sign::S3Config;
use crate::store_ext::with_store;
use crate::viewer::Viewer;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use taisce_store::books::KosyncProgress;
use taisce_store::{SharedStore, SqliteStore, StoreError};
use uuid::Uuid;

/// The device name every web-reader push carries (the toast on a Kindle's
/// side says "Taisce web"; KOReader shows it in its sync prompt).
pub const DEVICE: &str = "Taisce web";
/// Bounds, matching the kosync routes' (`kosync.rs`).
const MAX_LABEL: usize = 128;
const MAX_PROGRESS: usize = 4096;

#[derive(Clone)]
pub struct ReaderState {
    pub store: SharedStore,
    /// None = LOCAL mode or no bucket: the book file 404s.
    pub s3: Option<Arc<S3Config>>,
    pub server_mode: bool,
    pub local_human: Uuid,
}

impl FromRequestParts<ReaderState> for Viewer {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &ReaderState) -> Result<Self, Response> {
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
    crate::library::fail(e)
}

// ---- the book file ----

async fn book_file(State(st): State<ReaderState>, v: Viewer, headers: HeaderMap, method: Method, Path(id): Path<Uuid>) -> Response {
    if let Err(r) = crate::sentdocs::owner_gate(&v, &headers) {
        return r;
    }
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
    let range = headers.get(header::RANGE).and_then(|r| r.to_str().ok()).map(str::to_owned);
    let head = method == Method::HEAD;
    let obj = match s3.get_object(&book.file_s3_key, head, range.as_deref()).await {
        Ok(Some(o)) => o,
        // the row outlived its object (a failed delete's other half): the
        // book is not readable, which is a 404 like any missing book
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::warn!("reader: object storage: {e:#}");
            return err(StatusCode::BAD_GATEWAY, "object storage is unavailable");
        }
    };
    let mut r = Response::new(obj.body);
    *r.status_mut() = StatusCode::from_u16(obj.status).unwrap_or(StatusCode::OK);
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/epub+zip"));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Some(v) = obj.content_length.and_then(|l| HeaderValue::from_str(&l).ok()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    if let Some(v) = obj.content_range.and_then(|l| HeaderValue::from_str(&l).ok()) {
        h.insert(header::CONTENT_RANGE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    r
}

// ---- positions ----

/// What a position route reads: a library book or a (sent or unsent) doc.
#[derive(Clone, Copy)]
enum Item {
    Book(Uuid),
    Doc(Uuid),
}

/// The kosync document key for an item the scope can see (else NotFound):
/// the digest the devices compute from the filename they saved it under.
fn digest(s: &SqliteStore, item: Item, user: Uuid) -> taisce_store::Result<String> {
    match item {
        Item::Book(id) => {
            let b = s.book_get(id)?;
            Ok(kosync_document_digest(&b.title, &b.author))
        }
        Item::Doc(id) => {
            let (_, filename) = crate::sentdocs::one_book(s, id, Some(user))?;
            Ok(md5_hex(filename.as_bytes()))
        }
    }
}

/// A row as a device would read it (the kosync pull's fields).
fn position_json(p: &KosyncProgress) -> Value {
    json!({
        "progress": p.progress,
        "percentage": p.percentage,
        "device": p.device,
        "device_id": p.device_id,
        "timestamp": p.updated_at,
    })
}

/// The user a position route answers for, after the person gate: SERVER
/// mode's signed-in user (LOCAL has no `auth_users` row to key to: 404).
fn position_user(v: &Viewer, headers: &HeaderMap) -> Result<Uuid, Response> {
    crate::sentdocs::owner_gate(v, headers)?;
    v.user.ok_or_else(not_found)
}

async fn get_position(st: ReaderState, v: Viewer, headers: HeaderMap, item: Item) -> Response {
    let user = match position_user(&v, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let got = with_store(&st.store, v.scope, move |s| -> taisce_store::Result<(String, Option<KosyncProgress>)> {
        let d = digest(s, item, user)?;
        let row = s.kosync_get(user, &d)?;
        Ok((d, row))
    })
    .await;
    match got {
        Ok((d, row)) => Json(json!({"document": d, "position": row.as_ref().map(position_json)})).into_response(),
        Err(e) => fail(e),
    }
}

#[derive(Deserialize)]
struct PositionIn {
    progress: String,
    percentage: f64,
    device_id: String,
    /// The timestamp of the row the reader last saw (0: none).
    #[serde(default)]
    base: i64,
}

/// The write's outcome under one store lock: written, or refused because
/// another device moved on since the reader last looked.
enum Put {
    Written(String, KosyncProgress),
    Newer(String, KosyncProgress),
}

/// Newest-wins with the stale-tab guard (see the module docs). Pure over
/// the store so the rule is tested without HTTP.
pub(crate) fn put_position(
    s: &mut SqliteStore,
    user: Uuid,
    document: &str,
    progress: &str,
    percentage: f64,
    device_id: &str,
    base: i64,
    now: i64,
) -> taisce_store::Result<Option<KosyncProgress>> {
    if let Some(cur) = s.kosync_get(user, document)?
        && cur.device_id != device_id
        && cur.updated_at > base
    {
        return Ok(Some(cur));
    }
    s.kosync_upsert(user, document, progress, percentage, DEVICE, device_id, now)?;
    Ok(None)
}

async fn put(st: ReaderState, v: Viewer, headers: HeaderMap, item: Item, body: Result<Json<PositionIn>, JsonRejection>) -> Response {
    let user = match position_user(&v, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, e.body_text()),
    };
    if req.progress.len() > MAX_PROGRESS {
        return err(StatusCode::BAD_REQUEST, format!("progress: at most {MAX_PROGRESS} bytes"));
    }
    if !req.percentage.is_finite() || !(0.0..=1.0).contains(&req.percentage) {
        return err(StatusCode::BAD_REQUEST, "percentage: a fraction from 0 to 1");
    }
    if req.device_id.trim().is_empty() || req.device_id.len() > MAX_LABEL {
        return err(StatusCode::BAD_REQUEST, format!("device_id: 1-{MAX_LABEL} chars"));
    }
    let ts = crate::auth::now();
    let out = with_store(&st.store, v.scope, move |s| -> taisce_store::Result<Put> {
        let d = digest(s, item, user)?;
        match put_position(s, user, &d, &req.progress, req.percentage, &req.device_id, req.base, ts)? {
            Some(newer) => Ok(Put::Newer(d, newer)),
            None => {
                let row = s.kosync_get(user, &d)?.ok_or_else(|| StoreError::NotFound("position".into()))?;
                Ok(Put::Written(d, row))
            }
        }
    })
    .await;
    match out {
        Ok(Put::Written(d, row)) => Json(json!({"document": d, "position": position_json(&row)})).into_response(),
        Ok(Put::Newer(d, row)) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "another device has a newer position", "document": d, "position": position_json(&row)})),
        )
            .into_response(),
        Err(e) => fail(e),
    }
}

async fn book_position_get(State(st): State<ReaderState>, v: Viewer, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    get_position(st, v, headers, Item::Book(id)).await
}

async fn book_position_put(
    State(st): State<ReaderState>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Result<Json<PositionIn>, JsonRejection>,
) -> Response {
    put(st, v, headers, Item::Book(id), body).await
}

async fn doc_position_get(State(st): State<ReaderState>, v: Viewer, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    get_position(st, v, headers, Item::Doc(id)).await
}

async fn doc_position_put(
    State(st): State<ReaderState>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Result<Json<PositionIn>, JsonRejection>,
) -> Response {
    put(st, v, headers, Item::Doc(id), body).await
}

pub fn router(state: ReaderState) -> Router {
    Router::new()
        .route("/api/books/{id}/file", get(book_file))
        .route("/api/books/{id}/position", get(book_position_get).put(book_position_put))
        .route("/api/docs/{id}/position", get(doc_position_get).put(doc_position_put))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        // the person's own surface (ADR 0004): a connector never writes here
        .layer(axum::middleware::from_fn(crate::viewer::refuse_connector_writes))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use taisce_store::{BlockStore, PrincipalKind, Scope};

    fn store() -> (SharedStore, Uuid) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let human = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let user = s.auth_ensure_owner(human, "Tom", 1).unwrap().id;
        (SharedStore::new(s), user)
    }

    const DOC: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn the_reader_writes_as_taisce_web_and_newest_wins() {
        let (store, user) = store();
        let mut s = store.lock(Scope::User(user));
        assert!(put_position(&mut s, user, DOC, "/body/DocFragment[2]/body/p[1]/text()[1].0", 0.1, "web-1", 0, 100).unwrap().is_none());
        let row = s.kosync_get(user, DOC).unwrap().unwrap();
        assert_eq!((row.device.as_str(), row.device_id.as_str(), row.updated_at, row.percentage), (DEVICE, "web-1", 100, 0.1));
        // its own later write moves it, whatever base it carries
        assert!(put_position(&mut s, user, DOC, "/x", 0.2, "web-1", 0, 110).unwrap().is_none());
        assert_eq!(s.kosync_get(user, DOC).unwrap().unwrap().progress, "/x");
    }

    #[test]
    fn a_stale_tab_never_drags_a_device_back() {
        let (store, user) = store();
        let mut s = store.lock(Scope::User(user));
        // the reader saw nothing; then the Kindle pushes at 200
        s.kosync_upsert(user, DOC, "/kindle", 0.57, "KindlePaperWhite6", "kpw6", 200).unwrap();
        // a write based on an older look is refused with the Kindle's row
        let newer = put_position(&mut s, user, DOC, "/web", 0.1, "web-1", 150, 210).unwrap().unwrap();
        assert_eq!((newer.device.as_str(), newer.progress.as_str()), ("KindlePaperWhite6", "/kindle"));
        assert_eq!(s.kosync_get(user, DOC).unwrap().unwrap().progress, "/kindle", "unmoved");
        // once the reader has seen it (base = its stamp), the next turn wins
        assert!(put_position(&mut s, user, DOC, "/web", 0.58, "web-1", 200, 220).unwrap().is_none());
        let row = s.kosync_get(user, DOC).unwrap().unwrap();
        assert_eq!((row.progress.as_str(), row.device.as_str(), row.updated_at), ("/web", DEVICE, 220));
        // another browser is another device: the same guard applies
        assert!(put_position(&mut s, user, DOC, "/other", 0.3, "web-2", 0, 230).unwrap().is_some());
    }

    #[test]
    fn a_doc_and_a_book_key_as_the_devices_do() {
        // the book: MD5 of `<Author> - <Title>.epub`, the same digest the
        // shelf joins on (kosync.rs)
        assert_eq!(kosync_document_digest("Piranesi", "Susanna Clarke"), md5_hex(b"Susanna Clarke - Piranesi.epub"));
    }
}
