//! JSON API for the human UI (M5). This surface acts as the HUMAN principal
//! (tom) — resolve here is Tom clicking accept/decline. Agents use MCP.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use grimoire_store::{BlockStore, DocStatus, OpInput, ReviewDecision, SqliteStore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<SqliteStore>>,
    pub human: Uuid,
    /// The freeze: content writes against a live doc are refused (P2.3).
    pub hot: crate::hot::HotState,
    /// Federation runtime: focus heartbeats (adaptive pull) + owner nudges.
    pub runtime: crate::fed::Runtime,
    /// The live database file — backups live beside it.
    pub db_path: std::path::PathBuf,
    /// Federation identity (node id), None when federation is disabled.
    pub node_id: Option<String>,
    /// Block embeddings for ask-the-vault; None if the model failed to load
    /// (retrieval falls back to keywords).
    pub embedder: Option<Arc<crate::embed::Embedder>>,
    /// Idempotency cache for `request_id` on propose routes, shared with MCP.
    pub dedupe: crate::mcp::DedupeCache,
}

/// Optional on every HTTP write: attribute the write to this Agent principal
/// (find-or-create by name, the `identify` rule) instead of the human. Lets
/// an HTTP client such as workbox get the same provenance an MCP agent gets —
/// its writes go through the gate as an agent's. Absent → the human.
pub const PRINCIPAL_HEADER: &str = "x-grimoire-principal";

fn resolve_principal(st: &ApiState, headers: &HeaderMap, s: &mut SqliteStore) -> Result<Uuid, String> {
    match headers.get(PRINCIPAL_HEADER) {
        None => Ok(st.human),
        Some(v) => {
            let name = v
                .to_str()
                .map_err(|_| format!("{PRINCIPAL_HEADER}: not valid UTF-8"))?;
            crate::mcp::agent_principal_by_name(s, name).map_err(|m| format!("{PRINCIPAL_HEADER}: {m}"))
        }
    }
}

/// UI heartbeat: this doc is open. For a mirror, its share joins the fast
/// (5s) pull tier for the focus window; owned docs are a harmless no-op.
async fn focus_doc(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let share = {
        let s = st
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.get_mirror(id).ok().flatten().map(|m| m.share_id)
    };
    if let Some(share) = share {
        st.runtime.focus_share(share);
    }
    Json(json!({"ok": true, "focused_share": share}))
}

#[derive(Deserialize)]
struct EventsQuery {
    since: Option<u64>,
}

/// Nudges received from owners (live_started / doc_added / doc_changed),
/// cursor-paginated: pass the previous `next`.
async fn events(State(st): State<ApiState>, Query(q): Query<EventsQuery>) -> Json<Value> {
    let (next, events) = st.runtime.events_since(q.since.unwrap_or(0));
    Json(json!({"next": next, "events": events}))
}

/// Mirror docs are the owner's: no local rename/delete/status/policy — and
/// no move except of the share root, which the grantee may file where they
/// like. Returns the user-facing refusal.
fn refuse_if_mirror(s: &SqliteStore, id: Uuid, what: &str) -> Option<String> {
    match s.get_mirror(id) {
        Ok(Some(_)) => Some(format!(
            "this doc is shared with you by its owner — {what} is the owner's call"
        )),
        _ => None,
    }
}

async fn docs(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let canvases: std::collections::HashSet<String> =
        s.canvas_doc_ids().unwrap_or_default().into_iter().collect();
    let tended: std::collections::HashSet<String> = s
        .list_gardeners()
        .unwrap_or_default()
        .into_iter()
        .filter(|g| g.enabled)
        .filter_map(|g| g.scope_doc.map(|d| d.to_string()))
        .collect();
    // federation decorations: mirror docs ("shared with me") and the roots
    // of active shares ("you are sharing this")
    let mirror_rows = s.list_mirrors().unwrap_or_default();
    let mirrors: std::collections::HashMap<String, String> = mirror_rows
        .iter()
        .map(|m| (m.doc_id.to_string(), m.permission.as_str().to_string()))
        .collect();
    // hub (slice 1): mirrors that come from a hub, relayed docs' true owners,
    // and my subtrees published to a hub (every doc under a published root)
    let contacts = s.list_contacts().unwrap_or_default();
    let hub_contacts: std::collections::HashMap<Uuid, String> = contacts
        .iter()
        .filter(|c| c.is_hub && !c.revoked)
        .map(|c| (c.id, c.petname.clone()))
        .collect();
    let from_hub: std::collections::HashSet<String> = mirror_rows
        .iter()
        .filter(|m| hub_contacts.contains_key(&m.owner))
        .map(|m| m.doc_id.to_string())
        .collect();
    let origin_names: std::collections::HashMap<String, String> = mirror_rows
        .iter()
        .filter_map(|m| m.origin_owner_name.clone().map(|n| (m.doc_id.to_string(), n)))
        .collect();
    let all_shares = s.list_shares().unwrap_or_default();
    let published_roots: std::collections::HashMap<Uuid, String> = all_shares
        .iter()
        .filter(|sh| sh.state == grimoire_store::ShareState::Active)
        .filter_map(|sh| sh.contact.and_then(|c| hub_contacts.get(&c)).map(|n| (sh.root_doc, n.clone())))
        .collect();
    // mirrors tended on the owner's side: shown as tended locally, and the
    // tend panel refuses to configure them (avoids two-sided agent edits)
    let owner_tended: std::collections::HashSet<String> = mirror_rows
        .iter()
        .filter(|m| m.owner_tended)
        .map(|m| m.doc_id.to_string())
        .collect();
    let share_roots: std::collections::HashSet<String> = all_shares
        .iter()
        .filter(|sh| sh.state != grimoire_store::ShareState::Revoked)
        .map(|sh| sh.root_doc.to_string())
        .collect();
    match s.list_docs() {
        Ok(d) => {
            let parent_of: std::collections::HashMap<Uuid, Option<Uuid>> =
                d.iter().map(|x| (x.id, x.parent_id)).collect();
            // the hub a doc is published to: the nearest published ancestor (or itself)
            let published_to = |mut cur: Uuid| -> Option<&String> {
                if published_roots.is_empty() {
                    return None;
                }
                loop {
                    if let Some(n) = published_roots.get(&cur) {
                        return Some(n);
                    }
                    match parent_of.get(&cur).copied().flatten() {
                        Some(p) => cur = p,
                        None => return None,
                    }
                }
            };
            Json(json!(
                d.iter()
                    .map(|doc| {
                        let id = doc.id.to_string();
                        let mut v = json!(doc);
                        v["is_canvas"] = json!(canvases.contains(&id));
                        let owner_t = owner_tended.contains(&id);
                        v["is_tended"] = json!(tended.contains(&id) || owner_t);
                        v["owner_tended"] = json!(owner_t);
                        if let Some(perm) = mirrors.get(&id) {
                            v["mirror_permission"] = json!(perm);
                        }
                        v["is_shared"] = json!(share_roots.contains(&id));
                        if from_hub.contains(&id) {
                            v["from_hub"] = json!(true);
                        }
                        if let Some(n) = origin_names.get(&id) {
                            v["origin_owner_name"] = json!(n);
                        }
                        if let Some(n) = published_to(doc.id) {
                            v["published_to"] = json!(n);
                        }
                        v
                    })
                    .collect::<Vec<_>>()
            ))
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Hub mode (slice 1), for the hub's own UI and the CLI on the box: whether
/// this Grimoire is a hub, its name and root, active members and pending
/// requests. Read-only; approving lives under /admin/hub/*.
async fn hub_info(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(hub) = crate::fed::hub::config(&s) else {
        return Json(json!({"enabled": false}));
    };
    let members = crate::fed::hub::members(&s).unwrap_or_default();
    let (active, pending): (Vec<_>, Vec<_>) = members
        .into_iter()
        .filter(|m| m.membership != "ejected")
        .partition(|m| m.membership == "active");
    Json(json!({
        "enabled": true,
        "name": hub.name,
        "root_doc": hub.root_doc,
        "members": active,
        "pending": pending,
    }))
}

/// Everything the doc view needs to render federation state for one doc:
/// its mirror origin (if it is shared WITH us), the shares exposing it (if
/// we are sharing it), and our pending upstream proposals against it.
async fn doc_federation(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let contacts = s.list_contacts().unwrap_or_default();
    let petname_of = |cid: Uuid| {
        contacts
            .iter()
            .find(|c| c.id == cid)
            .map(|c| c.petname.clone())
            .unwrap_or_else(|| "?".into())
    };
    let is_hub = |cid: Uuid| contacts.iter().any(|c| c.id == cid && c.is_hub);
    // hub slice 2: this doc (or an ancestor) was handed to the mirror's owner by me
    let transferred_roots: Vec<Uuid> = s
        .list_doc_transfers()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.direction == grimoire_store::TransferDirection::Out && t.state == "done")
        .map(|t| t.root_doc)
        .collect();
    let transferred_from_me = |mut cur: Uuid| -> bool {
        if transferred_roots.is_empty() {
            return false;
        }
        loop {
            if transferred_roots.contains(&cur) {
                return true;
            }
            match s.get_doc(cur).ok().and_then(|d| d.parent_id) {
                Some(p) => cur = p,
                None => return false,
            }
        }
    };
    let mirror = s.get_mirror(id).ok().flatten().map(|m| {
        json!({
            "owner": m.owner,
            "owner_petname": petname_of(m.owner),
            "permission": m.permission,
            "synced_epoch": m.synced_epoch,
            "owner_tended": m.owner_tended,
            // hub relay (slice 1): the share comes from a hub; a relayed doc
            // names its true owner (slice 2: edits reach them through the hub)
            "from_hub": is_hub(m.owner),
            "origin_owner": m.origin_owner,
            "origin_owner_name": m.origin_owner_name,
            "transferred_from_me": transferred_from_me(id),
        })
    });
    let shares: Vec<Value> = s
        .shares_containing(id)
        .unwrap_or_default()
        .into_iter()
        .map(|sh| {
            json!({
                "id": sh.id,
                "root_doc": sh.root_doc,
                "permission": sh.permission,
                "state": sh.state,
                "petname": sh.contact.map(&petname_of),
                "trust": sh.trust,
                "to_hub": sh.contact.map(is_hub).unwrap_or(false),
            })
        })
        .collect();
    let outbound: Vec<Value> = s
        .list_outbound_proposals(false)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.doc_id == id)
        .map(|p| json!(p))
        .collect();
    Json(json!({"mirror": mirror, "shares": shares, "outbound": outbound}))
}

async fn doc(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.read_doc(id) {
        Ok(t) => Json(json!(t)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn backlinks(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.backlinks(id) {
        Ok(b) => Json(json!(b)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Decorate review items for rendering: doc titles, proposer names, and the
/// live content of the target block (what a red would replace).
pub(crate) fn decorate_review_items(s: &SqliteStore, q: Vec<grimoire_store::ReviewItem>) -> Vec<Value> {
    q.into_iter()
        .map(|item| {
            let doc_title = s
                .get_doc(item.annotation.doc_id)
                .map(|d| d.title)
                .unwrap_or_default();
            let proposer = s
                .get_principal(item.op.principal)
                .map(|p| p.display_name)
                .unwrap_or_default();
            let current = item
                .op
                .kind
                .target_block()
                .and_then(|t| s.read_block(t).ok())
                .map(|b| b.content);
            json!({
                "item": item,
                "doc_title": doc_title,
                "proposer": proposer,
                "current_content": current,
            })
        })
        .collect()
}

/// Open review items for ONE doc — the in-editor review rail's data source.
/// Same item shape as /api/queue.
async fn doc_review(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.review_queue(Some(id)) {
        Ok(q) => Json(json!(decorate_review_items(&s, q))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct ActivityQuery {
    limit: Option<usize>,
}

/// The owner's notification feed: content edits applied directly by remote
/// principals (maintainer-tier shares). Newest first.
async fn activity(State(st): State<ApiState>, Query(q): Query<ActivityQuery>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.recent_remote_ops(q.limit.unwrap_or(20).min(200)) {
        Ok(items) => Json(json!(items)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn queue(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.review_queue(None) {
        Ok(q) => Json(json!(decorate_review_items(&s, q))),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct ResolveReq {
    annotation_id: Uuid,
    decision: String,
}

async fn resolve(State(st): State<ApiState>, Json(req): Json<ResolveReq>) -> Json<Value> {
    let decision = match req.decision.as_str() {
        "accept" => ReviewDecision::Accept,
        "decline" => ReviewDecision::Decline,
        other => return Json(json!({"error": format!("bad decision: {other}")})),
    };
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(doc) = crate::hot::annotation_doc(&s, req.annotation_id)
        && let Err(m) = st.hot.assert_cold(doc)
    {
        return Json(json!({"error": m}));
    }
    match s.resolve(req.annotation_id, st.human, decision) {
        Ok(receipt) => Json(json!({"ok": true, "receipt": receipt})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct ResolveBulkReq {
    annotation_ids: Vec<Uuid>,
    decision: String,
}

/// Bulk resolve as the human: one request, per-item outcomes. Errors on
/// individual items (already resolved, proposer==approver) don't stop the rest.
async fn resolve_bulk(State(st): State<ApiState>, Json(req): Json<ResolveBulkReq>) -> Json<Value> {
    let decision = match req.decision.as_str() {
        "accept" => ReviewDecision::Accept,
        "decline" => ReviewDecision::Decline,
        other => return Json(json!({"error": format!("bad decision: {other}")})),
    };
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut done, mut failed) = (0usize, Vec::new());
    for id in req.annotation_ids {
        if let Some(doc) = crate::hot::annotation_doc(&s, id)
            && let Err(m) = st.hot.assert_cold(doc)
        {
            failed.push(json!({"id": id, "error": m}));
            continue;
        }
        match s.resolve(id, st.human, decision) {
            Ok(_) => done += 1,
            Err(e) => failed.push(json!({"id": id, "error": e.to_string()})),
        }
    }
    Json(json!({"resolved": done, "failed": failed}))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(State(st): State<ApiState>, Query(p): Query<SearchQuery>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.search_blocks(&p.q, 20) {
        Ok(h) => Json(json!(h)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn tags(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_tags() {
        Ok(t) => Json(json!(t)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn runs(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_runs(20) {
        Ok(r) => Json(json!(r)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct ProposeReq {
    doc_id: Uuid,
    base_epoch: i64,
    ops: Vec<OpInput>,
    /// Optional idempotency key (any UUID): a retry with the same request_id
    /// returns the first outcome instead of double-applying (per principal).
    #[serde(default)]
    request_id: Option<Uuid>,
}

/// Writes: propose as the human (or the `X-Grimoire-Principal` agent) —
/// current-epoch ops green and apply directly, stale ones are scored per op.
async fn propose(State(st): State<ApiState>, headers: HeaderMap, Json(req): Json<ProposeReq>) -> Json<Value> {
    if let Err(m) = st.hot.assert_cold(req.doc_id) {
        return Json(json!({"error": m}));
    }
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let principal = match resolve_principal(&st, &headers, &mut s) {
        Ok(p) => p,
        Err(m) => return Json(json!({"error": m})),
    };
    if let Some(rid) = req.request_id
        && let Some(prev) = crate::mcp::dedupe_get(&st.dedupe, principal, rid)
    {
        return Json(prev);
    }
    match s.propose(req.doc_id, req.base_epoch, principal, req.ops) {
        Ok(out) => {
            let v = json!(out);
            if let Some(rid) = req.request_id {
                crate::mcp::dedupe_put(&st.dedupe, principal, rid, v.clone());
            }
            Json(v)
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct ProposeMarkdownReq {
    doc_id: Uuid,
    base_epoch: i64,
    /// The doc's complete new markdown (frontmatter included).
    markdown: String,
    #[serde(default)]
    request_id: Option<Uuid>,
}

/// HTTP twin of the MCP `propose_markdown` tool: diff the whole doc's new
/// markdown against the current blocks and propose the minimal ops. Whole-doc
/// semantics make a stale base an error here (the diff would re-apply the
/// caller's stale view over others' edits) — `stale_base` carries the missed
/// ops so the caller can re-read, re-apply and re-send.
async fn propose_markdown(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<ProposeMarkdownReq>,
) -> Json<Value> {
    if let Err(m) = st.hot.assert_cold(req.doc_id) {
        return Json(json!({"error": m}));
    }
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let principal = match resolve_principal(&st, &headers, &mut s) {
        Ok(p) => p,
        Err(m) => return Json(json!({"error": m})),
    };
    if let Some(rid) = req.request_id
        && let Some(prev) = crate::mcp::dedupe_get(&st.dedupe, principal, rid)
    {
        return Json(prev);
    }
    let tree = match s.read_doc(req.doc_id) {
        Ok(t) => t,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    if req.base_epoch != tree.doc.current_epoch {
        let missed = s.ops_since(req.doc_id, req.base_epoch).unwrap_or_default();
        return Json(json!({
            "error": "stale_base",
            "base_epoch": req.base_epoch,
            "current_epoch": tree.doc.current_epoch,
            "missed_ops": missed,
            "recover": "re-read the doc, re-apply your edit to the fresh markdown, re-send with the current epoch",
        }));
    }
    let ops = grimoire_store::mddiff::markdown_to_ops(&tree.roots, &req.markdown);
    if ops.is_empty() {
        return Json(json!({
            "doc_id": req.doc_id, "epoch": tree.doc.current_epoch, "verdicts": [], "note": "no changes"
        }));
    }
    match s.propose(req.doc_id, req.base_epoch, principal, ops) {
        Ok(out) => {
            let v = json!(out);
            if let Some(rid) = req.request_id {
                crate::mcp::dedupe_put(&st.dedupe, principal, rid, v.clone());
            }
            Json(v)
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct CreateDocReq {
    title: String,
    parent_doc_id: Option<Uuid>,
}

async fn create_doc(State(st): State<ApiState>, headers: HeaderMap, Json(req): Json<CreateDocReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let principal = match resolve_principal(&st, &headers, &mut s) {
        Ok(p) => p,
        Err(m) => return Json(json!({"error": m})),
    };
    match s.create_doc(&req.title, req.parent_doc_id, principal) {
        Ok(d) => Json(json!(d)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn principals(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_principals() {
        Ok(p) => Json(json!(p)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Per-doc op history, newest first — the provenance panel (5.4).
async fn history(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // newest first, capped in SQL (was: whole ledger, reversed, truncated)
    match s.ops_for_doc_limited(id, 100) {
        Ok(ops) => {
            let rows: Vec<Value> = ops
                .into_iter()
                .map(|op| {
                    let principal = s
                        .get_principal(op.principal)
                        .map(|p| (p.display_name, p.kind.as_str().to_string()))
                        .unwrap_or_default();
                    json!({
                        "op": op,
                        "principal_name": principal.0,
                        "principal_kind": principal.1,
                    })
                })
                .collect();
            Json(json!(rows))
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct CommentReq {
    block_id: Uuid,
    text: String,
    reply_to: Option<Uuid>,
}

async fn add_comment(State(st): State<ApiState>, headers: HeaderMap, Json(req): Json<CommentReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let principal = match resolve_principal(&st, &headers, &mut s) {
        Ok(p) => p,
        Err(m) => return Json(json!({"error": m})),
    };
    match s.add_comment(req.block_id, principal, &req.text, req.reply_to) {
        Ok(c) => Json(json!(c)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Graph view data (5.10): nodes = docs (tinted by tending principal —
/// the principal of the doc's last applied op), links = resolved wikilinks.
async fn graph(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let docs = match s.list_docs() {
        Ok(d) => d,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    let mut tenders: std::collections::HashMap<String, String> = Default::default();
    let mut names: std::collections::HashMap<String, String> = Default::default();
    if let Ok(principals) = s.list_principals() {
        for p in principals {
            names.insert(p.id.to_string(), p.display_name);
        }
    }
    // last applied op per doc = who tends it
    if let Ok(rows) = s.raw_tending() {
        for (doc, principal) in rows {
            if let Some(name) = names.get(&principal) {
                tenders.insert(doc, name.clone());
            }
        }
    }
    let links = s.raw_links().unwrap_or_default();
    let tags = s.raw_doc_tags().unwrap_or_default();
    let nodes: Vec<Value> = docs
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "title": d.title,
                "tender": tenders.get(&d.id.to_string()),
                "tags": tags.get(&d.id.to_string()).cloned().unwrap_or_default(),
            })
        })
        .collect();
    let links: Vec<Value> = links
        .into_iter()
        .map(|(a, b)| json!({"source": a, "target": b}))
        .collect();
    Json(json!({"nodes": nodes, "links": links}))
}

#[derive(Deserialize)]
struct D2Req {
    source: String,
}

/// Render D2 to SVG by shelling to the d2 binary (5.7). Text-to-diagram
/// only — the diagram block's content stays the source of truth.
/// Where `d2` lives, probed once (each probe is a process spawn — never on
/// the runtime, never per request). Only a hit is cached: install d2 while
/// the daemon runs and the next render finds it.
async fn d2_binary() -> Option<&'static str> {
    static FOUND: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    if let Some(b) = FOUND.get() {
        return Some(b);
    }
    let probed = tokio::task::spawn_blocking(|| {
        ["d2", "/opt/homebrew/bin/d2", "/usr/local/bin/d2"]
            .iter()
            .find(|b| {
                std::process::Command::new(b)
                    .arg("--version")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            })
            .copied()
    })
    .await
    .ok()
    .flatten()?;
    Some(*FOUND.get_or_init(|| probed))
}

async fn render_d2(Json(req): Json<D2Req>) -> Json<Value> {
    let Some(bin) = d2_binary().await else {
        return Json(json!({"error": "d2 binary not installed (brew install d2)"}));
    };
    let out = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut child = std::process::Command::new(bin)
            .args(["--theme", "200", "-", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(req.source.as_bytes())?;
        child.wait_with_output()
    })
    .await;
    match out {
        Ok(Ok(o)) if o.status.success() => Json(json!({"svg": String::from_utf8_lossy(&o.stdout)})),
        Ok(Ok(o)) => Json(
            json!({"error": String::from_utf8_lossy(&o.stderr).chars().take(400).collect::<String>()}),
        ),
        _ => Json(json!({"error": "d2 render failed"})),
    }
}

#[derive(Deserialize)]
struct StatusReq {
    status: Option<String>,
}

async fn set_status(
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(req): Json<StatusReq>,
) -> Json<Value> {
    let status = match req.status.as_deref() {
        None => None,
        Some(v) => match DocStatus::parse(v) {
            Some(v) => Some(v),
            None => return Json(json!({"error": format!("bad status: {v}")})),
        },
    };
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(e) = refuse_if_mirror(&s, id, "status") {
        return Json(json!({"error": e}));
    }
    match s.set_doc_status(id, status) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Agent audit flags: comments by agent principals, queue-adjacent surface.
async fn flags(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.agent_flags() {
        Ok(rows) => Json(json!(
            rows.into_iter()
                .map(|(block, doc_title, author, target_content)| json!({
                    "block": block,
                    "doc_title": doc_title,
                    "author": author,
                    "target_content": target_content,
                }))
                .collect::<Vec<_>>()
        )),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct DismissReq {
    comment_id: Uuid,
}

/// Dismiss a flag: delete the comment block through the gate as the human.
async fn dismiss_flag(State(st): State<ApiState>, Json(req): Json<DismissReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let block = match s.read_block(req.comment_id) {
        Ok(b) if b.block_type == grimoire_store::BlockType::Comment => b,
        Ok(_) => return Json(json!({"error": "not a comment block"})),
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    let epoch = match s.get_doc(block.doc_id) {
        Ok(d) => d.current_epoch,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    let op = OpInput {
        kind: grimoire_store::OpKind::Delete {
            target: req.comment_id,
        },
        source_refs: vec!["flag:dismissed".into()],
    };
    match s.propose(block.doc_id, epoch, st.human, vec![op]) {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Tendings covering a doc: gardeners scoped to it or to any ancestor.
/// The opt-in surface — configure agents where the docs live.
async fn tendings(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // ancestor chain of this doc, self first
    let mut chain = vec![id];
    let mut cur = id;
    while let Ok(d) = s.get_doc(cur) {
        match d.parent_id {
            Some(p) => {
                chain.push(p);
                cur = p;
            }
            None => break,
        }
    }
    match s.list_gardeners() {
        Ok(gs) => {
            let rows: Vec<Value> = gs
                .into_iter()
                .filter(|g| g.scope_doc.map(|sd| chain.contains(&sd)).unwrap_or(false))
                .map(|g| {
                    let scope_title = g
                        .scope_doc
                        .and_then(|sd| s.get_doc(sd).ok())
                        .map(|d| d.title)
                        .unwrap_or_default();
                    let inherited = g.scope_doc != Some(id);
                    let mut v = json!(g);
                    v["scope_title"] = json!(scope_title);
                    v["inherited"] = json!(inherited);
                    v
                })
                .collect();
            Json(json!(rows))
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Data change stamp: the app polls this and live-refreshes whatever view is
/// open when it moves — gardener writes, MCP proposals from other sessions,
/// queue resolutions all appear without a reload.
async fn stamp(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.change_stamp() {
        // version/build ride along so the app needs one poll, not two
        Ok(v) => Json(json!({"stamp": v, "version": env!("CARGO_PKG_VERSION"), "build": build_stamp(), "git": crate::GIT_SHA})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// UI build stamp. The app polls this and reloads itself when it changes —
/// no manual ⌘R after a deploy. With the embedded frontend the stamp is a
/// hash of the bundled index.html (changes exactly when the UI does); under
/// the GRIMOIRE_UI_DIST dev override it is that file's mtime. Never a
/// machine-specific path.
async fn buildinfo() -> Json<Value> {
    Json(json!({"build": build_stamp(), "version": env!("CARGO_PKG_VERSION"), "git": crate::GIT_SHA}))
}

fn build_stamp() -> u64 {
    match std::env::var("GRIMOIRE_UI_DIST") {
        Ok(dist) => std::fs::metadata(format!("{dist}/index.html"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
        Err(_) => crate::ui_build_stamp(),
    }
}

/// Last `n` lines of a file, reading only its tail (the daily log can be a
/// few MB; never slurp it whole for 200 lines).
fn tail_lines(path: &std::path::Path, n: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const WINDOW: u64 = 256 * 1024;
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(WINDOW);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if f.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    // the window can start mid-codepoint; never fail the whole tail on it
    let buf = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = buf.lines().collect();
    let skip = lines.len().saturating_sub(n);
    // a window cut mid-line leaves a partial first line; drop it when we skipped
    let from = if start > 0 && skip == 0 { 1.min(lines.len()) } else { skip };
    lines[from..].join("\n")
}

/// What to paste into a bug report: version, log location and the last 200
/// log lines. The UI adds node id + fingerprint from /api/profile.
async fn diagnostics(State(st): State<ApiState>) -> Json<Value> {
    let path = crate::log_path();
    let tail = path.as_deref().map(|p| tail_lines(p, 200)).unwrap_or_default();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "node_id": st.node_id,
        "fingerprint": st.node_id.as_deref().map(crate::identity::fingerprint_of),
        "embedded_blocks": st.embedder.as_ref().map(|e| e.indexed()),
        "log_path": path.map(|p| p.to_string_lossy().to_string()),
        "log_tail": tail,
    }))
}

/// Gardeners shell out to Claude Code; a fresh Mac may not have it. The UI
/// asks before offering to create one, so the first run never fails with a
/// bare "spawn claude: No such file or directory".
async fn gardeners_preflight() -> Json<Value> {
    let path = crate::garden::claude_bin();
    Json(json!({
        "claude": path.is_some(),
        "path": path.map(|p| p.to_string_lossy().to_string()),
    }))
}

#[derive(Deserialize)]
struct MoveDocReq {
    parent_id: Option<Uuid>,
    sort_key: Option<String>,
}

async fn move_doc(
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(req): Json<MoveDocReq>,
) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(e) = refuse_move(&s, id, req.parent_id) {
        return Json(json!({"error": e}));
    }
    match s.move_doc(id, req.parent_id, req.sort_key.as_deref()) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Tree-move rules at the boundary between my docs and mirrors (ADR 0002):
/// - a mirror may move only if it is a share ROOT (its parent is not part of
///   the same share) — the grantee files it, the owner shapes the inside;
/// - nothing lands INSIDE a mirror subtree (that tree is the owner's; a pull
///   would strand or delete it);
/// - no doc whose subtree contains a mirror moves INTO one of my shares —
///   the re-share guard at create time would otherwise be bypassed and the
///   pull would ship someone else's content onward.
fn refuse_move(s: &SqliteStore, id: Uuid, new_parent: Option<Uuid>) -> Option<String> {
    let mirrors: std::collections::HashMap<Uuid, grimoire_store::Mirror> = s
        .list_mirrors()
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.doc_id, m))
        .collect();
    if mirrors.is_empty() {
        return None;
    }
    if let Some(m) = mirrors.get(&id) {
        let parent_in_same_share = s
            .get_doc(id)
            .ok()
            .and_then(|d| d.parent_id)
            .and_then(|p| mirrors.get(&p))
            .is_some_and(|pm| pm.share_id == m.share_id);
        if parent_in_same_share {
            return Some("this doc sits inside a shared tree — only its owner can move it".into());
        }
    }
    if let Some(p) = new_parent
        && mirrors.contains_key(&p)
    {
        return Some("cannot file a doc inside a tree shared with you — that tree is the owner's".into());
    }
    // does the moved subtree contain a mirror, and is the destination inside one of my shares?
    let subtree_has_mirror = {
        let mut stack = vec![id];
        let docs = s.list_docs().unwrap_or_default();
        let mut found = false;
        while let Some(d) = stack.pop() {
            if mirrors.contains_key(&d) {
                found = true;
                break;
            }
            stack.extend(docs.iter().filter(|c| c.parent_id == Some(d)).map(|c| c.id));
        }
        found
    };
    if subtree_has_mirror
        && let Some(p) = new_parent
        && !s.shares_containing(p).unwrap_or_default().is_empty()
    {
        return Some(
            "cannot move a doc shared TO you into a tree you share — only the owner can share it onward"
                .into(),
        );
    }
    None
}

#[derive(Deserialize)]
struct RenameReq {
    title: String,
}

/// Rewrite [[Old Title]] / [[Path/Old|alias]] / [[Old#anchor]] link forms.
fn rewrite_links(content: &str, old: &str, new: &str) -> String {
    let mut out = content.to_string();
    for (from, to) in [
        (format!("[[{old}]]"), format!("[[{new}]]")),
        (format!("[[{old}|"), format!("[[{new}|")),
        (format!("[[{old}#"), format!("[[{new}#")),
        (format!("/{old}]]"), format!("/{new}]]")),
        (format!("/{old}|"), format!("/{new}|")),
        (format!("/{old}#"), format!("/{new}#")),
    ] {
        out = out.replace(&from, &to);
    }
    out
}

async fn rename_doc(
    State(st): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(req): Json<RenameReq>,
) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let old_title = match s.get_doc(id) {
        Ok(d) => d.title,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    if let Some(e) = refuse_if_mirror(&s, id, "renaming") {
        return Json(json!({"error": e}));
    }
    if let Err(e) = s.rename_doc(id, &req.title) {
        return Json(json!({"error": e.to_string()}));
    }
    // no more link-rot: rewrite every inbound [[wikilink]] through the gate
    let new_title = req.title.trim();
    let mut rewritten = 0usize;
    if old_title != new_title {
        let linkers = s.linking_blocks(&old_title).unwrap_or_default();
        // group by doc so each doc gets one epoch
        let mut by_doc: std::collections::HashMap<Uuid, Vec<(Uuid, String)>> = Default::default();
        for (block, doc, content) in linkers {
            by_doc.entry(doc).or_default().push((block, content));
        }
        let mut deferred = 0usize;
        for (doc, blocks) in by_doc {
            let Ok(d) = s.get_doc(doc) else { continue };
            // mirrors are the owner's (their pull will bring the new title);
            // hot docs are the session's — their links get fixed on the next
            // rename or by a gardener, never by writing under a live session
            if s.get_mirror(doc).ok().flatten().is_some() {
                continue;
            }
            if st.hot.is_hot(doc) {
                deferred += blocks.len();
                continue;
            }
            let ops: Vec<OpInput> = blocks
                .into_iter()
                .filter_map(|(block, content)| {
                    let new_content = rewrite_links(&content, &old_title, new_title);
                    (new_content != content).then(|| OpInput {
                        kind: ks_store_op_replace(block, new_content),
                        source_refs: vec![format!("rename:{old_title} → {new_title}")],
                    })
                })
                .collect();
            if ops.is_empty() {
                continue;
            }
            rewritten += ops.len();
            let _ = s.propose(doc, d.current_epoch, st.human, ops);
        }
        if deferred > 0 {
            return Json(json!({"ok": true, "links_rewritten": rewritten, "links_deferred_hot": deferred}));
        }
    }
    Json(json!({"ok": true, "links_rewritten": rewritten}))
}

fn ks_store_op_replace(target: Uuid, content: String) -> grimoire_store::OpKind {
    grimoire_store::OpKind::Replace { target, content }
}

async fn delete_doc(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(e) = refuse_if_mirror(&s, id, "deleting") {
        return Json(json!({"error": e}));
    }
    // a live session anywhere in the subtree would flatten into a tombstone
    // (and the session would outlive the doc): end it first
    match s.doc_subtree_ids(id) {
        Ok(ids) => {
            if let Some(hot) = ids.iter().find(|d| st.hot.is_hot(**d)) {
                let title = s.get_doc(*hot).map(|d| d.title).unwrap_or_default();
                return Json(json!({
                    "error": format!("“{title}” is in a live session — end it before deleting"),
                    "code": "doc_hot",
                }));
            }
        }
        Err(e) => return Json(json!({"error": e.to_string()})),
    }
    match s.delete_doc(id) {
        Ok(n) => Json(json!({"ok": true, "deleted": n})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Backups: list snapshots (GET) or take one now (POST).
async fn backups(State(st): State<ApiState>) -> Json<Value> {
    Json(json!({
        "dir": crate::backup::backup_dir(&st.db_path).to_string_lossy(),
        "backups": crate::backup::list_backups(&st.db_path),
    }))
}

async fn backup_now(State(st): State<ApiState>) -> Json<Value> {
    let path = st.db_path.clone();
    match tokio::task::spawn_blocking(move || crate::backup::backup_now(&path, true)).await {
        Ok(Ok(info)) => Json(json!(info)),
        Ok(Err(e)) => Json(json!({"error": format!("{e:#}")})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Export every doc as a markdown tree under ~/Downloads (the escape hatch,
/// in-app). Titles become file names; docs with children become folders.
async fn export_vault(State(st): State<ApiState>) -> Json<Value> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M").to_string();
    let dir = std::path::PathBuf::from(home)
        .join("Downloads")
        .join(format!("grimoire-export-{stamp}"));
    let store = st.store.clone();
    let out = dir.clone();
    let res = tokio::task::spawn_blocking(move || {
        let s = store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        grimoire_store::export::export_vault(&*s, &out)
    })
    .await;
    match res {
        Ok(Ok(report)) => Json(json!({"path": dir.to_string_lossy(), "files": report.files})),
        Ok(Err(e)) => Json(json!({"error": e.to_string()})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct ImportFile {
    /// Relative path inside the chosen folder (`notes/2026/a.md`).
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct ImportReq {
    files: Vec<ImportFile>,
}

/// Import a folder of markdown from the app. The browser can't hand the
/// daemon a directory path (WKWebView uploads file contents), so the UI
/// sends `{path, content}` pairs. Folders become docs with children, files
/// become docs — the same shape `import_vault` gives the CLI — but nothing
/// touches disk: every file is parsed OFF the store lock and the lock is
/// taken per doc, so a 5000-file import never stalls the UI for its
/// duration. Non-markdown files are skipped (the UI filters too).
async fn import_markdown(State(st): State<ApiState>, Json(req): Json<ImportReq>) -> Json<Value> {
    const MAX_FILES: usize = 5000;
    const MAX_BYTES: usize = 200 * 1024 * 1024;
    if req.files.is_empty() {
        return Json(json!({"error": "no markdown files in that folder"}));
    }
    if req.files.len() > MAX_FILES {
        return Json(json!({"error": format!("too many files ({}); import in smaller folders", req.files.len())}));
    }
    let total: usize = req.files.iter().map(|f| f.content.len()).sum();
    if total > MAX_BYTES {
        return Json(json!({"error": "that folder is over 200 MB; import in smaller folders"}));
    }
    for f in &req.files {
        // no absolute paths, no traversal: every component must be a plain name
        let rel = std::path::Path::new(&f.path);
        if rel.is_absolute()
            || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Json(json!({"error": format!("refusing path {:?}", f.path)}));
        }
    }
    let store = st.store.clone();
    let human = st.human;
    let res = tokio::task::spawn_blocking(move || import_files(&store, human, req.files)).await;
    match res {
        Ok(Ok(report)) => Json(json!({
            "docs": report.docs,
            "blocks": report.blocks,
            "skipped": report.skipped.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
        })),
        Ok(Err(e)) => Json(json!({"error": e.to_string()})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// The import walk, in memory: sorted by path (folders create in order, as
/// the CLI walk does), parse outside the lock, one lock per doc.
fn import_files(
    store: &Arc<Mutex<SqliteStore>>,
    human: Uuid,
    mut files: Vec<ImportFile>,
) -> grimoire_store::Result<grimoire_store::import::ImportReport> {
    use grimoire_store::import::{ImportReport, segment, to_ops};
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut report = ImportReport::default();
    // folder path → doc id, created on first use
    let mut folders: HashMap<std::path::PathBuf, Uuid> = HashMap::new();
    for f in files {
        let rel = std::path::PathBuf::from(&f.path);
        let name = rel.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if name.starts_with('.') || rel.extension().and_then(|e| e.to_str()) != Some("md") {
            report.skipped.push(rel);
            continue;
        }
        let title = rel.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or(name);
        // parse off the lock
        let ops = to_ops(segment(&f.content));
        // folders: walk the ancestors root-first, each its own short lock
        let mut parent: Option<Uuid> = None;
        let mut sofar = std::path::PathBuf::new();
        for comp in rel.parent().map(|p| p.components().collect::<Vec<_>>()).unwrap_or_default() {
            sofar.push(comp);
            if sofar.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                break;
            }
            let id = match folders.get(&sofar) {
                Some(id) => *id,
                None => {
                    let mut s = store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let d = s.create_doc(&comp.as_os_str().to_string_lossy(), parent, human)?;
                    report.docs += 1;
                    folders.insert(sofar.clone(), d.id);
                    d.id
                }
            };
            parent = Some(id);
        }
        let mut s = store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let doc = s.create_doc(&title, parent, human)?;
        let n = ops.len();
        if n > 0 {
            s.apply(doc.id, 0, human, ops)?;
        }
        report.docs += 1;
        report.blocks += n;
    }
    Ok(report)
}

#[derive(Deserialize)]
struct AskReq {
    question: String,
}

/// Ask the vault: question → answer doc with block-level citations. Waits
/// for the model (a minute at most); the UI shows "thinking…" meanwhile.
async fn ask_vault(State(st): State<ApiState>, Json(req): Json<AskReq>) -> Json<Value> {
    match crate::ask::ask(st.store.clone(), st.embedder.clone(), st.human, req.question).await {
        Ok(a) => Json(json!(a)),
        Err(e) => Json(json!({"error": e})),
    }
}

/// Sync Claude Code's memory files into `Claude Memory` now.
async fn memory_sync(State(st): State<ApiState>) -> Json<Value> {
    let store = st.store.clone();
    let human = st.human;
    let root = crate::memory::memory_root();
    if !root.is_dir() {
        return Json(json!({"error": format!("no Claude Code memory found at {}", root.display())}));
    }
    match tokio::task::spawn_blocking(move || crate::memory::sync(&store, &root, human)).await {
        Ok(Ok(r)) => Json(json!(r)),
        Ok(Err(e)) => Json(json!({"error": e.to_string()})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn trash(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_trash() {
        Ok(rows) => Json(json!(rows)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn restore_doc(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.restore_doc(id) {
        Ok(n) => Json(json!({"ok": true, "restored": n})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct ExportReq {
    filename: String,
    /// data:image/png;base64,… or data:image/svg+xml;…
    data_url: String,
}

/// Save a canvas export to ~/Downloads (#68 v2.2). WKWebView downloads are
/// unreliable in Tauri, so the daemon writes the file — human surface only.
async fn export_file(Json(req): Json<ExportReq>) -> Json<Value> {
    let name: String = req
        .filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if name.is_empty() || name.starts_with('.') {
        return Json(json!({"error": "bad filename"}));
    }
    let Some((_, payload)) = req.data_url.split_once(',') else {
        return Json(json!({"error": "not a data url"}));
    };
    let bytes = if req.data_url.contains(";base64,") {
        use base64::Engine;
        match base64::engine::general_purpose::STANDARD.decode(payload) {
            Ok(b) => b,
            Err(e) => return Json(json!({"error": format!("bad base64: {e}")})),
        }
    } else {
        match urlencoding_decode(payload) {
            Ok(s) => s.into_bytes(),
            Err(e) => return Json(json!({"error": e})),
        }
    };
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dir = std::path::PathBuf::from(home).join("Downloads");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join(&name);
    match std::fs::write(&path, bytes) {
        Ok(()) => Json(json!({"path": path.to_string_lossy()})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

fn urlencoding_decode(s: &str) -> Result<String, String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).map_err(|e| e.to_string())?;
                out.push(u8::from_str_radix(hex, 16).map_err(|e| e.to_string())?);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/docs", get(docs).post(create_doc))
        .route("/api/propose", post(propose))
        // axum's default body cap is 2 MiB: whole-doc markdown and folder
        // imports need more, and the import's own 200 MB guard must be reachable
        .route(
            "/api/propose_markdown",
            post(propose_markdown).layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route("/api/doc/{id}", get(doc))
        .route("/api/doc/{id}/backlinks", get(backlinks))
        .route("/api/queue", get(queue))
        .route("/api/doc/{id}/review", get(doc_review))
        .route("/api/activity", get(activity))
        .route("/api/doc/{id}/focus", post(focus_doc))
        .route("/api/events", get(events))
        .route("/api/flags", get(flags))
        .route("/api/flags/dismiss", post(dismiss_flag))
        .route("/api/principals", get(principals))
        .route("/api/doc/{id}/history", get(history))
        .route("/api/doc/{id}/status", post(set_status))
        .route("/api/doc/{id}/move", post(move_doc))
        .route("/api/buildinfo", get(buildinfo))
        .route("/api/diagnostics", get(diagnostics))
        .route("/api/gardeners/preflight", get(gardeners_preflight))
        .route("/api/stamp", get(stamp))
        .route("/api/doc/{id}/delete", post(delete_doc))
        .route("/api/doc/{id}/restore", post(restore_doc))
        .route("/api/trash", get(trash))
        .route("/api/backups", get(backups).post(backup_now))
        .route("/api/export_vault", post(export_vault))
        .route(
            "/api/import",
            post(import_markdown).layer(axum::extract::DefaultBodyLimit::max(256 * 1024 * 1024)),
        )
        .route("/api/ask", post(ask_vault))
        .route("/api/memory/sync", post(memory_sync))
        .route("/api/doc/{id}/rename", post(rename_doc))
        .route("/api/doc/{id}/tendings", get(tendings))
        .route("/api/doc/{id}/federation", get(doc_federation))
        .route("/api/hub", get(hub_info))
        .route("/api/comment", post(add_comment))
        .route("/api/resolve", post(resolve))
        .route("/api/resolve_bulk", post(resolve_bulk))
        .route("/api/search", get(search))
        .route("/api/tags", get(tags))
        .route("/api/runs", get(runs))
        .route("/api/graph", get(graph))
        .route("/api/render/d2", post(render_d2))
        .route("/api/export", post(export_file))
        .with_state(state)
}

#[cfg(test)]
mod http_client_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use grimoire_store::PrincipalKind;
    use tower::ServiceExt;

    fn app() -> (Router, Uuid) {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let human = store.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let dir = std::env::temp_dir().join(format!("grimoire-api-test-{}", Uuid::now_v7()));
        let st = ApiState {
            store: Arc::new(Mutex::new(store)),
            human,
            hot: crate::hot::HotState::new(dir.clone()),
            runtime: crate::fed::Runtime::default(),
            db_path: dir.join("ks.db"),
            node_id: None,
            embedder: None,
            dedupe: crate::mcp::new_dedupe(),
        };
        (router(st), human)
    }

    async fn call(app: &Router, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> Value {
        let mut req = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&b).unwrap()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn new_doc(app: &Router, headers: &[(&str, &str)]) -> Value {
        call(app, "POST", "/api/docs", headers, Some(json!({"title": "T", "parent_doc_id": null}))).await
    }

    #[tokio::test]
    async fn body_limits_are_per_route() {
        let (app, _) = app();
        let doc = new_doc(&app, &[]).await;
        let id = doc["id"].as_str().unwrap().to_string();
        // 3 MiB is over axum's 2 MiB default: propose_markdown must still take it
        let big = "x".repeat(3 * 1024 * 1024);
        let body = serde_json::to_vec(&json!({"doc_id": id, "base_epoch": 0, "markdown": big})).unwrap();
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/propose_markdown")
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // the import route takes far more (its own guard says 200 MB)
        let files = json!({"files": [{"path": "a.md", "content": "x".repeat(3 * 1024 * 1024)}]});
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/import")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&files).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // every other route keeps the default
        let res = app
            .oneshot(
                Request::post("/api/docs")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn import_builds_folders_from_paths_and_skips_non_markdown() {
        let (app, _) = app();
        let files = json!({"files": [
            {"path": "notes/sub/b.md", "content": "# B\n\nbody b\n"},
            {"path": "top.md", "content": "top\n"},
            {"path": "notes/a.md", "content": "a one\n\na two\n"},
            {"path": "notes/img.png", "content": "not md"},
        ]});
        let v = call(&app, "POST", "/api/import", &[], Some(files)).await;
        // notes, sub, a, b, top
        assert_eq!(v["docs"], 5, "{v}");
        assert_eq!(v["blocks"], 5);
        assert_eq!(v["skipped"], json!(["notes/img.png"]));
        let docs = call(&app, "GET", "/api/docs", &[], None).await;
        let docs = docs.as_array().unwrap();
        let find = |t: &str| docs.iter().find(|d| d["title"] == t).unwrap_or_else(|| panic!("{t}: {docs:?}"));
        let notes = find("notes");
        assert!(notes["parent_id"].is_null());
        assert_eq!(find("sub")["parent_id"], notes["id"]);
        assert_eq!(find("b")["parent_id"], find("sub")["id"]);
        assert_eq!(find("a")["parent_id"], notes["id"]);
        assert!(find("top")["parent_id"].is_null());
    }

    #[tokio::test]
    async fn propose_markdown_diffs_then_refuses_a_stale_base() {
        let (app, _) = app();
        let doc = new_doc(&app, &[]).await;
        let id = doc["id"].as_str().unwrap().to_string();
        let out = call(
            &app,
            "POST",
            "/api/propose_markdown",
            &[],
            Some(json!({"doc_id": id, "base_epoch": 0, "markdown": "# H\n\nfirst para\n"})),
        )
        .await;
        assert_eq!(out["epoch"], 1, "{out}");
        assert_eq!(out["verdicts"].as_array().unwrap().len(), 2);
        assert!(out["verdicts"].as_array().unwrap().iter().all(|v| v["verdict"] == "green" && v["applied"] == true));
        let tree = call(&app, "GET", &format!("/api/doc/{id}"), &[], None).await;
        assert_eq!(tree["doc"]["current_epoch"], 1);
        assert_eq!(tree["roots"][0]["block"]["content"], "# H");
        assert_eq!(tree["roots"][0]["children"][0]["block"]["content"], "first para");
        // same markdown again → no ops, epoch unchanged
        let out = call(
            &app,
            "POST",
            "/api/propose_markdown",
            &[],
            Some(json!({"doc_id": id, "base_epoch": 1, "markdown": "# H\n\nfirst para\n"})),
        )
        .await;
        assert_eq!(out["note"], "no changes");
        assert_eq!(out["epoch"], 1);
        // stale base → typed error with the missed ops, nothing applied
        let out = call(
            &app,
            "POST",
            "/api/propose_markdown",
            &[],
            Some(json!({"doc_id": id, "base_epoch": 0, "markdown": "# H\n\nedited\n"})),
        )
        .await;
        assert_eq!(out["error"], "stale_base");
        assert_eq!(out["current_epoch"], 1);
        assert_eq!(out["missed_ops"].as_array().unwrap().len(), 2);
        let tree = call(&app, "GET", &format!("/api/doc/{id}"), &[], None).await;
        assert_eq!(tree["doc"]["current_epoch"], 1);
    }

    #[tokio::test]
    async fn principal_header_attributes_writes_to_a_named_agent() {
        let (app, human) = app();
        // absent → the human
        let doc = new_doc(&app, &[]).await;
        assert_eq!(doc["created_by"], human.to_string());
        // present → an Agent principal created on first use, reused after
        let doc = new_doc(&app, &[(PRINCIPAL_HEADER, "workbox")]).await;
        let agent = doc["created_by"].as_str().unwrap().to_string();
        assert_ne!(agent, human.to_string());
        let doc2 = new_doc(&app, &[("X-Grimoire-Principal", "workbox")]).await;
        assert_eq!(doc2["created_by"], agent);
        let ps = call(&app, "GET", "/api/principals", &[], None).await;
        let p = ps.as_array().unwrap().iter().find(|p| p["id"] == agent).unwrap();
        assert_eq!(p["display_name"], "workbox");
        assert_eq!(p["kind"], "agent");
        // the agent's propose carries its principal into the ledger
        let id = doc["id"].as_str().unwrap();
        let out = call(
            &app,
            "POST",
            "/api/propose",
            &[(PRINCIPAL_HEADER, "workbox")],
            Some(json!({"doc_id": id, "base_epoch": 0, "ops": [{"kind": {"op": "insert",
                "block_id": Uuid::now_v7(), "parent_id": null, "order_key": "",
                "block_type": "paragraph", "content": "hi"}}]})),
        )
        .await;
        assert_eq!(out["verdicts"][0]["verdict"], "green", "{out}");
        let hist = call(&app, "GET", &format!("/api/doc/{id}/history"), &[], None).await;
        assert_eq!(hist[0]["principal_name"], "workbox");
        // invalid names are refused before anything is written
        let bad = new_doc(&app, &[(PRINCIPAL_HEADER, "   ")]).await;
        assert!(bad["error"].as_str().unwrap().contains("1-64 printable chars"), "{bad}");
        let long = "x".repeat(65);
        let bad = new_doc(&app, &[(PRINCIPAL_HEADER, long.as_str())]).await;
        assert!(bad["error"].as_str().unwrap().contains("1-64 printable chars"));
    }

    #[tokio::test]
    async fn request_id_makes_propose_idempotent_per_principal() {
        let (app, _) = app();
        let doc = new_doc(&app, &[]).await;
        let id = doc["id"].as_str().unwrap().to_string();
        let rid = Uuid::now_v7();
        let body = json!({"doc_id": id, "base_epoch": 0, "request_id": rid, "ops": [{"kind": {"op": "insert",
            "block_id": Uuid::now_v7(), "parent_id": null, "order_key": "",
            "block_type": "paragraph", "content": "once"}}]});
        let first = call(&app, "POST", "/api/propose", &[], Some(body.clone())).await;
        assert_eq!(first["epoch"], 1, "{first}");
        // the retry returns the stored outcome; the doc did not move
        let second = call(&app, "POST", "/api/propose", &[], Some(body.clone())).await;
        assert_eq!(second, first);
        let tree = call(&app, "GET", &format!("/api/doc/{id}"), &[], None).await;
        assert_eq!(tree["doc"]["current_epoch"], 1);
        assert_eq!(tree["roots"].as_array().unwrap().len(), 1);
        // another principal with the same request_id is not replayed: its
        // ops are scored on their own (stale base here → insert still greens)
        let mut body2 = body.clone();
        body2["ops"][0]["kind"]["block_id"] = json!(Uuid::now_v7());
        let other = call(&app, "POST", "/api/propose", &[(PRINCIPAL_HEADER, "other")], Some(body2)).await;
        assert_ne!(other, first);
        assert_eq!(other["epoch"], 2, "{other}");
        // propose_markdown honours it too
        let rid = Uuid::now_v7();
        let body = json!({"doc_id": id, "base_epoch": 2, "request_id": rid, "markdown": "only\n"});
        let a = call(&app, "POST", "/api/propose_markdown", &[], Some(body.clone())).await;
        let b = call(&app, "POST", "/api/propose_markdown", &[], Some(body)).await;
        assert_eq!(a, b);
        assert_eq!(a["epoch"], 3, "{a}");
    }
}
