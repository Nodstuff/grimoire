//! Docs on e-readers (`Taisce › EPUB proposal`, Phase 2): the owner routes
//! that send a doc, download it as an EPUB and store its diagram pictures,
//! and the gathering both those routes and the OPDS feed (`opds.rs`, the
//! `/opds/docs` shelf and `/opds/doc/{id}/file`) share.
//!
//! A doc is *sent* when it carries the `ereader` frontmatter tag that the
//! person set (`taisce_store::epub`). A sent doc with descendants is ONE book:
//! its own text (if any) first, then every visible descendant in tree order,
//! nested in the contents by depth; a sent doc without any is a one-chapter
//! book. A descendant is not also a book of its own unless it is tagged too.
//!
//! Owner routes (the person's own app or web session, or LOCAL mode; never
//! a connector, an agent principal or a PAT — PATs never open `/api`), each
//! in the caller's scope, so a doc they cannot see is 404 like a missing one:
//!
//! - `GET /api/docs/{id}/epub` → the EPUB (`Content-Disposition` carries the
//!   name an e-reader would save it under). Any visible doc, sent or not.
//! - `GET /api/docs/{id}/ereader` → `{sent, tagged, in_sent_book, filename,
//!   chapters, diagrams: [{block_id, lang, source, source_hash, has_image}]}`
//! - `POST /api/docs/{id}/ereader {send}` → the same; adds or removes the tag
//!   in the doc's frontmatter as the person (one green op through the gate,
//!   like any edit of theirs). A tag someone else set (a shared workspace)
//!   is theirs: 409 either way.
//! - `POST /api/docs/{id}/epub-images {images: [{block_id, source_hash, png,
//!   width, height}]}` → `{stored, ignored}`: PNGs (base64) of the book's
//!   diagram fences, drawn by the web app; a block outside the book or a
//!   hash that is not the fence's current one is ignored.
//! - `GET /api/ereader/sent` → `{docs: [...]}` the person's sent books, with
//!   kosync progress joined on the derived filename (SERVER mode).
//!
//! Names: the feed serves every doc as author "Taisce" and its title, so both
//! e-reader clients save `Taisce - <Title>.epub` (booknaming's rule; the
//! kosync digest is md5 of it). Names must be unique against the person's
//! books and other sent docs (case-insensitively): on a clash, the doc
//! created later (by id) gets ` (<Workspace>)` appended, then ` (<id>)`,
//! then a counter — deterministic, never refused.

use crate::booknaming::{derived_book_filename, feed_title, kosync_document_digest};
use crate::epub::{self, BookSpec, Chapter, Picture, SpecBlock};
use crate::store_ext::with_store;
use crate::viewer::Viewer;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use taisce_store::books::BookPage;
use taisce_store::epub::EREADER_TAG;
use taisce_store::{BlockStore, BlockType, OpInput, OpKind, SharedStore, SqliteStore, StoreError};
use uuid::Uuid;

/// The author every sent doc is served under (the filename's first half).
pub const FEED_AUTHOR: &str = "Taisce";
/// A picture upload's caps.
const MAX_PNG_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGES: usize = 32;
const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_DIM: i64 = 8000;

#[derive(Clone)]
pub struct SentState {
    pub store: SharedStore,
    pub server_mode: bool,
    pub local_human: Uuid,
}

impl FromRequestParts<SentState> for Viewer {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &SentState) -> Result<Self, Response> {
        Self::from_parts(parts, st.server_mode, st.local_human)
    }
}

fn err(status: StatusCode, msg: impl std::fmt::Display) -> Response {
    (status, Json(json!({"error": msg.to_string()}))).into_response()
}

pub(crate) fn fail(e: StoreError) -> Response {
    let code = match &e {
        StoreError::NotFound(_) => StatusCode::NOT_FOUND,
        StoreError::Forbidden(_) => StatusCode::FORBIDDEN,
        StoreError::InvalidOp(_) | StoreError::StaleBase { .. } => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, Json(json!({"error": e.to_string(), "code": e.code()}))).into_response()
}

// ---- gathering (store side, in the caller's scope) ----

/// A frontmatter block: `---` then at least one more line. A lone `---`
/// (a rule) is content.
fn is_frontmatter(content: &str) -> bool {
    content.starts_with("---") && content.contains('\n')
}

/// A doc's content blocks in tree order: no comments, canvases or
/// frontmatter.
fn content_blocks(s: &SqliteStore, doc: Uuid) -> taisce_store::Result<Vec<SpecBlock>> {
    fn walk(nodes: &[taisce_store::BlockNode], out: &mut Vec<SpecBlock>) {
        for n in nodes {
            let b = &n.block;
            if matches!(b.block_type, BlockType::Comment | BlockType::CanvasScene) || is_frontmatter(&b.content) {
                continue;
            }
            if !b.content.trim().is_empty() {
                out.push(SpecBlock { id: b.id, markdown: b.content.clone() });
            }
            walk(&n.children, out);
        }
    }
    let tree = s.read_doc(doc)?;
    let mut out = Vec::new();
    walk(&tree.roots, &mut out);
    Ok(out)
}

/// The book a doc makes: the doc's own text (when it has any, or when it is
/// the only chapter), then every visible descendant depth-first in tree
/// order (siblings as the tree sorts them). The root's children are depth 1.
pub(crate) fn gather(s: &SqliteStore, root: Uuid) -> taisce_store::Result<BookSpec> {
    let doc = s.get_doc(root)?;
    let sub = s.doc_subtree(root)?;
    let mut kids: HashMap<Uuid, Vec<&taisce_store::Doc>> = HashMap::new();
    for d in &sub {
        if d.id != root
            && let Some(p) = d.parent_id
        {
            kids.entry(p).or_default().push(d);
        }
    }
    let mut chapters = Vec::new();
    let own = content_blocks(s, root)?;
    let has_kids = kids.get(&root).is_some_and(|k| !k.is_empty());
    if !own.is_empty() || !has_kids {
        chapters.push(Chapter { doc_id: root, title: doc.title.clone(), depth: 1, blocks: own });
    }
    fn walk(
        s: &SqliteStore,
        at: Uuid,
        depth: usize,
        kids: &HashMap<Uuid, Vec<&taisce_store::Doc>>,
        out: &mut Vec<Chapter>,
        seen: &mut HashSet<Uuid>,
    ) -> taisce_store::Result<()> {
        for d in kids.get(&at).map(Vec::as_slice).unwrap_or_default() {
            if !seen.insert(d.id) {
                continue;
            }
            out.push(Chapter { doc_id: d.id, title: d.title.clone(), depth, blocks: content_blocks(s, d.id)? });
            walk(s, d.id, depth + 1, kids, out, seen)?;
        }
        Ok(())
    }
    let mut seen = HashSet::from([root]);
    walk(s, root, 1, &kids, &mut chapters, &mut seen)?;
    let ids: Vec<Uuid> = chapters.iter().map(|c| c.doc_id).collect();
    let mut pictures = BTreeMap::new();
    for img in s.epub_images(&ids)? {
        pictures.insert((img.block_id, img.source_hash), Picture { png: img.png, width: img.width, height: img.height });
    }
    let creator = s.doc_workspace_name(root)?.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| FEED_AUTHOR.to_string());
    let modified = s.docs_last_change(&ids)?.map(|t| ledger_time(&t)).unwrap_or_else(|| "2000-01-01T00:00:00Z".into());
    Ok(BookSpec { id: root, title: doc.title, creator, language: "en".into(), modified, chapters, pictures })
}

/// `2026-10-23T09:30:12.345Z` (or sqlite's `2026-10-23 09:30:12`) →
/// `2026-10-23T09:30:12Z`, the form `dcterms:modified` takes.
fn ledger_time(t: &str) -> String {
    let t = t.replace(' ', "T");
    let base = t.get(..19).unwrap_or(&t);
    format!("{base}Z")
}

/// One of the person's sent books, as the feed and the shelf show it.
pub(crate) struct SentBook {
    pub spec: BookSpec,
    /// the title the feed serves (the doc's, disambiguated on a clash)
    pub feed_title: String,
    pub filename: String,
    pub workspace: Option<String>,
}

/// `title` with ` (<suffix>)`, the title cut (on a char boundary) so the
/// suffix survives the feed's 100-byte rule.
fn with_suffix(title: &str, suffix: &str) -> String {
    // "Taisce - " + title + " (" + suffix + ")" within 100 bytes
    let room = 100usize.saturating_sub(FEED_AUTHOR.len() + 3 + suffix.len() + 3);
    let mut end = title.len().min(room);
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} ({suffix})", title[..end].trim_end())
}

/// The feed names of these docs, unique against `taken` (lowercase derived
/// names: the person's books) and each other. Docs are named oldest id
/// first, so an existing doc keeps its name when a newer one clashes.
pub(crate) fn assign_names(docs: &[(Uuid, String, Option<String>)], taken: &HashSet<String>) -> HashMap<Uuid, String> {
    let mut order: Vec<&(Uuid, String, Option<String>)> = docs.iter().collect();
    order.sort_by_key(|d| d.0);
    let mut used = taken.clone();
    let mut out = HashMap::new();
    for (id, title, ws) in order {
        let mut candidates = vec![title.clone()];
        if let Some(w) = ws.as_deref().filter(|w| !w.trim().is_empty()) {
            candidates.push(with_suffix(title, w.trim()));
        }
        let short = id.simple().to_string();
        candidates.push(with_suffix(title, &short[short.len() - 8..]));
        let mut chosen = None;
        for c in candidates {
            let name = derived_book_filename(&c, FEED_AUTHOR).to_lowercase();
            if !used.contains(&name) {
                used.insert(name);
                chosen = Some(c);
                break;
            }
        }
        let chosen = chosen.unwrap_or_else(|| {
            (2..)
                .map(|n| with_suffix(title, &format!("{} {n}", &short[short.len() - 8..])))
                .find(|c| !used.contains(&derived_book_filename(c, FEED_AUTHOR).to_lowercase()))
                .expect("a free counter")
        });
        used.insert(derived_book_filename(&chosen, FEED_AUTHOR).to_lowercase());
        // the feed's form of it (what the devices name the file from)
        out.insert(*id, feed_title(&chosen, FEED_AUTHOR).0);
    }
    out
}

/// The derived names of the person's books (lowercase), for the clash
/// check. LOCAL mode has no shelf.
fn book_names(s: &SqliteStore, user: Option<Uuid>) -> taisce_store::Result<HashSet<String>> {
    let mut out = HashSet::new();
    let Some(user) = user else { return Ok(out) };
    let mut page = BookPage::default();
    loop {
        let books = s.book_list(user, &page, 500, None)?;
        for b in &books {
            out.insert(derived_book_filename(&b.title, &b.author).to_lowercase());
        }
        let Some(last) = books.last().filter(|_| books.len() == 500) else { break };
        page = BookPage { before: Some((last.added_at, last.id)), after: None };
    }
    Ok(out)
}

/// Every sent book of the scope's person, in title order, named.
pub(crate) fn sent_books(s: &SqliteStore, user: Option<Uuid>) -> taisce_store::Result<Vec<SentBook>> {
    let docs = s.ereader_sent_docs()?;
    let mut rows = Vec::new();
    for d in &docs {
        rows.push((d.id, d.title.clone(), s.doc_workspace_name(d.id)?));
    }
    let names = assign_names(&rows, &book_names(s, user)?);
    let mut out = Vec::new();
    for (id, _, workspace) in rows {
        let spec = gather(s, id)?;
        let feed = names.get(&id).cloned().unwrap_or_else(|| feed_title(&spec.title, FEED_AUTHOR).0);
        out.push(SentBook { filename: derived_book_filename(&feed, FEED_AUTHOR), feed_title: feed, spec, workspace });
    }
    Ok(out)
}

/// The book for one doc and the name it downloads under: a sent doc keeps
/// its feed name; any other doc its plain derived name.
pub(crate) fn one_book(s: &SqliteStore, doc: Uuid, user: Option<Uuid>) -> taisce_store::Result<(BookSpec, String)> {
    if s.ereader_sent(doc)?
        && let Some(b) = sent_books(s, user)?.into_iter().find(|b| b.spec.id == doc)
    {
        return Ok((b.spec, b.filename));
    }
    let spec = gather(s, doc)?;
    let name = derived_book_filename(&spec.title, FEED_AUTHOR);
    Ok((spec, name))
}

/// The response carrying an EPUB (GET or HEAD alike: axum drops a HEAD's
/// body, the headers stay).
pub(crate) fn epub_response(bytes: &[u8], filename: &str) -> Response {
    let mut r = (StatusCode::OK, bytes.to_vec()).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(epub::EPUB_CONTENT_TYPE));
    if let Ok(v) = HeaderValue::from_str(&crate::library::content_disposition(filename)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    r
}

pub(crate) fn render_failed(e: anyhow::Error) -> Response {
    tracing::error!("epub: render failed: {e:#}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "could not render the EPUB")
}

// ---- owner routes ----

/// The person's own surface (their app, their web session, LOCAL mode).
fn owner_gate(v: &Viewer, headers: &HeaderMap) -> Result<(), Response> {
    v.require_human_surface()?;
    if headers.contains_key(crate::mcp::PRINCIPAL_HEADER) {
        return Err(crate::viewer::forbidden("e-reader sending is done by a person, not an agent"));
    }
    Ok(())
}

async fn download(State(st): State<SentState>, v: Viewer, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    if let Err(r) = owner_gate(&v, &headers) {
        return r;
    }
    let user = v.user;
    match with_store(&st.store, v.scope, move |s| one_book(s, id, user)).await {
        Ok((spec, name)) => match epub::CACHE.get_or_render(&spec) {
            Ok(bytes) => epub_response(&bytes, &name),
            Err(e) => render_failed(e),
        },
        Err(e) => fail(e),
    }
}

/// What the doc view needs: is it sent, which diagrams lack a picture.
fn info(s: &SqliteStore, doc: Uuid, user: Option<Uuid>) -> taisce_store::Result<Value> {
    let sent = s.ereader_sent(doc)?;
    let tagged = s.doc_has_tag(doc, EREADER_TAG)?;
    let (spec, filename) = one_book(s, doc, user)?;
    // the nearest sent ancestor (this doc is one of its chapters)
    let mut in_book = Value::Null;
    if !sent {
        let mut at = s.get_doc(doc)?.parent_id;
        let mut hops = 0;
        while let Some(p) = at {
            hops += 1;
            if hops > 256 {
                break;
            }
            let Ok(pd) = s.get_doc(p) else { break };
            if s.ereader_sent(p)? {
                in_book = json!({"id": pd.id, "title": pd.title});
                break;
            }
            at = pd.parent_id;
        }
    }
    let mut diagrams = Vec::new();
    for ch in &spec.chapters {
        for b in &ch.blocks {
            for (lang, code) in epub::diagram_fences(&b.markdown) {
                let hash = epub::source_hash(&lang, &code);
                let has = spec.pictures.contains_key(&(b.id, hash.clone()));
                diagrams.push(json!({"block_id": b.id, "lang": lang, "source": code, "source_hash": hash, "has_image": has}));
            }
        }
    }
    Ok(json!({
        "sent": sent,
        "tagged": tagged,
        "in_sent_book": in_book,
        "filename": filename,
        "chapters": spec.chapters.len(),
        "diagrams": diagrams,
    }))
}

async fn get_info(State(st): State<SentState>, v: Viewer, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    if let Err(r) = owner_gate(&v, &headers) {
        return r;
    }
    let user = v.user;
    match with_store(&st.store, v.scope, move |s| info(s, id, user)).await {
        Ok(j) => Json(j).into_response(),
        Err(e) => fail(e),
    }
}

/// The frontmatter with `ereader` added to (or removed from) its tags, in
/// the block-list form the store's tag extraction reads. None = no change.
pub(crate) fn retag(frontmatter: Option<&str>, send: bool) -> Option<Option<String>> {
    let Some(fm) = frontmatter else {
        return send.then(|| Some(format!("---\ntags:\n  - {EREADER_TAG}\n---")));
    };
    let lines: Vec<&str> = fm.lines().collect();
    // the closing fence: the last `---`/`...` line after the first
    let close = lines.iter().enumerate().skip(1).rev().find(|(_, l)| matches!(l.trim_end(), "---" | "...")).map(|(i, _)| i)?;
    let mut body: Vec<String> = lines[1..close].iter().map(|l| l.to_string()).collect();
    let is_ours = |l: &str| {
        let t = l.trim_start();
        t.strip_prefix("- ").is_some_and(|v| v.trim().trim_matches(['"', '\'']).eq_ignore_ascii_case(EREADER_TAG))
    };
    let tags_at = body.iter().position(|l| l.trim_end() == "tags:" || l.trim_start().starts_with("tags:"));
    // the items of a block-list `tags:` (indented or not, `- x` lines)
    let items = |body: &Vec<String>, at: usize| {
        let mut end = at + 1;
        while end < body.len() && body[end].trim_start().starts_with("- ") {
            end += 1;
        }
        end
    };
    if send {
        match tags_at {
            Some(at) if body[at].trim_end() == "tags:" => {
                let end = items(&body, at);
                if body[at + 1..end].iter().any(|l| is_ours(l)) {
                    return None;
                }
                let indent = body.get(at + 1).filter(|_| end > at + 1).map(|l| l.len() - l.trim_start().len()).unwrap_or(2);
                body.insert(end, format!("{}- {EREADER_TAG}", " ".repeat(indent)));
            }
            Some(at) => {
                // `tags: [a, b]` or `tags: a`: rewritten as a block list
                let v = body[at].trim_start().trim_start_matches("tags:").trim().trim_matches(['[', ']']).to_string();
                let mut tags: Vec<String> =
                    v.split(',').map(|t| t.trim().trim_matches(['"', '\'']).to_string()).filter(|t| !t.is_empty()).collect();
                if !tags.iter().any(|t| t.eq_ignore_ascii_case(EREADER_TAG)) {
                    tags.push(EREADER_TAG.into());
                }
                body.splice(at..=at, std::iter::once("tags:".to_string()).chain(tags.into_iter().map(|t| format!("  - {t}"))));
            }
            None => {
                body.push("tags:".into());
                body.push(format!("  - {EREADER_TAG}"));
            }
        }
    } else {
        let Some(at) = tags_at.filter(|at| body[*at].trim_end() == "tags:") else { return None };
        let end = items(&body, at);
        let before = body.len();
        let kept: Vec<String> = body[at + 1..end].iter().filter(|l| !is_ours(l)).cloned().collect();
        if kept.len() == end - at - 1 {
            return None;
        }
        let tail: Vec<String> = body[end..].to_vec();
        body.truncate(at);
        if !kept.is_empty() {
            body.push("tags:".into());
            body.extend(kept);
        }
        body.extend(tail);
        debug_assert!(body.len() < before);
        if body.iter().all(|l| l.trim().is_empty()) {
            // nothing else in it: the block goes
            return Some(None);
        }
    }
    Some(Some(format!("{}\n{}\n{}", lines[0], body.join("\n"), lines[close])))
}

#[derive(Deserialize)]
struct SendReq {
    send: bool,
}

async fn set_sent(
    State(st): State<SentState>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Result<Json<SendReq>, JsonRejection>,
) -> Response {
    if let Err(r) = owner_gate(&v, &headers) {
        return r;
    }
    let Ok(Json(req)) = body else { return err(StatusCode::BAD_REQUEST, "expected {\"send\": true|false}") };
    let (user, human) = (v.user, v.human);
    let out = with_store(&st.store, v.scope, move |s| -> Result<Value, Response> {
        let sent = s.ereader_sent(id).map_err(fail)?;
        let tagged = s.doc_has_tag(id, EREADER_TAG).map_err(fail)?;
        if tagged && !sent {
            return Err(err(
                StatusCode::CONFLICT,
                "another member of this workspace sent this doc to their e-readers; a shared doc goes to whoever tagged it, and only they can stop it",
            ));
        }
        if sent != req.send {
            let tree = s.read_doc(id).map_err(fail)?;
            let fm = tree.roots.iter().map(|n| &n.block).find(|b| b.block_type != BlockType::Comment && is_frontmatter(&b.content));
            let first = tree.roots.iter().map(|n| &n.block).find(|b| b.block_type != BlockType::Comment);
            let op = match (retag(fm.map(|b| b.content.as_str()), req.send), fm) {
                (None, _) => None,
                (Some(Some(content)), Some(b)) => Some(OpKind::Replace { target: b.id, content }),
                (Some(None), Some(b)) => Some(OpKind::Delete { target: b.id }),
                (Some(Some(content)), None) => Some(OpKind::Insert {
                    block_id: Uuid::now_v7(),
                    parent_id: None,
                    order_key: taisce_store::order_key::between(None, first.map(|b| b.order_key.as_str())),
                    block_type: BlockType::Paragraph,
                    content,
                    refers_to: None,
                }),
                (Some(None), None) => None,
            };
            if let Some(kind) = op {
                let epoch = s.get_doc(id).map_err(fail)?.current_epoch;
                s.propose(id, epoch, human, vec![OpInput { kind, source_refs: vec!["ereader:send".into()] }]).map_err(fail)?;
                tracing::info!(target: crate::auth::AUDIT, event = if req.send { "ereader.send" } else { "ereader.stop" }, doc = %id, user = ?user);
            }
        }
        info(s, id, user).map_err(fail)
    })
    .await;
    match out {
        Ok(j) => Json(j).into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct ImageIn {
    block_id: Uuid,
    source_hash: String,
    png: String,
    width: i64,
    height: i64,
}

#[derive(Deserialize)]
struct ImagesReq {
    images: Vec<ImageIn>,
}

async fn put_images(
    State(st): State<SentState>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Result<Json<ImagesReq>, JsonRejection>,
) -> Response {
    if let Err(r) = owner_gate(&v, &headers) {
        return r;
    }
    let req = match body {
        Ok(Json(r)) => r,
        Err(JsonRejection::BytesRejection(e)) => return err(e.status(), e.body_text()),
        Err(e) => return err(StatusCode::BAD_REQUEST, e.body_text()),
    };
    if req.images.len() > MAX_IMAGES {
        return err(StatusCode::BAD_REQUEST, format!("at most {MAX_IMAGES} images a request"));
    }
    let mut decoded = Vec::new();
    for i in req.images {
        let png = match base64::engine::general_purpose::STANDARD.decode(i.png.trim()) {
            Ok(b) => b,
            Err(_) => return err(StatusCode::BAD_REQUEST, "png: not base64"),
        };
        if png.len() > MAX_PNG_BYTES {
            return err(StatusCode::PAYLOAD_TOO_LARGE, format!("png: at most {} MB", MAX_PNG_BYTES / 1024 / 1024));
        }
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return err(StatusCode::BAD_REQUEST, "png: not a PNG");
        }
        if !(1..=MAX_DIM).contains(&i.width) || !(1..=MAX_DIM).contains(&i.height) {
            return err(StatusCode::BAD_REQUEST, "width/height: 1..8000");
        }
        decoded.push((i.block_id, i.source_hash, png, i.width, i.height));
    }
    let now = crate::auth::now();
    let out = with_store(&st.store, v.scope, move |s| -> taisce_store::Result<(usize, usize)> {
        let spec = gather(s, id)?;
        // each block's doc and its fences' current hashes
        let mut current: HashMap<Uuid, (Uuid, HashSet<String>)> = HashMap::new();
        for ch in &spec.chapters {
            for b in &ch.blocks {
                let hashes = epub::diagram_fences(&b.markdown).iter().map(|(l, c)| epub::source_hash(l, c)).collect::<HashSet<_>>();
                if !hashes.is_empty() {
                    current.insert(b.id, (ch.doc_id, hashes));
                }
            }
        }
        let (mut stored, mut ignored) = (0, 0);
        for (block, hash, png, w, h) in decoded {
            match current.get(&block) {
                Some((doc, hashes)) if hashes.contains(&hash) => {
                    s.epub_image_put(*doc, block, &hash, &png, w, h, now)?;
                    stored += 1;
                }
                _ => ignored += 1,
            }
        }
        Ok((stored, ignored))
    })
    .await;
    match out {
        Ok((stored, ignored)) => Json(json!({"stored": stored, "ignored": ignored})).into_response(),
        Err(e) => fail(e),
    }
}

async fn list_sent(State(st): State<SentState>, v: Viewer, headers: HeaderMap) -> Response {
    if let Err(r) = owner_gate(&v, &headers) {
        return r;
    }
    let user = v.user;
    let out = with_store(&st.store, v.scope, move |s| -> taisce_store::Result<Vec<(SentBook, Option<Value>)>> {
        let books = sent_books(s, user)?;
        let mut out = Vec::new();
        for b in books {
            let progress = match user {
                Some(u) => s.kosync_get(u, &kosync_document_digest(&b.feed_title, FEED_AUTHOR))?.map(|p| crate::kosync::progress_json(&p)),
                None => None,
            };
            out.push((b, progress));
        }
        Ok(out)
    })
    .await;
    let books = match out {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let mut docs = Vec::new();
    for (b, progress) in books {
        let size = match epub::CACHE.get_or_render(&b.spec) {
            Ok(bytes) => bytes.len(),
            Err(e) => return render_failed(e),
        };
        docs.push(json!({
            "id": b.spec.id,
            "title": b.spec.title,
            "feed_title": b.feed_title,
            "filename": b.filename,
            "workspace": b.workspace,
            "chapters": b.spec.chapters.len(),
            "updated_at": b.spec.modified,
            "size": size,
            "progress": progress,
        }));
    }
    Json(json!({"docs": docs})).into_response()
}

pub fn router(state: SentState) -> Router {
    Router::new()
        .route("/api/docs/{id}/epub", get(download))
        .route("/api/docs/{id}/ereader", get(get_info).post(set_sent))
        .route(
            "/api/docs/{id}/epub-images",
            axum::routing::post(put_images).layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .route("/api/ereader/sent", get(list_sent))
        // the person's own surface only (ADR 0004): a connector never sends
        .layer(axum::middleware::from_fn(crate::viewer::refuse_connector_writes))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retag_adds_and_removes_the_tag_in_block_list_form() {
        assert_eq!(retag(None, true), Some(Some("---\ntags:\n  - ereader\n---".into())));
        assert_eq!(retag(None, false), None);
        let fm = "---\ntitle: x\ntags:\n  - proposal\n---";
        let added = retag(Some(fm), true).unwrap().unwrap();
        assert_eq!(added, "---\ntitle: x\ntags:\n  - proposal\n  - ereader\n---");
        assert_eq!(retag(Some(&added), true), None, "already there");
        assert_eq!(retag(Some(&added), false), Some(Some(fm.into())));
        // the flow form becomes a block list the store can read
        assert_eq!(retag(Some("---\ntags: [a, \"b\"]\n---"), true), Some(Some("---\ntags:\n  - a\n  - b\n  - ereader\n---".into())));
        // no tags line: one is added
        assert_eq!(retag(Some("---\nstatus: draft\n---"), true), Some(Some("---\nstatus: draft\ntags:\n  - ereader\n---".into())));
        // the last tag goes with its `tags:` line; an empty frontmatter goes whole
        assert_eq!(retag(Some("---\nstatus: draft\ntags:\n  - ereader\n---"), false), Some(Some("---\nstatus: draft\n---".into())));
        assert_eq!(retag(Some("---\ntags:\n  - ereader\n---"), false), Some(None));
        for got in [retag(None, true).unwrap().unwrap(), added] {
            assert!(taisce_store_tags(&got).contains(&"ereader".to_string()), "{got}");
        }
    }

    /// The store's own extraction rule, as the tests see it.
    fn taisce_store_tags(content: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut on = false;
        for l in content.lines() {
            if on {
                if let Some(i) = l.trim_start().strip_prefix("- ") {
                    out.push(i.trim().to_lowercase());
                    continue;
                }
                on = false;
            }
            if l.trim_end() == "tags:" {
                on = true;
            }
        }
        out
    }

    #[test]
    fn names_are_unique_against_books_and_each_other_and_stable() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let docs = vec![
            (b, "Plan".to_string(), Some("Family".to_string())),
            (a, "Plan".to_string(), Some("Work".to_string())),
            (c, "Notes".to_string(), None),
        ];
        // a book already derives "Taisce - Notes.epub"
        let taken = HashSet::from([derived_book_filename("notes", "Taisce").to_lowercase()]);
        let names = assign_names(&docs, &taken);
        assert_eq!(names[&a], "Plan", "the older doc keeps the plain name");
        assert_eq!(names[&b], "Plan (Family)");
        assert_eq!(names[&c], "Notes (00000003)");
        assert_eq!(assign_names(&docs, &taken), names, "deterministic");
        let all: HashSet<String> = names.values().map(|t| derived_book_filename(t, FEED_AUTHOR).to_lowercase()).collect();
        assert_eq!(all.len(), 3);
        // a long title keeps its suffix inside the 100-byte rule
        let long = "x".repeat(300);
        let names = assign_names(&[(a, long.clone(), None), (b, long, Some("Family".into()))], &HashSet::new());
        assert!(names[&b].ends_with("(Family)"), "{}", names[&b]);
        assert_ne!(derived_book_filename(&names[&a], FEED_AUTHOR), derived_book_filename(&names[&b], FEED_AUTHOR));
        assert!(derived_book_filename(&names[&b], FEED_AUTHOR).len() <= 105);
    }

    #[test]
    fn ledger_times_become_opf_instants() {
        assert_eq!(ledger_time("2026-10-23T09:30:12.345Z"), "2026-10-23T09:30:12Z");
        assert_eq!(ledger_time("2026-10-23 09:30:12"), "2026-10-23T09:30:12Z");
    }
}
