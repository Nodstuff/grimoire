//! JSON API for the human UI (M5). This surface acts as the HUMAN principal
//! (tom) — resolve here is Tom clicking accept/decline. Agents use MCP.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use grimoire_store::{BlockStore, DocStatus, OpInput, ReviewDecision, SqliteStore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<SqliteStore>>,
    pub human: Uuid,
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
    let mirrors: std::collections::HashMap<String, String> = s
        .list_mirrors()
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.doc_id.to_string(), m.permission.as_str().to_string()))
        .collect();
    let share_roots: std::collections::HashSet<String> = s
        .list_shares()
        .unwrap_or_default()
        .into_iter()
        .filter(|sh| sh.state != grimoire_store::ShareState::Revoked)
        .map(|sh| sh.root_doc.to_string())
        .collect();
    match s.list_docs() {
        Ok(d) => Json(json!(
            d.into_iter()
                .map(|doc| {
                    let id = doc.id.to_string();
                    let mut v = json!(doc);
                    v["is_canvas"] = json!(canvases.contains(&id));
                    v["is_tended"] = json!(tended.contains(&id));
                    if let Some(perm) = mirrors.get(&id) {
                        v["mirror_permission"] = json!(perm);
                    }
                    v["is_shared"] = json!(share_roots.contains(&id));
                    v
                })
                .collect::<Vec<_>>()
        )),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
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
    let mirror = s.get_mirror(id).ok().flatten().map(|m| {
        json!({
            "owner": m.owner,
            "owner_petname": petname_of(m.owner),
            "permission": m.permission,
            "synced_epoch": m.synced_epoch,
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

async fn queue(State(st): State<ApiState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.review_queue(None) {
        Ok(q) => {
            // decorate with doc titles + principal names for rendering
            let rows: Vec<Value> = q
                .into_iter()
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
                .collect();
            Json(json!(rows))
        }
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
}

/// Human writes: propose as tom — current-epoch ops green and apply directly.
async fn propose(State(st): State<ApiState>, Json(req): Json<ProposeReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.propose(req.doc_id, req.base_epoch, st.human, req.ops) {
        Ok(out) => Json(json!(out)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct CreateDocReq {
    title: String,
    parent_doc_id: Option<Uuid>,
}

async fn create_doc(State(st): State<ApiState>, Json(req): Json<CreateDocReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.create_doc(&req.title, req.parent_doc_id, st.human) {
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
    match s.ops_since(id, 0) {
        Ok(mut ops) => {
            ops.reverse();
            ops.truncate(100);
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

async fn add_comment(State(st): State<ApiState>, Json(req): Json<CommentReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.add_comment(req.block_id, st.human, &req.text, req.reply_to) {
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
async fn render_d2(Json(req): Json<D2Req>) -> Json<Value> {
    let bin = ["d2", "/opt/homebrew/bin/d2", "/usr/local/bin/d2"]
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
        .copied();
    let Some(bin) = bin else {
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
        Ok(v) => Json(json!({"stamp": v})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// UI build stamp: mtime of the served index.html. The app polls this and
/// reloads itself when a deploy lands — no manual ⌘R.
async fn buildinfo() -> Json<Value> {
    let dist = std::env::var("GRIMOIRE_UI_DIST")
        .unwrap_or_else(|_| "/Users/tmeaney/personal/knowledge-system/ui/dist".into());
    let stamp = std::fs::metadata(format!("{dist}/index.html"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Json(json!({"build": stamp}))
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
    match s.move_doc(id, req.parent_id, req.sort_key.as_deref()) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
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
        for (doc, blocks) in by_doc {
            let Ok(d) = s.get_doc(doc) else { continue };
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
    match s.delete_doc(id) {
        Ok(n) => Json(json!({"ok": true, "deleted": n})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/docs", get(docs).post(create_doc))
        .route("/api/propose", post(propose))
        .route("/api/doc/{id}", get(doc))
        .route("/api/doc/{id}/backlinks", get(backlinks))
        .route("/api/queue", get(queue))
        .route("/api/flags", get(flags))
        .route("/api/flags/dismiss", post(dismiss_flag))
        .route("/api/principals", get(principals))
        .route("/api/doc/{id}/history", get(history))
        .route("/api/doc/{id}/status", post(set_status))
        .route("/api/doc/{id}/move", post(move_doc))
        .route("/api/buildinfo", get(buildinfo))
        .route("/api/stamp", get(stamp))
        .route("/api/doc/{id}/delete", post(delete_doc))
        .route("/api/doc/{id}/rename", post(rename_doc))
        .route("/api/doc/{id}/tendings", get(tendings))
        .route("/api/doc/{id}/federation", get(doc_federation))
        .route("/api/comment", post(add_comment))
        .route("/api/resolve", post(resolve))
        .route("/api/resolve_bulk", post(resolve_bulk))
        .route("/api/search", get(search))
        .route("/api/tags", get(tags))
        .route("/api/runs", get(runs))
        .route("/api/graph", get(graph))
        .route("/api/render/d2", post(render_d2))
        .with_state(state)
}
