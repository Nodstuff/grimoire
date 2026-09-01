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
    /// Guard against a session that was ended but never confirmed.
    pub ending: bool,
    _doc_sub: yrs::Subscription,
}

#[derive(Clone, Default)]
pub struct HotState {
    pub sessions: Arc<Mutex<HashMap<Uuid, HotSession>>>,
    pub journal_dir: Arc<std::path::PathBuf>,
}

impl HotState {
    pub fn new(journal_dir: std::path::PathBuf) -> Self {
        std::fs::create_dir_all(&journal_dir).ok();
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            journal_dir: Arc::new(journal_dir),
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

/// End the session: sockets close, the epoch un-freezes so the ending client
/// can land the flatten through the normal propose path. The journal stays
/// until `confirm`.
async fn hot_end(State(ctx): State<HotCtx>, Path(doc_id): Path<Uuid>) -> Json<Value> {
    let mut sessions = ctx
        .hot
        .sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let Some(session) = sessions.get_mut(&doc_id) else {
        return Json(json!({"error": "doc is not hot"}));
    };
    session.ending = true;
    let frozen = session.frozen_epoch;
    // closing the broadcast channel ends every socket's send loop
    let secs = session.started_at.elapsed().as_secs();
    Json(json!({"frozen_epoch": frozen, "session_secs": secs}))
}

/// The flatten landed (or nothing changed): drop the session and journal.
async fn hot_confirm(State(ctx): State<HotCtx>, Path(doc_id): Path<Uuid>) -> Json<Value> {
    let mut sessions = ctx
        .hot
        .sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if sessions.remove(&doc_id).is_none() {
        return Json(json!({"error": "doc is not hot"}));
    }
    drop(sessions);
    std::fs::remove_file(ctx.hot.journal_path(doc_id)).ok();
    Json(json!({"ok": true}))
}

async fn hot_status(State(ctx): State<HotCtx>, Path(doc_id): Path<Uuid>) -> Json<Value> {
    if mirror_of(&ctx, doc_id) {
        let Some(ep) = &ctx.endpoint else {
            return Json(json!({"hot": false}));
        };
        return match crate::fed::hot_status_upstream(ep, &ctx.store, doc_id).await {
            Ok((hot, frozen_epoch)) => Json(json!({"hot": hot, "frozen_epoch": frozen_epoch})),
            // owner offline etc: not joinable, so not hot
            Err(_) => Json(json!({"hot": false})),
        };
    }
    let sessions = ctx.hot.sessions.lock().unwrap_or_else(|p| p.into_inner());
    match sessions.get(&doc_id) {
        Some(s) => Json(json!({
            "hot": !s.ending,
            "frozen_epoch": s.frozen_epoch,
            "participants": s.tx.receiver_count(),
        })),
        None => Json(json!({"hot": false})),
    }
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
        .route("/ws/hot/{id}", any(ws_hot))
        .with_state(ctx)
}
