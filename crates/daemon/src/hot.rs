//! Hot sessions (#65, ADR 0003): live co-editing.
//!
//! The daemon hosts a yrs doc per hot session and speaks the y-sync protocol
//! over `/ws/hot/{doc}` — wire-compatible with y-websocket clients. While a
//! doc is hot its epoch is FROZEN: the daemon-level hot set gates every
//! propose surface, and the collab session is the only writer. Cool-down:
//! the ending client flattens the final editor state through the existing
//! diff/propose machinery at the frozen epoch (block ids ride the Yjs doc as
//! node attrs, so unchanged blocks keep ids and comment anchors survive);
//! `confirm` drops the session and its journal.
//!
//! Crash safety: every incoming update frame is appended (length-prefixed)
//! to a journal under ~/.grimoire/hot/. A daemon restart with a journal
//! present re-hydrates the session — the doc simply stays hot until someone
//! ends it properly. Journals die only on confirmed flatten.

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;
use yrs::sync::{Awareness, DefaultProtocol, Message as YMessage, Protocol, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{Doc, ReadTxn, Transact, Update};

pub struct HotSession {
    pub awareness: Awareness,
    /// Encoded y-sync frames fanned out to every connected socket.
    pub tx: broadcast::Sender<Vec<u8>>,
    pub frozen_epoch: i64,
    pub journal: std::fs::File,
    pub started_at: std::time::Instant,
    pub last_activity: std::time::Instant,
    /// Guard against a session that was ended but never confirmed.
    pub ending: bool,
    _doc_sub: yrs::Subscription,
}

#[derive(Clone, Default)]
pub struct HotState {
    pub sessions: Arc<Mutex<HashMap<Uuid, HotSession>>>,
    pub journal_dir: Arc<std::path::PathBuf>,
    /// Cold-editor heartbeats (auto-hot, P2.1): doc → editor key → last ping.
    /// Two live keys on a cold doc = concurrent editing = the UIs go live.
    pub editing: Arc<Mutex<HashMap<Uuid, HashMap<Uuid, std::time::Instant>>>>,
}

impl HotState {
    pub fn new(journal_dir: std::path::PathBuf) -> Self {
        std::fs::create_dir_all(&journal_dir).ok();
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            journal_dir: Arc::new(journal_dir),
            editing: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record a cold-editor heartbeat; returns how many distinct editors
    /// pinged within the liveness window.
    pub fn edit_ping(&self, doc_id: Uuid, editor_key: Uuid) -> usize {
        const LIVE: std::time::Duration = std::time::Duration::from_secs(10);
        let mut editing = self.editing.lock().unwrap_or_else(|p| p.into_inner());
        let keys = editing.entry(doc_id).or_default();
        keys.insert(editor_key, std::time::Instant::now());
        keys.retain(|_, t| t.elapsed() < LIVE);
        keys.len()
    }

    pub fn editors(&self, doc_id: Uuid) -> usize {
        const LIVE: std::time::Duration = std::time::Duration::from_secs(10);
        let mut editing = self.editing.lock().unwrap_or_else(|p| p.into_inner());
        match editing.get_mut(&doc_id) {
            Some(keys) => {
                keys.retain(|_, t| t.elapsed() < LIVE);
                keys.len()
            }
            None => 0,
        }
    }

    /// Is this doc in a live session? Consulted by every propose surface.
    pub fn is_hot(&self, doc: Uuid) -> bool {
        let s = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        s.get(&doc).map(|h| !h.ending).unwrap_or(false)
    }

    fn journal_path(&self, doc: Uuid) -> std::path::PathBuf {
        self.journal_dir.join(format!("{doc}.yjournal"))
    }

    /// Recover sessions whose journals survived a daemon restart: the doc
    /// stays hot with its full state; participants reconnect.
    pub fn recover(&self, store: &Arc<Mutex<grimoire_store::SqliteStore>>) {
        let Ok(entries) = std::fs::read_dir(&*self.journal_dir) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(doc_id) = name
                .strip_suffix(".yjournal")
                .and_then(|s| s.parse::<Uuid>().ok())
            else {
                continue;
            };
            let frozen_epoch = {
                use grimoire_store::BlockStore;
                let s = store.lock().unwrap_or_else(|p| p.into_inner());
                match s.get_doc(doc_id) {
                    Ok(d) => d.current_epoch,
                    Err(_) => continue,
                }
            };
            match self.start(doc_id, frozen_epoch) {
                Ok(created) => {
                    tracing::warn!(%doc_id, created, "recovered hot session from journal")
                }
                Err(e) => tracing::error!(%doc_id, "journal recovery failed: {e}"),
            }
        }
    }

    /// Create (or join) the session. Returns true when this call created it —
    /// the caller that created it seeds the Yjs doc from the current content.
    pub fn start(&self, doc_id: Uuid, frozen_epoch: i64) -> std::io::Result<bool> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = sessions.get_mut(&doc_id) {
            existing.ending = false; // an aborted end re-opens
            return Ok(false);
        }
        let path = self.journal_path(doc_id);
        let had_journal = path.exists();
        let doc = Doc::new();
        // replay a surviving journal before anything else touches the doc
        if had_journal {
            if let Ok(mut f) = std::fs::File::open(&path) {
                let mut buf = Vec::new();
                f.read_to_end(&mut buf).ok();
                let mut off = 0usize;
                while off + 4 <= buf.len() {
                    let len =
                        u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
                    off += 4;
                    if off + len > buf.len() {
                        break; // torn tail write: ignore
                    }
                    if let Ok(u) = Update::decode_v1(&buf[off..off + len]) {
                        doc.transact_mut().apply_update(u).ok();
                    }
                    off += len;
                }
            }
        }
        let journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let (tx, _) = broadcast::channel::<Vec<u8>>(256);
        // fan out every doc change as a y-sync Update frame; clients ignore
        // their own (Yjs updates apply idempotently)
        let fan = tx.clone();
        let sub = doc
            .observe_update_v1(move |_, e| {
                let mut enc = EncoderV1::new();
                YMessage::Sync(SyncMessage::Update(e.update.clone())).encode(&mut enc);
                fan.send(enc.to_vec()).ok();
            })
            .expect("observer");
        sessions.insert(
            doc_id,
            HotSession {
                awareness: Awareness::new(doc),
                tx,
                frozen_epoch,
                journal,
                started_at: std::time::Instant::now(),
                last_activity: std::time::Instant::now(),
                ending: false,
                _doc_sub: sub,
            },
        );
        Ok(!had_journal)
    }
}

impl HotState {
    /// Subscribe to a live session: returns the fan-out receiver plus the
    /// y-sync handshake frames to send first. None when the doc isn't hot.
    pub fn connect(&self, doc_id: Uuid) -> Option<(broadcast::Receiver<Vec<u8>>, Vec<u8>)> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let session = sessions.get_mut(&doc_id)?;
        if session.ending {
            return None;
        }
        let mut enc = EncoderV1::new();
        DefaultProtocol.start(&session.awareness, &mut enc).ok()?;
        Some((session.tx.subscribe(), enc.to_vec()))
    }

    /// One incoming y-sync frame from any transport: journal, apply, fan out
    /// replies + awareness. Returns false when the session is gone/ending.
    pub fn handle_frame(&self, doc_id: Uuid, data: &[u8]) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let Some(session) = sessions.get_mut(&doc_id) else {
            return false;
        };
        if session.ending {
            return false;
        }
        session.last_activity = std::time::Instant::now();
        journal_updates(&mut session.journal, data);
        let replies = DefaultProtocol.handle(&mut session.awareness, data);
        rebroadcast_awareness(&session.tx, data);
        if let Ok(msgs) = replies {
            for m in msgs {
                let mut enc = EncoderV1::new();
                m.encode(&mut enc);
                session.tx.send(enc.to_vec()).ok();
            }
        }
        true
    }

    pub fn status(&self, doc_id: Uuid) -> (bool, Option<i64>) {
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        match sessions.get(&doc_id) {
            Some(s) if !s.ending => (true, Some(s.frozen_epoch)),
            _ => (false, None),
        }
    }
}

impl HotState {
    /// The one flatten (#67): render the session's Yjs doc to markdown,
    /// LCS-diff it against the live blocks (mddiff — unchanged blocks keep
    /// ids, comments and canvases untouched), land ONE propose at the frozen
    /// epoch, drop the session and its journal. Every ending path — owner
    /// end, grantee relay, idle timeout, recovery — comes through here.
    pub fn flatten_and_close(
        &self,
        store: &Arc<Mutex<grimoire_store::SqliteStore>>,
        doc_id: Uuid,
        reason: &str,
    ) -> anyhow::Result<usize> {
        use grimoire_store::BlockStore;
        // 1. mark ending (stops new frames) and render under the lock
        let (markdown, frozen_epoch, secs) = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let session = sessions
                .get_mut(&doc_id)
                .ok_or_else(|| anyhow::anyhow!("doc is not hot"))?;
            session.ending = true;
            let frag = session.awareness.doc().get_or_insert_xml_fragment("default");
            let txn = session.awareness.doc().transact();
            let md = crate::yrender::fragment_to_markdown(&txn, &frag);
            (md, session.frozen_epoch, session.started_at.elapsed().as_secs())
        };
        // canvas sessions (#68/P2.5): shapes live in Y.Maps ("canvas_nodes"/
        // "canvas_edges", values = JSON strings, LWW per shape). Non-empty
        // maps mean this was a canvas session — flatten to ks_diagram.
        let canvas: Option<serde_json::Value> = {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions.get(&doc_id).and_then(|session| {
                use yrs::Map as _;
                let ydoc = session.awareness.doc();
                let nodes_map = ydoc.get_or_insert_map("canvas_nodes");
                let edges_map = ydoc.get_or_insert_map("canvas_edges");
                let txn = ydoc.transact();
                let parse_map = |m: &yrs::MapRef| -> Vec<serde_json::Value> {
                    let mut out: Vec<(String, serde_json::Value)> = m
                        .iter(&txn)
                        .filter_map(|(k, v)| {
                            let s = match v {
                                yrs::Out::Any(yrs::Any::String(s)) => s.to_string(),
                                _ => return None,
                            };
                            serde_json::from_str(&s).ok().map(|j| (k.to_string(), j))
                        })
                        .collect();
                    out.sort_by(|a, b| a.0.cmp(&b.0));
                    out.into_iter().map(|(_, v)| v).collect()
                };
                let nodes = parse_map(&nodes_map);
                if nodes.is_empty() {
                    return None;
                }
                let edges = parse_map(&edges_map);
                Some(serde_json::json!({"ks_diagram": {"nodes": nodes, "edges": edges}}))
            })
        };

        // 2. diff + propose OUTSIDE the session lock
        let applied = if let Some(content) = canvas {
            let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
            let tree = s.read_doc(doc_id)?;
            let target = tree
                .roots
                .iter()
                .find(|n| n.block.block_type == grimoire_store::BlockType::CanvasScene)
                .map(|n| n.block.id);
            let Some(target) = target else {
                anyhow::bail!("canvas session on a doc with no canvas block");
            };
            let human = s
                .list_principals()?
                .into_iter()
                .find(|p| p.kind == grimoire_store::PrincipalKind::Human)
                .ok_or_else(|| anyhow::anyhow!("no human principal"))?;
            let op = grimoire_store::OpInput {
                kind: grimoire_store::OpKind::Replace {
                    target,
                    content: content.to_string(),
                },
                source_refs: vec![format!("hot-session: {reason}, {secs}s"), "canvas:live".into()],
            };
            s.propose(doc_id, frozen_epoch, human.id, vec![op])?;
            1
        } else {
            let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
            let tree = s.read_doc(doc_id)?;
            let has_content = !tree.roots.is_empty();
            if markdown.trim().is_empty() && has_content {
                // never-seeded session: a flatten would mass-delete
                tracing::warn!(%doc_id, "hot session empty at flatten; doc kept as-is");
                0
            } else {
                let mut ops =
                    grimoire_store::mddiff::markdown_to_ops(&tree.roots, &markdown);
                for op in &mut ops {
                    op.source_refs
                        .push(format!("hot-session: {reason}, {secs}s"));
                }
                if ops.is_empty() {
                    0
                } else {
                    let human = s
                        .list_principals()?
                        .into_iter()
                        .find(|p| p.kind == grimoire_store::PrincipalKind::Human)
                        .ok_or_else(|| anyhow::anyhow!("no human principal"))?;
                    let n = ops.len();
                    s.propose(doc_id, frozen_epoch, human.id, ops)?;
                    n
                }
            }
        };
        // 3. drop the session (ends every socket/bridge) and the journal
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions.remove(&doc_id);
        }
        std::fs::remove_file(self.journal_path(doc_id)).ok();
        tracing::info!(%doc_id, reason, applied, "hot session flattened");
        Ok(applied)
    }

    /// Sessions with no frames for `idle` get flattened automatically —
    /// covers abandoned and crash-recovered sessions (ADR 0003 §5-6).
    pub fn idle_candidates(&self, idle: std::time::Duration) -> Vec<Uuid> {
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        sessions
            .iter()
            .filter(|(_, s)| !s.ending && s.last_activity.elapsed() > idle)
            .map(|(id, _)| *id)
            .collect()
    }
}

/// The idle reaper: one scan a minute; ten quiet minutes end a session.
pub async fn idle_loop(hot: HotState, store: Arc<Mutex<grimoire_store::SqliteStore>>) {
    const IDLE: std::time::Duration = std::time::Duration::from_secs(600);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        for doc_id in hot.idle_candidates(IDLE) {
            if let Err(e) = hot.flatten_and_close(&store, doc_id, "idle timeout") {
                tracing::error!(%doc_id, "idle flatten failed: {e:#}");
            }
        }
    }
}

static GLOBAL: std::sync::OnceLock<HotState> = std::sync::OnceLock::new();

/// Install the daemon-wide hot set (called once at serve start).
pub fn set_global(state: HotState) {
    GLOBAL.set(state).ok();
}

/// Is this doc in a live session? Safe from any propose surface; false when
/// the daemon isn't serving (CLI paths).
pub fn doc_is_hot(doc: Uuid) -> bool {
    GLOBAL.get().map(|h| h.is_hot(doc)).unwrap_or(false)
}

/// The daemon-wide hot set, for the federation surface (#66).
pub fn global() -> Option<&'static HotState> {
    GLOBAL.get()
}

#[derive(Clone)]
pub struct HotCtx {
    pub hot: HotState,
    pub store: Arc<Mutex<grimoire_store::SqliteStore>>,
    /// Federation endpoint for mirror docs (#66): status/start/ws relay to
    /// the owner's session. None = federation disabled.
    pub endpoint: Option<iroh::Endpoint>,
}

fn mirror_of(ctx: &HotCtx, doc_id: Uuid) -> bool {
    use grimoire_store::BlockStore;
    let s = ctx.store.lock().unwrap_or_else(|p| p.into_inner());
    s.get_mirror(doc_id).ok().flatten().is_some()
}

async fn hot_start(State(ctx): State<HotCtx>, Path(doc_id): Path<Uuid>) -> Json<Value> {
    use grimoire_store::BlockStore;
    // mirror docs: the session lives on the OWNER's daemon (#66)
    if mirror_of(&ctx, doc_id) {
        let Some(ep) = &ctx.endpoint else {
            return Json(json!({"error": "federation disabled"}));
        };
        return match crate::fed::hot_start_upstream(ep, &ctx.store, doc_id).await {
            Ok((frozen_epoch, seed)) => Json(json!({
                "ws": format!("/ws/hot/{doc_id}"),
                "frozen_epoch": frozen_epoch,
                "seed": seed,
            })),
            Err(e) => Json(json!({"error": format!("{e:#}")})),
        };
    }
    let frozen_epoch = {
        let s = ctx.store.lock().unwrap_or_else(|p| p.into_inner());
        match s.get_doc(doc_id) {
            Ok(d) => d.current_epoch,
            Err(e) => return Json(json!({"error": e.to_string()})),
        }
    };
    match ctx.hot.start(doc_id, frozen_epoch) {
        Ok(seed) => Json(json!({
            "ws": format!("/ws/hot/{doc_id}"),
            "frozen_epoch": frozen_epoch,
            "seed": seed,
        })),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// End the session: the DAEMON flattens (#67) — one propose at the frozen
/// epoch, session and journal dropped. Mirrors relay the end to the owner.
async fn hot_end(State(ctx): State<HotCtx>, Path(doc_id): Path<Uuid>) -> Json<Value> {
    if mirror_of(&ctx, doc_id) {
        let Some(ep) = &ctx.endpoint else {
            return Json(json!({"error": "federation disabled"}));
        };
        return match crate::fed::hot_end_upstream(ep, &ctx.store, doc_id).await {
            Ok(applied) => Json(json!({"flattened_ops": applied})),
            Err(e) => Json(json!({"error": format!("{e:#}")})),
        };
    }
    match ctx.hot.flatten_and_close(&ctx.store, doc_id, "ended") {
        Ok(applied) => Json(json!({"flattened_ops": applied})),
        Err(e) => Json(json!({"error": format!("{e:#}")})),
    }
}

/// Compat no-op: hot/end flattens and closes in one step now (#67).
async fn hot_confirm(State(ctx): State<HotCtx>, Path(doc_id): Path<Uuid>) -> Json<Value> {
    let _ = (&ctx, doc_id);
    Json(json!({"ok": true}))
}

async fn hot_status(State(ctx): State<HotCtx>, Path(doc_id): Path<Uuid>) -> Json<Value> {
    if mirror_of(&ctx, doc_id) {
        let Some(ep) = &ctx.endpoint else {
            return Json(json!({"hot": false, "editors": 0}));
        };
        return match crate::fed::hot_status_upstream(ep, &ctx.store, doc_id).await {
            Ok((hot, frozen_epoch, editors)) => {
                Json(json!({"hot": hot, "frozen_epoch": frozen_epoch, "editors": editors}))
            }
            // owner offline etc: not joinable, so not hot
            Err(_) => Json(json!({"hot": false, "editors": 0})),
        };
    }
    let editors = ctx.hot.editors(doc_id);
    let sessions = ctx.hot.sessions.lock().unwrap_or_else(|p| p.into_inner());
    match sessions.get(&doc_id) {
        Some(s) => Json(json!({
            "hot": !s.ending,
            "frozen_epoch": s.frozen_epoch,
            "participants": s.tx.receiver_count(),
            "editors": editors,
        })),
        None => Json(json!({"hot": false, "editors": editors})),
    }
}

#[derive(serde::Deserialize)]
struct EditPing {
    key: Uuid,
}

/// Cold-editor heartbeat. Mirrors relay to the owner so cross-instance
/// concurrent editing also escalates.
async fn editing_ping(
    State(ctx): State<HotCtx>,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<EditPing>,
) -> Json<Value> {
    if mirror_of(&ctx, doc_id) {
        let Some(ep) = &ctx.endpoint else {
            return Json(json!({"editors": 1}));
        };
        return match crate::fed::edit_ping_upstream(ep, &ctx.store, doc_id, req.key).await {
            Ok(editors) => Json(json!({"editors": editors})),
            Err(_) => Json(json!({"editors": 1})),
        };
    }
    Json(json!({"editors": ctx.hot.edit_ping(doc_id, req.key)}))
}

async fn ws_hot(
    State(ctx): State<HotCtx>,
    Path(doc_id): Path<Uuid>,
    upgrade: WebSocketUpgrade,
) -> axum::response::Response {
    upgrade.on_upgrade(move |socket| ws_session(socket, ctx, doc_id))
}

async fn ws_session(mut socket: WebSocket, ctx: HotCtx, doc_id: Uuid) {
    // mirror docs: pure byte pipe to the owner's session over iroh (#66) —
    // the UI speaks y-sync to the owner's daemon through us
    if mirror_of(&ctx, doc_id) {
        let Some(ep) = ctx.endpoint.clone() else {
            socket.send(WsMessage::Close(None)).await.ok();
            return;
        };
        match crate::fed::open_hot_bridge(&ep, &ctx.store, doc_id).await {
            Ok((to_owner, mut from_owner)) => {
                use futures_util::{SinkExt, StreamExt};
                let (mut ws_tx, mut ws_rx) = socket.split();
                let down = tokio::spawn(async move {
                    while let Some(frame) = from_owner.recv().await {
                        if ws_tx.send(WsMessage::Binary(frame.into())).await.is_err() {
                            break;
                        }
                    }
                    ws_tx.send(WsMessage::Close(None)).await.ok();
                });
                while let Some(Ok(msg)) = ws_rx.next().await {
                    match msg {
                        WsMessage::Binary(b) => {
                            if to_owner.send(b.to_vec()).await.is_err() {
                                break;
                            }
                        }
                        WsMessage::Close(_) => break,
                        _ => {}
                    }
                }
                down.abort();
            }
            Err(e) => {
                tracing::debug!(%doc_id, "hot bridge failed: {e:#}");
                socket.send(WsMessage::Close(None)).await.ok();
            }
        }
        return;
    }
    let Some((mut rx, hello)) = ctx.hot.connect(doc_id) else {
        socket.send(WsMessage::Close(None)).await.ok();
        return;
    };
    if socket.send(WsMessage::Binary(hello.into())).await.is_err() {
        return;
    }
    let (mut ws_tx, mut ws_rx) = socket.split();
    use futures_util::{SinkExt, StreamExt};

    let fan_out = tokio::spawn(async move {
        while let Ok(frame) = rx.recv().await {
            if ws_tx.send(WsMessage::Binary(frame.into())).await.is_err() {
                break;
            }
        }
        ws_tx.send(WsMessage::Close(None)).await.ok();
    });

    while let Some(Ok(msg)) = ws_rx.next().await {
        let data = match msg {
            WsMessage::Binary(b) => b,
            WsMessage::Close(_) => break,
            _ => continue,
        };
        if !ctx.hot.handle_frame(doc_id, &data) {
            break;
        }
    }
    fan_out.abort();
}

/// Append every sync Update in the frame to the journal, length-prefixed.
fn journal_updates(journal: &mut std::fs::File, data: &[u8]) {
    use yrs::updates::decoder::DecoderV1;
    let mut decoder = DecoderV1::new(yrs::encoding::read::Cursor::new(data));
    let mut reader = yrs::sync::MessageReader::new(&mut decoder);
    while let Some(Ok(msg)) = reader.next() {
        if let YMessage::Sync(SyncMessage::Update(u) | SyncMessage::SyncStep2(u)) = msg {
            let len = (u.len() as u32).to_le_bytes();
            journal.write_all(&len).ok();
            journal.write_all(&u).ok();
        }
    }
}

/// Awareness updates must reach the other participants; doc observers only
/// cover content updates.
fn rebroadcast_awareness(tx: &broadcast::Sender<Vec<u8>>, data: &[u8]) {
    use yrs::updates::decoder::DecoderV1;
    let mut decoder = DecoderV1::new(yrs::encoding::read::Cursor::new(data));
    let mut reader = yrs::sync::MessageReader::new(&mut decoder);
    while let Some(Ok(msg)) = reader.next() {
        if matches!(msg, YMessage::Awareness(_)) {
            let mut enc = EncoderV1::new();
            msg.encode(&mut enc);
            tx.send(enc.to_vec()).ok();
        }
    }
}

pub fn router(ctx: HotCtx) -> Router {
    Router::new()
        .route("/api/doc/{id}/hot/start", post(hot_start))
        .route("/api/doc/{id}/hot/end", post(hot_end))
        .route("/api/doc/{id}/hot/confirm", post(hot_confirm))
        .route("/api/doc/{id}/hot/status", get(hot_status))
        .route("/api/doc/{id}/editing", post(editing_ping))
        .route("/ws/hot/{id}", any(ws_hot))
        .with_state(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::{BlockStore, BlockType, OpInput, OpKind, PrincipalKind, SqliteStore};
    use yrs::{Text as _, Xml, XmlFragment as _};

    fn scratch_hot() -> HotState {
        let dir = std::env::temp_dir().join(format!("grimoire-hot-test-{}", Uuid::now_v7()));
        HotState::new(dir)
    }

    /// Build the y-prosemirror shape a client would have produced.
    fn seed_session(hot: &HotState, doc_id: Uuid, paragraphs: &[(&str, Option<&str>)]) {
        let sessions = hot.sessions.lock().unwrap();
        let session = sessions.get(&doc_id).unwrap();
        let frag = session.awareness.doc().get_or_insert_xml_fragment("default");
        let mut txn = session.awareness.doc().transact_mut();
        for (i, (text, heading)) in paragraphs.iter().enumerate() {
            let tag = if heading.is_some() { "heading" } else { "paragraph" };
            let el = frag.insert(&mut txn, i as u32, yrs::XmlElementPrelim::empty(tag));
            if let Some(level) = heading {
                el.insert_attribute(&mut txn, "level", *level);
            }
            let t = el.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
            t.insert(&mut txn, 0, text);
        }
    }

    #[test]
    fn flatten_lands_one_commit_and_preserves_unchanged_ids() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let doc = s.create_doc("Doc", None, tom.id).unwrap();
        let keep_id = Uuid::now_v7();
        s.apply(
            doc.id,
            0,
            tom.id,
            vec![
                OpInput {
                    kind: OpKind::Insert {
                        block_id: keep_id,
                        parent_id: None,
                        order_key: "i".into(),
                        block_type: BlockType::Paragraph,
                        content: "unchanged paragraph".into(),
                        refers_to: None,
                    },
                    source_refs: vec![],
                },
                OpInput {
                    kind: OpKind::Insert {
                        block_id: Uuid::now_v7(),
                        parent_id: None,
                        order_key: "r".into(),
                        block_type: BlockType::Paragraph,
                        content: "will be edited".into(),
                        refers_to: None,
                    },
                    source_refs: vec![],
                },
            ],
        )
        .unwrap();
        let store = Arc::new(Mutex::new(s));
        let hot = scratch_hot();
        hot.start(doc.id, 1).unwrap();
        seed_session(
            &hot,
            doc.id,
            &[
                ("unchanged paragraph", None),
                ("edited live in session", None),
                ("Session Notes", Some("2")),
            ],
        );

        let applied = hot.flatten_and_close(&store, doc.id, "test").unwrap();
        assert!(applied >= 2, "expected replace+insert, got {applied}");

        let s = store.lock().unwrap();
        let tree = s.read_doc(doc.id).unwrap();
        assert_eq!(tree.doc.current_epoch, 2); // exactly one commit
        let contents: Vec<_> = tree.roots.iter().map(|n| n.block.content.as_str()).collect();
        assert_eq!(
            contents,
            vec!["unchanged paragraph", "edited live in session", "## Session Notes"]
        );
        // unchanged block kept its id — comment anchors survive
        assert_eq!(tree.roots[0].block.id, keep_id);
        assert!(!hot.is_hot(doc.id));
        assert!(!hot.journal_path(doc.id).exists());
    }

    #[test]
    fn empty_session_never_deletes_content() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let doc = s.create_doc("Doc", None, tom.id).unwrap();
        s.apply(
            doc.id,
            0,
            tom.id,
            vec![OpInput {
                kind: OpKind::Insert {
                    block_id: Uuid::now_v7(),
                    parent_id: None,
                    order_key: "i".into(),
                    block_type: BlockType::Paragraph,
                    content: "precious".into(),
                    refers_to: None,
                },
                source_refs: vec![],
            }],
        )
        .unwrap();
        let store = Arc::new(Mutex::new(s));
        let hot = scratch_hot();
        hot.start(doc.id, 1).unwrap();
        // never seeded: flatten must keep the doc as it was
        let applied = hot.flatten_and_close(&store, doc.id, "test").unwrap();
        assert_eq!(applied, 0);
        let s = store.lock().unwrap();
        let tree = s.read_doc(doc.id).unwrap();
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.doc.current_epoch, 1); // untouched
    }

    #[test]
    fn canvas_session_flattens_maps_to_ks_diagram() {
        use yrs::Map as _;
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let doc = s.create_doc("Canvas", None, tom.id).unwrap();
        let canvas_block = Uuid::now_v7();
        s.apply(
            doc.id,
            0,
            tom.id,
            vec![OpInput {
                kind: OpKind::Insert {
                    block_id: canvas_block,
                    parent_id: None,
                    order_key: "i".into(),
                    block_type: BlockType::CanvasScene,
                    content: r#"{"ks_diagram":{"nodes":[{"id":"a","label":"old"}],"edges":[]}}"#.into(),
                    refers_to: None,
                },
                source_refs: vec![],
            }],
        )
        .unwrap();
        let store = Arc::new(Mutex::new(s));
        let hot = scratch_hot();
        hot.start(doc.id, 1).unwrap();
        // what the client's Y.Map.set(id, JSON.stringify(shape)) produces
        {
            let sessions = hot.sessions.lock().unwrap();
            let session = sessions.get(&doc.id).unwrap();
            let ydoc = session.awareness.doc();
            let nodes = ydoc.get_or_insert_map("canvas_nodes");
            let edges = ydoc.get_or_insert_map("canvas_edges");
            let mut txn = ydoc.transact_mut();
            nodes.insert(&mut txn, "a", r#"{"id":"a","label":"daemon","x":0,"y":0,"shape":"box"}"#);
            nodes.insert(&mut txn, "b", r#"{"id":"b","label":"gate","x":300,"y":0,"shape":"diamond"}"#);
            edges.insert(&mut txn, "e1", r#"{"id":"e1","from":"a","to":"b","arrow":"end"}"#);
        }
        let applied = hot.flatten_and_close(&store, doc.id, "test").unwrap();
        assert_eq!(applied, 1);
        let s = store.lock().unwrap();
        let tree = s.read_doc(doc.id).unwrap();
        assert_eq!(tree.doc.current_epoch, 2);
        let content: serde_json::Value =
            serde_json::from_str(&tree.roots[0].block.content).unwrap();
        let kd = &content["ks_diagram"];
        assert_eq!(kd["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(kd["nodes"][1]["label"], "gate");
        assert_eq!(kd["edges"][0]["from"], "a");
        assert_eq!(tree.roots[0].block.id, canvas_block); // same block, replaced
    }

    #[test]
    fn yrender_marks_lists_and_code() {
        let ydoc = Doc::new();
        let frag = ydoc.get_or_insert_xml_fragment("default");
        {
            let mut txn = ydoc.transact_mut();
            let p = frag.insert(&mut txn, 0, yrs::XmlElementPrelim::empty("paragraph"));
            let t = p.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
            t.insert(&mut txn, 0, "plain ");
            let mut attrs = std::collections::HashMap::new();
            attrs.insert("bold".into(), yrs::Any::Bool(true));
            t.insert_with_attributes(&mut txn, 6, "bold", attrs);

            let code = frag.insert(&mut txn, 1, yrs::XmlElementPrelim::empty("codeBlock"));
            code.insert_attribute(&mut txn, "language", "rust");
            let ct = code.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
            ct.insert(&mut txn, 0, "fn main() {}");

            let list = frag.insert(&mut txn, 2, yrs::XmlElementPrelim::empty("bulletList"));
            let item = list.insert(&mut txn, 0, yrs::XmlElementPrelim::empty("listItem"));
            let ip = item.insert(&mut txn, 0, yrs::XmlElementPrelim::empty("paragraph"));
            let it = ip.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
            it.insert(&mut txn, 0, "first item");
        }
        let txn = ydoc.transact();
        let md = crate::yrender::fragment_to_markdown(&txn, &frag);
        assert_eq!(md, "plain **bold**\n\n```rust\nfn main() {}\n```\n\n- first item");
    }
}
