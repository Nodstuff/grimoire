//! JSON API for the human UI (M5). This surface acts as the HUMAN principal
//! (tom) — resolve here is Tom clicking accept/decline. Agents use MCP.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use ks_store::{BlockStore, DocStatus, OpInput, ReviewDecision, SqliteStore};
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
    match s.list_docs() {
        Ok(d) => Json(json!(d)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
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
        Ok(b) if b.block_type == ks_store::BlockType::Comment => b,
        Ok(_) => return Json(json!({"error": "not a comment block"})),
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    let epoch = match s.get_doc(block.doc_id) {
        Ok(d) => d.current_epoch,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    let op = OpInput {
        kind: ks_store::OpKind::Delete {
            target: req.comment_id,
        },
        source_refs: vec!["flag:dismissed".into()],
    };
    match s.propose(block.doc_id, epoch, st.human, vec![op]) {
        Ok(_) => Json(json!({"ok": true})),
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
        .route("/api/comment", post(add_comment))
        .route("/api/resolve", post(resolve))
        .route("/api/search", get(search))
        .route("/api/tags", get(tags))
        .route("/api/runs", get(runs))
        .route("/api/graph", get(graph))
        .route("/api/render/d2", post(render_d2))
        .with_state(state)
}
