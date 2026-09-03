//! Hot sessions (#65, ADR 0003): live co-editing.
//!
//! The daemon hosts a yrs doc per hot session and speaks the y-sync protocol
//! over `/ws/hot/{doc}` — wire-compatible with y-websocket clients. While a
//! doc is hot its epoch is FROZEN for content writes: every content-writing
//! surface (api/mcp propose, resolve, gardeners, remote proposals) asks
//! `HotState::assert_cold` first, and the collab session is the only writer.
//! Comments stay allowed (conversation, not content — ADR 0003 §1); they bump
//! the epoch, which is why the flatten diffs against and proposes at the
//! doc's CURRENT epoch, recording the frozen one in provenance.
//!
//! Cool-down: the daemon renders the Yjs doc to markdown (`yrender`) and
//! lands ONE propose through `mddiff::markdown_to_ops_editor` — the editor's
//! view of the doc (frontmatter, comments, canvases excluded, exactly as the
//! UI seeded it), so unchanged blocks keep ids and hidden blocks are never
//! touched. A failed flatten leaves the session hot (not a zombie) so it can
//! be retried; the journal dies only after the commit lands.
//!
//! Crash safety: every incoming update frame is appended (length-prefixed)
//! to a journal beside the db. A daemon restart with a journal present
//! re-hydrates the session — the doc simply stays hot until someone ends it
//! or the idle reaper does.
//!
//! `HotState` is plain state threaded into every surface that needs it
//! (api, mcp, admin/garden, fed) — there is no process-global.

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use crate::store_ext::{blocking, with_store};
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
    /// Set while a flatten is in flight: refuses new frames/joins. Cleared
    /// again if the flatten fails, so the session is never stranded.
    pub ending: bool,
    /// Session = consent: while live, `view` grantees may write too (the
    /// owner opened the doc up). The owner can flip this off for a
    /// "watch only" presentation; `propose` grantees always write.
    pub viewers_write: bool,
    /// Who created the session: `None` = this instance (the owner), `Some`
    /// = the remote peer's pubkey. Only the owner or the starter may end it.
    pub starter: Option<String>,
    /// Everyone who joined: `None` = a local (owner) socket, `Some(pubkey)`
    /// = a remote bridge. Recorded into the flatten's provenance.
    pub participants: std::collections::HashSet<Option<String>>,
    /// Agents that wrote suggestions in this session (room.rs) — named in
    /// the flatten's provenance like any other participant.
    pub agents: std::collections::HashSet<String>,
    /// First journal write failure (disk full, permissions). The session
    /// stays live — the text is in memory and flattens normally — but the
    /// crash-safety guarantee is gone, so the UI is told.
    pub journal_error: Option<String>,
    _doc_sub: yrs::Subscription,
}

/// Bounded fan-in per session: each participant holds a broadcast receiver
/// and (remotely) a QUIC stream; beyond this a session is a broadcast, not
/// a collaboration.
pub const MAX_PARTICIPANTS: usize = 32;
/// Frames a slow subscriber may fall behind before the broadcast channel
/// drops the oldest for it (`Lagged`). A lagged subscriber is resynced with
/// the full doc state (`next_fan_out`) rather than left silently stale.
pub const FAN_OUT_DEPTH: usize = 1024;

/// A live owner-side bridge to a remote participant, registered so a
/// revoke can cut it immediately instead of waiting for the re-auth timer.
pub struct BridgeHandle {
    pub doc: Uuid,
    pub peer: String,
    pub share: Uuid,
    pub cancel: Arc<tokio::sync::Notify>,
}

#[derive(Clone, Default)]
pub struct HotState {
    pub sessions: Arc<Mutex<HashMap<Uuid, HotSession>>>,
    pub journal_dir: Arc<std::path::PathBuf>,
    /// Cold-editor heartbeats (auto-hot, P2.1): doc → editor key → last ping.
    /// Two live keys on a cold doc = concurrent editing = the UIs go live.
    pub editing: Arc<Mutex<HashMap<Uuid, HashMap<Uuid, std::time::Instant>>>>,
    /// Grantee side: the last reason a bridge to the owner's session failed,
    /// per mirror doc — surfaced in hot/status so the UI can say WHY instead
    /// of showing "connecting…" forever. Cleared when a bridge succeeds.
    pub bridge_errors: Arc<Mutex<HashMap<Uuid, String>>>,
    /// Owner side: live bridges by registration id (see `BridgeHandle`).
    pub bridges: Arc<Mutex<HashMap<u64, BridgeHandle>>>,
    /// Bumped whenever a session starts or ends — a cheap "did hotness
    /// change?" signal for the owner-side nudge detector.
    pub generation: Arc<std::sync::atomic::AtomicU64>,
    /// Agents in the room (`room.rs`): per live doc, is the agent thinking,
    /// what did it last say / fail with. Surfaced in hot/status.
    pub agent: Arc<Mutex<HashMap<Uuid, crate::room::AgentStatus>>>,
}

impl HotState {
    pub fn new(journal_dir: std::path::PathBuf) -> Self {
        std::fs::create_dir_all(&journal_dir).ok();
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            journal_dir: Arc::new(journal_dir),
            editing: Arc::new(Mutex::new(HashMap::new())),
            bridge_errors: Arc::new(Mutex::new(HashMap::new())),
            bridges: Arc::new(Mutex::new(HashMap::new())),
            generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn agent_status(&self, doc: Uuid) -> crate::room::AgentStatus {
        self.agent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&doc)
            .cloned()
            .unwrap_or_default()
    }

    /// Mark the agent busy for a doc; false if it already is (one ask at a time).
    pub fn agent_begin(&self, doc: Uuid) -> bool {
        let mut m = self.agent.lock().unwrap_or_else(|p| p.into_inner());
        let st = m.entry(doc).or_default();
        if st.busy {
            return false;
        }
        st.busy = true;
        st.asks += 1;
        st.last_error = None;
        st.last_ok = None;
        true
    }

    pub fn agent_finish(&self, doc: Uuid, result: Result<usize, String>) {
        let mut m = self.agent.lock().unwrap_or_else(|p| p.into_inner());
        let st = m.entry(doc).or_default();
        st.busy = false;
        match result {
            Ok(n) => {
                st.last_error = None;
                if st.last_ok.is_none() {
                    st.last_ok = Some(format!("{n} suggestion{}", if n == 1 { "" } else { "s" }));
                }
            }
            Err(e) => st.last_error = Some(e),
        }
    }

    /// The agent's one-line note to the room (from its reply), if any.
    pub fn set_agent_note(&self, doc: Uuid, note: Option<String>) {
        let mut m = self.agent.lock().unwrap_or_else(|p| p.into_inner());
        m.entry(doc).or_default().last_ok = note;
    }

    /// Current hotness generation (changes on every start/end).
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn bump_generation(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Register a live owner-side bridge; returns the id to unregister with.
    pub fn register_bridge(&self, doc: Uuid, peer: &str, share: Uuid) -> (u64, Arc<tokio::sync::Notify>) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cancel = Arc::new(tokio::sync::Notify::new());
        self.bridges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                id,
                BridgeHandle {
                    doc,
                    peer: peer.to_string(),
                    share,
                    cancel: cancel.clone(),
                },
            );
        (id, cancel)
    }

    pub fn unregister_bridge(&self, id: u64) {
        self.bridges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
    }

    /// Cut every live bridge matching the predicate NOW (revoke). Returns how
    /// many were signalled. The bridge task removes its own registration.
    pub fn drop_bridges_where(&self, pred: impl Fn(&BridgeHandle) -> bool) -> usize {
        let bridges = self.bridges.lock().unwrap_or_else(|p| p.into_inner());
        let mut n = 0;
        for b in bridges.values().filter(|b| pred(b)) {
            tracing::info!(doc = %b.doc, peer = b.peer, share = %b.share, "bridge cut by revoke");
            b.cancel.notify_one();
            n += 1;
        }
        n
    }

    pub fn drop_bridges_for_peer(&self, peer: &str) -> usize {
        self.drop_bridges_where(|b| b.peer == peer)
    }

    pub fn drop_bridges_for_share(&self, share: Uuid) -> usize {
        self.drop_bridges_where(|b| b.share == share)
    }

    /// May this remote peer end the session? Only its starter (the owner
    /// ends locally and is never asked).
    pub fn can_end(&self, doc: Uuid, peer: &str) -> bool {
        let s = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        s.get(&doc)
            .is_some_and(|h| h.starter.as_deref() == Some(peer))
    }

    /// The journal failure for a live session, if any (hot/status reads the
    /// field directly under its own lock; this is the standalone accessor).
    #[cfg(test)]
    pub fn journal_error(&self, doc: Uuid) -> Option<String> {
        let s = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        s.get(&doc).and_then(|h| h.journal_error.clone())
    }

    pub fn set_bridge_error(&self, doc: Uuid, err: Option<String>) {
        let mut m = self.bridge_errors.lock().unwrap_or_else(|p| p.into_inner());
        match err {
            Some(e) => {
                m.insert(doc, e);
            }
            None => {
                m.remove(&doc);
            }
        }
    }

    pub fn bridge_error(&self, doc: Uuid) -> Option<String> {
        self.bridge_errors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&doc)
            .cloned()
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

    /// Is this doc in a live session? Consulted by every content-writing
    /// surface via `assert_cold`.
    pub fn is_hot(&self, doc: Uuid) -> bool {
        let s = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        s.get(&doc).map(|h| !h.ending).unwrap_or(false)
    }

    /// The freeze (ADR 0003 §4): Err with the user-facing reason when the doc
    /// is live. Every content write path calls this before touching the store.
    pub fn assert_cold(&self, doc: Uuid) -> Result<(), String> {
        if self.is_hot(doc) {
            Err("doc is in a live session — edits go through the session; retry after it ends (P2.3)".into())
        } else {
            Ok(())
        }
    }

    /// May `view` grantees write in this live session? None when not hot.
    pub fn viewers_write(&self, doc: Uuid) -> Option<bool> {
        let s = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        s.get(&doc).filter(|h| !h.ending).map(|h| h.viewers_write)
    }

    /// Owner toggle: "everyone can edit" ↔ "watch only". Err when not hot.
    pub fn set_viewers_write(&self, doc: Uuid, enabled: bool) -> Result<bool, String> {
        let mut s = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        match s.get_mut(&doc).filter(|h| !h.ending) {
            Some(h) => {
                h.viewers_write = enabled;
                Ok(enabled)
            }
            None => Err("doc is not in a live session".into()),
        }
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

    /// Create (or join) the session as the owner. Returns true when this call
    /// created it — the creator seeds the Yjs doc from the current content.
    pub fn start(&self, doc_id: Uuid, frozen_epoch: i64) -> std::io::Result<bool> {
        self.start_by(doc_id, frozen_epoch, None)
    }

    /// `start` with the creator recorded: `Some(pubkey)` for a remote peer.
    pub fn start_by(
        &self,
        doc_id: Uuid,
        frozen_epoch: i64,
        starter: Option<&str>,
    ) -> std::io::Result<bool> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = sessions.get(&doc_id) {
            if existing.ending {
                // a flatten is in flight; it either lands (session gone) or
                // fails (session re-opened) — never flip its flag from here
                return Err(std::io::Error::other("session is closing; retry in a moment"));
            }
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
        let (tx, _) = broadcast::channel::<Vec<u8>>(FAN_OUT_DEPTH);
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
                viewers_write: true,
                starter: starter.map(str::to_string),
                participants: std::collections::HashSet::new(),
                agents: std::collections::HashSet::new(),
                journal_error: None,
                _doc_sub: sub,
            },
        );
        self.bump_generation();
        Ok(!had_journal)
    }
}

impl HotState {
    /// Subscribe to a live session: returns the fan-out receiver plus the
    /// y-sync handshake frames to send first. None when the doc isn't hot.
    pub fn connect(&self, doc_id: Uuid) -> Option<(broadcast::Receiver<Vec<u8>>, Vec<u8>)> {
        self.connect_as(doc_id, None).ok()
    }

    /// The next frame for one fan-out subscriber. Before 0.7.2 a subscriber
    /// that fell `FAN_OUT_DEPTH` frames behind got `Lagged` and the fan-out
    /// loop ended: the socket stayed open and simply never received another
    /// edit. Now a lag is logged and answered with a full-state `SyncStep2`
    /// (Yjs applies it idempotently, so the client catches up in one frame);
    /// `None` only when the session is gone.
    pub async fn next_fan_out(
        &self,
        rx: &mut broadcast::Receiver<Vec<u8>>,
        doc_id: Uuid,
        who: &str,
    ) -> Option<Vec<u8>> {
        loop {
            match rx.recv().await {
                Ok(frame) => return Some(frame),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(%doc_id, who, skipped = n, "hot fan-out lagged; resyncing the subscriber");
                    match self.resync_frame(doc_id) {
                        Some(f) => return Some(f),
                        None => continue, // session ending: drain to Closed
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// The whole doc as one `SyncStep2` frame (what a lagged subscriber is
    /// sent). None when the doc is not hot.
    pub fn resync_frame(&self, doc_id: Uuid) -> Option<Vec<u8>> {
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let session = sessions.get(&doc_id)?;
        let update = session
            .awareness
            .doc()
            .transact()
            .encode_state_as_update_v1(&yrs::StateVector::default());
        let mut enc = EncoderV1::new();
        YMessage::Sync(SyncMessage::SyncStep2(update)).encode(&mut enc);
        Some(enc.to_vec())
    }

    /// Test seam: push a raw frame to every subscriber of a session.
    #[cfg(test)]
    pub fn broadcast_raw(&self, doc_id: Uuid, frame: Vec<u8>) -> bool {
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        sessions.get(&doc_id).is_some_and(|s| s.tx.send(frame).is_ok())
    }

    /// `connect` with the participant recorded (`None` = a local owner
    /// socket) and the fan-in cap enforced. Err carries the reason.
    pub fn connect_as(
        &self,
        doc_id: Uuid,
        peer: Option<&str>,
    ) -> Result<(broadcast::Receiver<Vec<u8>>, Vec<u8>), String> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let session = sessions.get_mut(&doc_id).ok_or("doc is not hot")?;
        if session.ending {
            return Err("session is closing".into());
        }
        if session.tx.receiver_count() >= MAX_PARTICIPANTS {
            return Err(format!(
                "session is full ({MAX_PARTICIPANTS} participants)"
            ));
        }
        let mut enc = EncoderV1::new();
        DefaultProtocol
            .start(&session.awareness, &mut enc)
            .map_err(|e| format!("sync handshake failed: {e}"))?;
        session.participants.insert(peer.map(str::to_string));
        Ok((session.tx.subscribe(), enc.to_vec()))
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
        if let Err(e) = journal_updates(&mut session.journal, data)
            && session.journal_error.is_none()
        {
            // once per session: the session stays live (text is in memory
            // and flattens normally) but crash recovery is no longer covered
            tracing::error!(%doc_id, "hot journal write failed; session kept live without crash safety: {e}");
            session.journal_error = Some(e.to_string());
        }
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
    /// LCS-diff it against the editor-visible blocks (mddiff — unchanged
    /// blocks keep ids; frontmatter, comments and canvases untouched), land
    /// ONE propose at the doc's current epoch, drop the session and its
    /// journal. Every ending path — owner end, grantee relay, idle timeout,
    /// recovery — comes through here. On error the session is re-opened
    /// (`ending = false`) and the journal kept, so nothing is stranded.
    pub fn flatten_and_close(
        &self,
        store: &Arc<Mutex<grimoire_store::SqliteStore>>,
        doc_id: Uuid,
        reason: &str,
    ) -> anyhow::Result<usize> {
        match self.flatten_inner(store, doc_id, reason) {
            Ok(n) => Ok(n),
            Err(e) => {
                let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(s) = sessions.get_mut(&doc_id) {
                    s.ending = false; // stays live; retryable (idle reaper too)
                }
                tracing::error!(%doc_id, reason, "flatten failed; session kept live: {e:#}");
                Err(e)
            }
        }
    }

    fn flatten_inner(
        &self,
        store: &Arc<Mutex<grimoire_store::SqliteStore>>,
        doc_id: Uuid,
        reason: &str,
    ) -> anyhow::Result<usize> {
        use grimoire_store::BlockStore;
        // 1. mark ending (stops new frames) and render under the lock
        let (markdown, frozen_epoch, secs, participants) = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let session = sessions
                .get_mut(&doc_id)
                .ok_or_else(|| anyhow::anyhow!("doc is not hot"))?;
            session.ending = true;
            let frag = session.awareness.doc().get_or_insert_xml_fragment("default");
            let txn = session.awareness.doc().transact();
            let md = crate::yrender::fragment_to_markdown(&txn, &frag);
            // the starter was in the room even if their socket never connected
            let mut who = session.participants.clone();
            who.insert(session.starter.clone());
            let agents = session.agents.clone();
            (md, session.frozen_epoch, session.started_at.elapsed().as_secs(), (who, agents))
        };
        let (participants, agents) = participants;
        // who was in the room, by petname (the owner by their own name): the
        // flatten lands under the owner's principal, so this is the only
        // record that a remote peer's keystrokes are in it
        let participants_line = {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            let contacts = s.list_contacts().unwrap_or_default();
            let owner_name = s
                .list_principals()
                .unwrap_or_default()
                .into_iter()
                .find(|p| p.kind == grimoire_store::PrincipalKind::Human)
                .map(|p| p.display_name)
                .unwrap_or_else(|| "owner".into());
            let mut names: Vec<String> = participants
                .iter()
                .map(|p| match p {
                    None => owner_name.clone(),
                    Some(pk) => contacts
                        .iter()
                        .find(|c| &c.pubkey == pk)
                        .map(|c| c.petname.clone())
                        .unwrap_or_else(|| format!("peer {}", pk.chars().take(8).collect::<String>())),
                })
                .collect();
            names.sort();
            names.dedup();
            let mut agents: Vec<String> = agents.iter().map(|a| format!("🌿 {a}")).collect();
            agents.sort();
            names.extend(agents);
            format!("participants: {}", names.join(", "))
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

        // 2. diff + propose OUTSIDE the session lock. Base = the epoch of the
        // tree we diff against (comments may have moved it while hot); the
        // frozen epoch is provenance.
        let provenance = |kind: &str| {
            vec![
                format!("hot-session: {reason}, {secs}s, frozen at epoch {frozen_epoch}"),
                kind.to_string(),
                participants_line.clone(),
            ]
        };
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
                source_refs: provenance("canvas:live"),
            };
            s.propose(doc_id, tree.doc.current_epoch, human.id, vec![op])?;
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
                // the editor's view: hidden blocks (frontmatter, comments,
                // canvases) were never seeded and must never be diffed away
                let mut ops =
                    grimoire_store::mddiff::markdown_to_ops_editor(&tree.roots, &markdown);
                for op in &mut ops {
                    op.source_refs = provenance("text:live");
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
                    s.propose(doc_id, tree.doc.current_epoch, human.id, ops)?;
                    n
                }
            }
        };
        // 3. drop the session (ends every socket/bridge) and the journal
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions.remove(&doc_id);
        }
        self.bump_generation();
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
            let (hot, store) = (hot.clone(), store.clone());
            if let Err(e) = blocking(move || hot.flatten_and_close(&store, doc_id, "idle timeout")).await {
                tracing::error!(%doc_id, "idle flatten failed: {e:#}");
            }
        }
    }
}

/// Which doc an open annotation belongs to — so resolve surfaces can honour
/// the freeze (accepting a red / declining a yellow writes content).
pub fn annotation_doc(store: &grimoire_store::SqliteStore, annotation_id: Uuid) -> Option<Uuid> {
    use grimoire_store::BlockStore;
    store
        .review_queue(None)
        .ok()?
        .into_iter()
        .find(|i| i.annotation.id == annotation_id)
        .map(|i| i.annotation.doc_id)
}

#[derive(Clone)]
pub struct HotCtx {
    pub hot: HotState,
    pub store: Arc<Mutex<grimoire_store::SqliteStore>>,
    /// Federation endpoint for mirror docs (#66): status/start/ws relay to
    /// the owner's session. None = federation disabled.
    pub endpoint: Option<iroh::Endpoint>,
}

async fn mirror_of(ctx: &HotCtx, doc_id: Uuid) -> bool {
    use grimoire_store::BlockStore;
    with_store(&ctx.store, move |s| s.get_mirror(doc_id).ok().flatten().is_some()).await
}

/// `POST /api/doc/{id}/hot/start` body. `base_epoch` is the epoch of the tree
/// the UI will seed the session from; creating a session from any other
/// epoch is refused (`code: "stale_base"`), because the seed becomes the
/// doc at flatten. Joining a live session ignores it.
#[derive(serde::Deserialize, Default)]
struct StartReq {
    #[serde(default)]
    base_epoch: Option<i64>,
}

async fn hot_start(
    State(ctx): State<HotCtx>,
    Path(doc_id): Path<Uuid>,
    body: axum::body::Bytes,
) -> Json<Value> {
    use grimoire_store::BlockStore;
    let req: StartReq = if body.is_empty() {
        StartReq::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return Json(json!({"error": format!("bad request: {e}")})),
        }
    };
    // mirror docs: the session lives on the OWNER's daemon (#66)
    if mirror_of(&ctx, doc_id).await {
        let Some(ep) = &ctx.endpoint else {
            return Json(json!({"error": "federation disabled"}));
        };
        let base = match req.base_epoch {
            Some(b) => b,
            None => {
                with_store(&ctx.store, move |s| {
                    s.get_mirror(doc_id).ok().flatten().map(|m| m.synced_epoch).unwrap_or(-1)
                })
                .await
            }
        };
        return match crate::fed::hot_start_upstream(ep, &ctx.store, doc_id, base).await {
            Ok((frozen_epoch, seed)) => Json(json!({
                "ws": format!("/ws/hot/{doc_id}"),
                "frozen_epoch": frozen_epoch,
                "seed": seed,
            })),
            Err(e) => {
                let stale = e
                    .downcast_ref::<crate::fed::Refusal>()
                    .is_some_and(|r| r.code == crate::fed::RefusalCode::StaleBase);
                if stale {
                    // our copy is behind: pull it now so the UI's retry seeds
                    // from the owner's current text
                    match crate::fed::pull_owner_of(ep, &ctx.store, doc_id).await {
                        Ok(_) => Json(json!({
                            "error": "your copy was behind the owner's — synced; try again",
                            "code": "stale_base",
                        })),
                        Err(pe) => Json(json!({
                            "error": format!("your copy is behind the owner's and syncing failed: {pe:#}"),
                            "code": "stale_base",
                        })),
                    }
                } else {
                    Json(json!({"error": format!("{e:#}")}))
                }
            }
        };
    }
    let frozen_epoch = match with_store(&ctx.store, move |s| s.get_doc(doc_id)).await {
        Ok(d) => d.current_epoch,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    // Creating a session seeds it from the caller's tree; a caller holding
    // an older tree (a save landed between its fetch and this call) would
    // roll that save back at flatten. Refuse; the UI refetches and retries.
    if !ctx.hot.is_hot(doc_id)
        && let Some(base) = req.base_epoch
        && base != frozen_epoch
    {
        return Json(json!({
            "error": format!("the doc moved to epoch {frozen_epoch} while you were at {base}; reloading"),
            "code": "stale_base",
        }));
    }
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
    if mirror_of(&ctx, doc_id).await {
        let Some(ep) = &ctx.endpoint else {
            return Json(json!({"error": "federation disabled"}));
        };
        return match crate::fed::hot_end_upstream(ep, &ctx.store, doc_id).await {
            Ok(applied) => Json(json!({"flattened_ops": applied})),
            Err(e) => Json(json!({"error": format!("{e:#}")})),
        };
    }
    let (hot, store) = (ctx.hot.clone(), ctx.store.clone());
    match blocking(move || hot.flatten_and_close(&store, doc_id, "ended")).await {
        Ok(applied) => Json(json!({"flattened_ops": applied})),
        Err(e) => Json(json!({"error": format!("{e:#}")})),
    }
}

async fn hot_status(State(ctx): State<HotCtx>, Path(doc_id): Path<Uuid>) -> Json<Value> {
    if mirror_of(&ctx, doc_id).await {
        let Some(ep) = &ctx.endpoint else {
            return Json(json!({"hot": false, "editors": 0}));
        };
        return match crate::fed::hot_status_upstream(ep, &ctx.store, doc_id).await {
            Ok((hot, frozen_epoch, editors, can_write)) => {
                let mut v = json!({"hot": hot, "frozen_epoch": frozen_epoch, "editors": editors});
                if let Some(w) = can_write {
                    v["can_write"] = json!(w);
                }
                if let Some(err) = ctx.hot.bridge_error(doc_id) {
                    v["bridge_error"] = json!(err);
                }
                Json(v)
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
            "can_write": true, // owned doc: always writable
            "viewers_write": s.viewers_write,
            "journal_error": s.journal_error,
            "agent": ctx.hot.agent_status(doc_id),
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
    if mirror_of(&ctx, doc_id).await {
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
    if mirror_of(&ctx, doc_id).await {
        let Some(ep) = ctx.endpoint.clone() else {
            socket.send(WsMessage::Close(None)).await.ok();
            return;
        };
        match crate::fed::open_hot_bridge(&ep, &ctx.store, doc_id).await {
            Ok((to_owner, mut from_owner)) => {
                use futures_util::{SinkExt, StreamExt};
                ctx.hot.set_bridge_error(doc_id, None);
                tracing::info!(%doc_id, "hot bridge to owner open");
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
                // the grantee's "text never shows up": make it loud and
                // queryable (hot/status carries bridge_error to the UI)
                tracing::warn!(%doc_id, "hot bridge to owner failed: {e:#}");
                ctx.hot.set_bridge_error(doc_id, Some(format!("{e:#}")));
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

    let fan_hot = ctx.hot.clone();
    let fan_out = tokio::spawn(async move {
        while let Some(frame) = fan_hot.next_fan_out(&mut rx, doc_id, "local socket").await {
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
/// Err on the first failed write (the caller flags the session).
fn journal_updates(journal: &mut std::fs::File, data: &[u8]) -> std::io::Result<()> {
    use yrs::updates::decoder::DecoderV1;
    let mut decoder = DecoderV1::new(yrs::encoding::read::Cursor::new(data));
    let mut reader = yrs::sync::MessageReader::new(&mut decoder);
    while let Some(Ok(msg)) = reader.next() {
        if let YMessage::Sync(SyncMessage::Update(u) | SyncMessage::SyncStep2(u)) = msg {
            // 0.7.2: every join answers our SyncStep1 with a SyncStep2 that
            // is usually EMPTY (the joiner has nothing we lack); journaling
            // those grew the file by a record per join for no recoverable
            // state. An empty v1 update is exactly two zero bytes.
            if update_is_empty(&u) {
                continue;
            }
            let len = (u.len() as u32).to_le_bytes();
            journal.write_all(&len)?;
            journal.write_all(&u)?;
        }
    }
    Ok(())
}

/// A v1 update that carries no structs and no deletions (`[0, 0]`).
pub fn update_is_empty(u: &[u8]) -> bool {
    u.iter().all(|b| *b == 0)
}

/// Strip a client frame down to what a READ-ONLY participant may send:
/// awareness (presence/carets) and SyncStep1 (a state-vector request, so the
/// server replies with the doc). Anything that would change the doc — Update
/// and SyncStep2 — is dropped. Returns None when nothing survives.
pub fn readonly_filter(data: &[u8]) -> Option<Vec<u8>> {
    use yrs::updates::decoder::DecoderV1;
    let mut decoder = DecoderV1::new(yrs::encoding::read::Cursor::new(data));
    let mut reader = yrs::sync::MessageReader::new(&mut decoder);
    let mut enc = EncoderV1::new();
    let mut kept = 0usize;
    while let Some(Ok(msg)) = reader.next() {
        let allowed = matches!(
            msg,
            YMessage::Awareness(_) | YMessage::Sync(SyncMessage::SyncStep1(_))
        );
        if allowed {
            msg.encode(&mut enc);
            kept += 1;
        }
    }
    (kept > 0).then(|| enc.to_vec())
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

#[derive(serde::Deserialize)]
struct ViewersWriteReq {
    enabled: bool,
}

/// Owner toggle for a live session on an OWNED doc: let `view` grantees write
/// ("everyone can edit", the default) or make it "watch only". Mirrors can't
/// toggle — the owner hosts the session.
async fn hot_viewers_write(
    State(ctx): State<HotCtx>,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<ViewersWriteReq>,
) -> Json<Value> {
    if mirror_of(&ctx, doc_id).await {
        return Json(json!({"error": "only the owner can change who may edit a live session"}));
    }
    match ctx.hot.set_viewers_write(doc_id, req.enabled) {
        Ok(v) => Json(json!({"ok": true, "viewers_write": v})),
        Err(e) => Json(json!({"error": e})),
    }
}

#[derive(serde::Deserialize)]
struct AskReq {
    instruction: String,
}

/// Agents in the room: ask the session's agent for suggestions (owner side;
/// the session and the agent both live here). Returns at once; progress and
/// the outcome ride hot/status (`agent`), the suggestions ride the CRDT.
async fn hot_ask(
    State(ctx): State<HotCtx>,
    Path(doc_id): Path<Uuid>,
    Json(req): Json<AskReq>,
) -> Json<Value> {
    if mirror_of(&ctx, doc_id).await {
        return Json(json!({"error": "the room's agent runs on the owner's Grimoire — ask them to invite it"}));
    }
    if req.instruction.trim().is_empty() {
        return Json(json!({"error": "say what you'd like the agent to do"}));
    }
    if !ctx.hot.is_hot(doc_id) {
        return Json(json!({"error": "the doc is not in a live session"}));
    }
    if crate::garden::claude_bin().is_none() {
        return Json(json!({"error": "Claude Code is not installed on this Mac — the room's agent needs it", "code": "no_claude"}));
    }
    if !ctx.hot.agent_begin(doc_id) {
        return Json(json!({"error": "the agent is still working on the last ask", "code": "agent_busy"}));
    }
    let hot = ctx.hot.clone();
    let store = ctx.store.clone();
    tokio::spawn(async move {
        let res = crate::room::ask(hot.clone(), store, doc_id, req.instruction).await;
        match &res {
            Err(e) => tracing::warn!(%doc_id, "room agent ask failed: {e}"),
            Ok(n) => tracing::info!(%doc_id, landed = n, "room agent suggestions landed"),
        }
        hot.agent_finish(doc_id, res);
    });
    Json(json!({"ok": true}))
}

pub fn router(ctx: HotCtx) -> Router {
    Router::new()
        .route("/api/doc/{id}/hot/ask", post(hot_ask))
        .route("/api/doc/{id}/hot/start", post(hot_start))
        .route("/api/doc/{id}/hot/end", post(hot_end))
        .route("/api/doc/{id}/hot/viewers_write", post(hot_viewers_write))
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
    fn flatten_preserves_frontmatter_and_only_diffs_editor_blocks() {
        // C1: the UI seeds the session WITHOUT frontmatter/comment/canvas
        // blocks; the flatten must diff only those same editor blocks, so a
        // frontmatter block and a comment survive untouched.
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let doc = s.create_doc("Doc", None, tom.id).unwrap();
        let fm = Uuid::now_v7();
        let para = Uuid::now_v7();
        s.apply(
            doc.id,
            0,
            tom.id,
            vec![
                OpInput {
                    kind: OpKind::Insert {
                        block_id: fm,
                        parent_id: None,
                        order_key: "i".into(),
                        block_type: BlockType::Code,
                        content: "---\ntags:\n  - keep\n---".into(),
                        refers_to: None,
                    },
                    source_refs: vec![],
                },
                OpInput {
                    kind: OpKind::Insert {
                        block_id: para,
                        parent_id: None,
                        order_key: "r".into(),
                        block_type: BlockType::Paragraph,
                        content: "body".into(),
                        refers_to: None,
                    },
                    source_refs: vec![],
                },
            ],
        )
        .unwrap();
        // a comment anchored to the paragraph must also survive
        s.add_comment(para, tom.id, "a note", None).unwrap();
        let tags_before = s.list_tags().unwrap();
        assert_eq!(tags_before, vec![("keep".to_string(), 1)]);

        let store = Arc::new(Mutex::new(s));
        let hot = scratch_hot();
        let base = { store.lock().unwrap().get_doc(doc.id).unwrap().current_epoch };
        hot.start(doc.id, base).unwrap();
        // the editor only ever saw the one paragraph
        seed_session(&hot, doc.id, &[("body edited", None)]);
        let applied = hot.flatten_and_close(&store, doc.id, "test").unwrap();
        assert_eq!(applied, 1, "one replace of the paragraph, nothing else");

        let s = store.lock().unwrap();
        let tree = s.read_doc(doc.id).unwrap();
        // frontmatter block still there, same id, same content
        let fm_block = tree.roots.iter().find(|n| n.block.id == fm).expect("frontmatter kept");
        assert!(fm_block.block.content.starts_with("---"));
        assert_eq!(s.list_tags().unwrap(), tags_before, "tags survive");
        // paragraph edited in place, same id (comment anchor survives)
        let para_block = tree.roots.iter().find(|n| n.block.id == para).expect("paragraph kept");
        assert_eq!(para_block.block.content, "body edited");
        assert_eq!(s.list_comments(para).unwrap().len(), 1, "comment survives");
    }

    #[test]
    fn assert_cold_gates_only_while_live() {
        let hot = scratch_hot();
        let doc = Uuid::now_v7();
        assert!(hot.assert_cold(doc).is_ok());
        hot.start(doc, 0).unwrap();
        assert!(hot.assert_cold(doc).is_err());
    }

    #[test]
    fn failed_flatten_keeps_session_live_not_zombie() {
        // a canvas session on a doc with NO canvas block makes the flatten
        // bail (M1): the session must stay hot and retryable, not stranded
        // with ending=true (which would leave the doc unfrozen but unjoinable)
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
                    content: "prose, not a canvas".into(),
                    refers_to: None,
                },
                source_refs: vec![],
            }],
        )
        .unwrap();
        let store = Arc::new(Mutex::new(s));
        let hot = scratch_hot();
        hot.start(doc.id, 1).unwrap();
        // seed canvas maps → flatten takes the canvas branch → no canvas block → bail
        {
            use yrs::Map as _;
            let sessions = hot.sessions.lock().unwrap();
            let ydoc = sessions.get(&doc.id).unwrap().awareness.doc();
            let nodes = ydoc.get_or_insert_map("canvas_nodes");
            let mut txn = ydoc.transact_mut();
            nodes.insert(&mut txn, "a", r#"{"id":"a","label":"x"}"#);
        }
        assert!(hot.flatten_and_close(&store, doc.id, "test").is_err());
        // NOT a zombie: still hot, still joinable, journal intact
        assert!(hot.is_hot(doc.id));
        assert!(hot.connect(doc.id).is_some());
        assert!(hot.journal_path(doc.id).exists());
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

    /// Decode a y-sync frame into its message variants (for asserting what a
    /// filter kept).
    fn frame_kinds(data: &[u8]) -> Vec<&'static str> {
        use yrs::updates::decoder::DecoderV1;
        let mut decoder = DecoderV1::new(yrs::encoding::read::Cursor::new(data));
        let mut reader = yrs::sync::MessageReader::new(&mut decoder);
        let mut out = Vec::new();
        while let Some(Ok(msg)) = reader.next() {
            out.push(match msg {
                YMessage::Awareness(_) => "awareness",
                YMessage::Sync(SyncMessage::SyncStep1(_)) => "step1",
                YMessage::Sync(SyncMessage::SyncStep2(_)) => "step2",
                YMessage::Sync(SyncMessage::Update(_)) => "update",
                _ => "other",
            });
        }
        out
    }

    fn awareness_msg() -> YMessage {
        let mut a = Awareness::new(Doc::new());
        a.set_local_state(serde_json::json!({"user": {"name": "viewer"}}).to_string()).unwrap();
        YMessage::Awareness(a.update().unwrap())
    }

    #[test]
    fn readonly_filter_drops_content_writes_keeps_presence_and_sync_requests() {
        // a viewer frame carrying presence + a content Update + a state request
        let mut enc = EncoderV1::new();
        awareness_msg().encode(&mut enc);
        YMessage::Sync(SyncMessage::Update(vec![1, 2, 3])).encode(&mut enc);
        YMessage::Sync(SyncMessage::SyncStep1(yrs::StateVector::default())).encode(&mut enc);
        YMessage::Sync(SyncMessage::SyncStep2(vec![4, 5])).encode(&mut enc);
        let mixed = enc.to_vec();
        assert_eq!(frame_kinds(&mixed), vec!["awareness", "update", "step1", "step2"]);

        let filtered = readonly_filter(&mixed).expect("presence + step1 survive");
        assert_eq!(frame_kinds(&filtered), vec!["awareness", "step1"], "writes dropped");

        // a frame that is ONLY content writes yields nothing at all
        let mut enc = EncoderV1::new();
        YMessage::Sync(SyncMessage::Update(vec![9])).encode(&mut enc);
        YMessage::Sync(SyncMessage::SyncStep2(vec![9])).encode(&mut enc);
        assert!(readonly_filter(&enc.to_vec()).is_none());

        // presence alone passes untouched
        let mut enc = EncoderV1::new();
        awareness_msg().encode(&mut enc);
        assert_eq!(frame_kinds(&readonly_filter(&enc.to_vec()).unwrap()), vec!["awareness"]);
    }

    #[test]
    fn readonly_participant_cannot_change_the_session_doc() {
        use yrs::ReadTxn as _;
        // end-to-end at the session layer: a viewer's Update, once filtered,
        // must leave the hot doc's content untouched
        let hot = scratch_hot();
        let doc_id = Uuid::now_v7();
        hot.start(doc_id, 0).unwrap();
        seed_session(&hot, doc_id, &[("original", None)]);
        let before = {
            let s = hot.sessions.lock().unwrap();
            let sess = s.get(&doc_id).unwrap();
            let frag = sess.awareness.doc().get_or_insert_xml_fragment("default");
            crate::yrender::fragment_to_markdown(&sess.awareness.doc().transact(), &frag)
        };
        // craft a real Yjs update that would append text, as a client would
        let attacker = Doc::new();
        let upd = {
            let frag = attacker.get_or_insert_xml_fragment("default");
            let mut txn = attacker.transact_mut();
            let p = frag.insert(&mut txn, 0, yrs::XmlElementPrelim::empty("paragraph"));
            let t = p.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
            t.insert(&mut txn, 0, "INJECTED");
            drop(txn);
            attacker.transact().encode_state_as_update_v1(&yrs::StateVector::default())
        };
        let mut enc = EncoderV1::new();
        YMessage::Sync(SyncMessage::Update(upd)).encode(&mut enc);
        let frame = enc.to_vec();
        // through the read-only filter: nothing to apply
        assert!(readonly_filter(&frame).is_none());
        // (sanity: unfiltered, the same frame WOULD change the doc)
        assert!(hot.handle_frame(doc_id, &frame));
        let after_unfiltered = {
            let s = hot.sessions.lock().unwrap();
            let sess = s.get(&doc_id).unwrap();
            let frag = sess.awareness.doc().get_or_insert_xml_fragment("default");
            crate::yrender::fragment_to_markdown(&sess.awareness.doc().transact(), &frag)
        };
        assert_ne!(before, after_unfiltered, "unfiltered update changes the doc");
        assert!(after_unfiltered.contains("INJECTED"));
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

#[cfg(test)]
mod crash_tests {
    use super::*;
    use grimoire_store::{BlockStore, BlockType, OpInput, OpKind, PrincipalKind, SqliteStore};
    use yrs::{ReadTxn, Text as _, XmlFragment as _};

    /// Crash-mid-flatten: the propose committed but the process died before
    /// the session/journal were dropped. On restart the journal is replayed,
    /// the doc is hot again with the same text, and the second flatten is a
    /// no-op — no duplicate paragraphs, no lost text, journal gone.
    #[test]
    fn journal_recovery_after_a_crash_between_commit_and_cleanup_is_idempotent() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let doc = s.create_doc("Doc", None, tom.id).unwrap();
        s.apply(doc.id, 0, tom.id, vec![OpInput {
            kind: OpKind::Insert {
                block_id: Uuid::now_v7(), parent_id: None, order_key: "a".into(),
                block_type: BlockType::Paragraph, content: "before".into(), refers_to: None,
            },
            source_refs: vec![],
        }]).unwrap();
        let store = Arc::new(Mutex::new(s));
        let dir = std::env::temp_dir().join(format!("grimoire-hot-crash-{}", Uuid::now_v7()));
        let hot = HotState::new(dir.clone());
        hot.start(doc.id, 1).unwrap();

        // a client frame arrives (journaled) — the live text
        let frame = {
            let ydoc = Doc::new();
            let frag = ydoc.get_or_insert_xml_fragment("default");
            {
                let mut txn = ydoc.transact_mut();
                let el = frag.insert(&mut txn, 0, yrs::XmlElementPrelim::empty("paragraph"));
                let t = el.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
                t.insert(&mut txn, 0, "typed live");
            }
            let update = ydoc.transact().encode_state_as_update_v1(&yrs::StateVector::default());
            let mut enc = EncoderV1::new();
            YMessage::Sync(SyncMessage::Update(update)).encode(&mut enc);
            enc.to_vec()
        };
        assert!(hot.handle_frame(doc.id, &frame));
        assert!(hot.journal_path(doc.id).exists());

        // flatten commits…
        let applied = hot.flatten_and_close(&store, doc.id, "ended").unwrap();
        assert!(applied >= 1);
        let after_first = store.lock().unwrap().read_doc(doc.id).unwrap();
        assert_eq!(after_first.doc.current_epoch, 2);
        let texts: Vec<_> = after_first.roots.iter().map(|n| n.block.content.clone()).collect();
        assert_eq!(texts, vec!["typed live"]);

        // …but "the process died" before cleanup: put the journal back as it
        // was (append-only log of the same frames) and restart the daemon
        {
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(hot.journal_path(doc.id)).unwrap();
            journal_updates(&mut f, &frame).unwrap();
        }
        let hot2 = HotState::new(dir);
        hot2.recover(&store);
        assert!(hot2.is_hot(doc.id), "recovered session is live again");
        assert_eq!(hot2.status(doc.id).1, Some(2), "frozen at the post-commit epoch");

        // the second flatten (idle reaper / owner end) changes nothing
        let applied = hot2.flatten_and_close(&store, doc.id, "recovered").unwrap();
        assert_eq!(applied, 0);
        let s = store.lock().unwrap();
        let tree = s.read_doc(doc.id).unwrap();
        assert_eq!(tree.doc.current_epoch, 2, "no second commit");
        let texts: Vec<_> = tree.roots.iter().map(|n| n.block.content.clone()).collect();
        assert_eq!(texts, vec!["typed live"]);
        assert!(!hot2.is_hot(doc.id));
        assert!(!hot2.journal_path(doc.id).exists());
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;
    use grimoire_store::{BlockStore, BlockType, OpInput, OpKind, PrincipalKind, SqliteStore};
    use yrs::{ReadTxn, Text as _, XmlFragment as _};

    fn scratch() -> HotState {
        HotState::new(std::env::temp_dir().join(format!("grimoire-hot-hard-{}", Uuid::now_v7())))
    }

    fn update_frame(text: &str) -> Vec<u8> {
        let ydoc = Doc::new();
        let frag = ydoc.get_or_insert_xml_fragment("default");
        {
            let mut txn = ydoc.transact_mut();
            let el = frag.insert(&mut txn, 0, yrs::XmlElementPrelim::empty("paragraph"));
            let t = el.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
            t.insert(&mut txn, 0, text);
        }
        let update = ydoc.transact().encode_state_as_update_v1(&yrs::StateVector::default());
        let mut enc = EncoderV1::new();
        YMessage::Sync(SyncMessage::Update(update)).encode(&mut enc);
        enc.to_vec()
    }

    #[test]
    fn participant_cap_refuses_the_thirty_third_connection_with_a_reason() {
        let hot = scratch();
        let doc = Uuid::now_v7();
        hot.start(doc, 0).unwrap();
        let mut held = Vec::new();
        for i in 0..MAX_PARTICIPANTS {
            let (rx, _) = hot.connect_as(doc, Some(&format!("peer{i}"))).unwrap();
            held.push(rx);
        }
        let err = hot.connect_as(doc, Some("late")).unwrap_err();
        assert!(err.contains("full"), "{err}");
        // a participant leaving frees a slot
        held.pop();
        assert!(hot.connect_as(doc, Some("late")).is_ok());
        // not hot → its own reason
        assert_eq!(hot.connect_as(Uuid::now_v7(), None).unwrap_err(), "doc is not hot");
    }

    #[test]
    fn only_the_starter_may_end_a_remote_started_session() {
        let hot = scratch();
        let doc = Uuid::now_v7();
        assert!(hot.start_by(doc, 0, Some("alice")).unwrap());
        assert!(hot.can_end(doc, "alice"));
        assert!(!hot.can_end(doc, "bob"));
        // owner-started: no remote peer may end it
        let doc2 = Uuid::now_v7();
        hot.start(doc2, 0).unwrap();
        assert!(!hot.can_end(doc2, "alice"));
        // joining does not change the starter
        assert!(!hot.start_by(doc, 0, Some("bob")).unwrap());
        assert!(hot.can_end(doc, "alice"));
        assert!(!hot.can_end(doc, "bob"));
    }

    #[test]
    fn revoke_cuts_registered_bridges_by_peer_or_share_and_generation_tracks_sessions() {
        let hot = scratch();
        let doc = Uuid::now_v7();
        let share_a = Uuid::now_v7();
        let share_b = Uuid::now_v7();
        let g0 = hot.generation();
        hot.start(doc, 0).unwrap();
        assert_eq!(hot.generation(), g0 + 1, "start bumps");
        let (id1, c1) = hot.register_bridge(doc, "alice", share_a);
        let (_id2, c2) = hot.register_bridge(doc, "bob", share_b);
        let (_id3, c3) = hot.register_bridge(doc, "alice", share_b);
        assert_eq!(hot.drop_bridges_for_peer("alice"), 2);
        assert_eq!(hot.drop_bridges_for_share(share_b), 2);
        assert_eq!(hot.drop_bridges_for_peer("nobody"), 0);
        // the notifies are armed for their tasks
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(50), c1.notified()).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_millis(50), c2.notified()).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_millis(50), c3.notified()).await.unwrap();
        });
        hot.unregister_bridge(id1);
        assert_eq!(hot.drop_bridges_for_share(share_a), 0, "unregistered bridges are gone");
    }

    #[test]
    fn journal_write_failure_flags_the_session_once_and_keeps_it_live() {
        let hot = scratch();
        let doc = Uuid::now_v7();
        hot.start(doc, 0).unwrap();
        // swap the journal for a read-only handle: every write fails
        {
            let mut sessions = hot.sessions.lock().unwrap();
            let s = sessions.get_mut(&doc).unwrap();
            s.journal = std::fs::File::open(hot.journal_path(doc)).unwrap();
        }
        assert!(hot.handle_frame(doc, &update_frame("one")));
        let first = hot.journal_error(doc).expect("flagged");
        assert!(hot.handle_frame(doc, &update_frame("two")), "session stays live");
        assert_eq!(hot.journal_error(doc).as_deref(), Some(first.as_str()), "flag set once");
        assert!(hot.is_hot(doc));
        // the text is in memory regardless
        let sessions = hot.sessions.lock().unwrap();
        let s = sessions.get(&doc).unwrap();
        let frag = s.awareness.doc().get_or_insert_xml_fragment("default");
        let txn = s.awareness.doc().transact();
        assert_eq!(frag.len(&txn), 2);
    }

    #[test]
    fn flatten_records_who_was_in_the_room() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let doc = s.create_doc("Doc", None, tom.id).unwrap();
        s.apply(doc.id, 0, tom.id, vec![OpInput {
            kind: OpKind::Insert {
                block_id: Uuid::now_v7(), parent_id: None, order_key: "a".into(),
                block_type: BlockType::Paragraph, content: "before".into(), refers_to: None,
            },
            source_refs: vec![],
        }]).unwrap();
        let alice = s.pair_contact(&"ab".repeat(32), "alice").unwrap();
        let store = Arc::new(Mutex::new(s));
        let hot = scratch();
        // alice started it remotely; tom (local socket) and an unknown peer joined
        hot.start_by(doc.id, 1, Some(&alice.pubkey)).unwrap();
        hot.connect_as(doc.id, None).unwrap();
        hot.connect_as(doc.id, Some(&"cd".repeat(32))).unwrap();
        assert!(hot.handle_frame(doc.id, &update_frame("typed together")));
        hot.flatten_and_close(&store, doc.id, "ended").unwrap();
        let s = store.lock().unwrap();
        let ops = s.ops_since(doc.id, 1).unwrap();
        assert!(!ops.is_empty());
        let refs = &ops[0].source_refs;
        let line = refs.iter().find(|r| r.starts_with("participants:")).expect("participants ref");
        assert_eq!(line, "participants: alice, peer cdcdcdcd, tom");
        assert!(refs.iter().any(|r| r.starts_with("hot-session: ended")));
    }
}
