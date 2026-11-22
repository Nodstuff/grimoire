//! What the hub shows of a fleet: the shapes the viewer routes serve, the
//! order sessions are listed in, the changes a stream carries, and what an
//! alert says.

use serde::Serialize;
use std::sync::Arc;
use taisce_store::fabric::{FabricSession, SessionStatus};
use uuid::Uuid;

/// What a person is told about. The first three are states a session comes
/// to (and index a session's alert clocks); the rest are things waiting for
/// the person (ADR 0008 §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    NeedsYou,
    Stalled,
    Finished,
    /// A guardrail holds a command.
    Approval,
    /// An agent asked the person something.
    Question,
    /// An idle session with a full context has mail: wake it?
    WakeOffer,
    /// Two sessions reached the pair limit: continue?
    PairPaused,
    /// An ask waits for a session that is asleep.
    AskWaiting,
    /// An order was refused or failed.
    OrderRefused,
}

impl AlertKind {
    /// Does arriving in `next` from `prev` (None = first sight) warrant an
    /// alert? Needing the person and stalling always do. Finishing does
    /// only at the end of a turn, so a session seen finished for the first
    /// time, or flipping between finished and idle, says nothing. A state
    /// this build does not know never alerts.
    pub fn on(prev: Option<SessionStatus>, next: SessionStatus) -> Option<AlertKind> {
        use SessionStatus::*;
        match next {
            NeedsYou => Some(AlertKind::NeedsYou),
            Stalled => Some(AlertKind::Stalled),
            Finished if matches!(prev, Some(Working | Stalled | WaitingOnPeer | NeedsYou)) => Some(AlertKind::Finished),
            Finished | Unknown | Working | WaitingOnPeer | Idle | Asleep | Exited => None,
        }
    }

    /// The session-state alerts, which keep a clock each per session.
    pub fn clock(self) -> Option<usize> {
        match self {
            AlertKind::NeedsYou => Some(0),
            AlertKind::Stalled => Some(1),
            AlertKind::Finished => Some(2),
            _ => None,
        }
    }

    /// The notification category the app registers actions for.
    pub fn category(self) -> &'static str {
        match self {
            AlertKind::NeedsYou | AlertKind::Stalled => "FABRIC_SESSION",
            AlertKind::Finished => "FABRIC_FINISHED",
            AlertKind::Approval => "FABRIC_APPROVAL",
            AlertKind::Question => "FABRIC_QUESTION",
            AlertKind::WakeOffer => "FABRIC_WAKE",
            AlertKind::PairPaused => "FABRIC_PAIR",
            AlertKind::AskWaiting => "FABRIC_ASK",
            AlertKind::OrderRefused => "FABRIC_ORDER",
        }
    }

    /// What a notification says.
    pub fn words(self) -> &'static str {
        match self {
            AlertKind::NeedsYou => "Needs you",
            AlertKind::Stalled => "Stalled",
            AlertKind::Finished => "Finished",
            AlertKind::Approval => "Needs your approval",
            AlertKind::Question => "Has a question",
            AlertKind::WakeOffer => "Has mail: wake it?",
            AlertKind::PairPaused => "Agents paused: continue?",
            AlertKind::AskWaiting => "An ask waits for it",
            AlertKind::OrderRefused => "Your command was not carried out",
        }
    }
}

/// A session came to need its person: what a visible push says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAlert {
    pub user: Uuid,
    pub runtime_id: Uuid,
    pub session_id: Uuid,
    pub address: String,
    pub kind: AlertKind,
    /// What the notification may say beyond the address and the kind: the
    /// last line, the command, the question. None when the project's
    /// notifications are private, or there is nothing to say.
    pub preview: Option<String>,
    /// An attention item (approval, question, wake offer, …): its id.
    pub request_id: Option<String>,
    /// A question's choices `[{id, label}]`, for the notification's buttons
    /// (empty when private).
    pub choices: serde_json::Value,
    /// An order refused or failed: its id.
    pub order_id: Option<String>,
    /// A finished turn: every session of the fleet now finished, for the
    /// one quiet notification that says them all.
    pub finished: Vec<String>,
    /// A turn the person started from the phone: told even while they are
    /// at the Mac.
    pub from_phone: bool,
    /// The session's project (for private notifications, whether or not
    /// the fleet still holds the session).
    pub project: String,
}

impl SessionAlert {
    pub fn new(user: Uuid, runtime_id: Uuid, session_id: Uuid, address: &str, kind: AlertKind) -> Self {
        SessionAlert {
            user,
            runtime_id,
            session_id,
            address: address.to_string(),
            kind,
            preview: None,
            request_id: None,
            choices: serde_json::json!([]),
            order_id: None,
            finished: vec![],
            from_phone: false,
            project: String::new(),
        }
    }
}

/// Where alerts go (APNs in the daemon; a recorder in tests).
pub type Alerter = Arc<dyn Fn(SessionAlert) + Send + Sync>;

/// A runtime as the viewer routes show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuntimeView {
    pub id: Uuid,
    pub name: String,
    pub created_at: i64,
    /// Its stream is up right now.
    pub online: bool,
    /// `online`; `idle`: its last stream ended because the runtime closed
    /// it (the Mac is awake with nothing to report); `offline`: it dropped,
    /// went silent, or never connected. Never an alert.
    pub connection: &'static str,
    /// When its stream was last up: now, while it is.
    pub last_seen_at: Option<i64>,
    pub version: String,
    /// Headroom per Claude account, as last reported.
    pub accounts: serde_json::Value,
    pub accounts_at: Option<i64>,
    /// Hold all is on.
    pub held: bool,
    pub held_since: Option<i64>,
    /// The Settings version in force there.
    pub settings_version: u64,
    /// The person is at that Mac.
    pub present: bool,
    /// The runtime's signing key, base64 (null before it said).
    pub public_key: Option<String>,
    pub devices: Vec<DeviceInfo>,
}

/// A session as the viewer routes show it. Whether it is reachable is its
/// runtime's `online`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionView {
    pub id: Uuid,
    pub runtime_id: Uuid,
    pub address: String,
    pub fabric: String,
    pub project: String,
    pub worktree: String,
    pub branch: String,
    pub account: String,
    pub state: SessionStatus,
    pub tool: String,
    pub note: String,
    pub last_line: String,
    pub context_used_pct: Option<u8>,
    pub since: i64,
    pub updated_at: i64,
    /// Snoozed by its person until then: no alert is sent for it before
    /// that. null = not snoozed.
    pub quiet_until: Option<i64>,
    /// The person follows it: every finished turn alerts.
    pub following: bool,
    pub fabric_id: String,
    pub turn_id: String,
    pub queue: Vec<String>,
    pub queue_paused: String,
    /// Its project has Text off (nothing of its text is stored).
    pub text_off: bool,
}

impl SessionView {
    pub fn of(s: &FabricSession, text_off: bool) -> Self {
        SessionView { text_off, ..Self::from(s) }
    }
}

impl From<&FabricSession> for SessionView {
    fn from(s: &FabricSession) -> Self {
        Self {
            id: s.id,
            runtime_id: s.runtime_id,
            address: s.address.clone(),
            fabric: s.fabric.clone(),
            project: s.project.clone(),
            worktree: s.worktree.clone(),
            branch: s.branch.clone(),
            account: s.account.clone(),
            state: s.state,
            tool: s.tool.clone(),
            note: s.note.clone(),
            last_line: s.last_line.clone(),
            context_used_pct: s.context_used_pct,
            since: s.since,
            updated_at: s.updated_at,
            quiet_until: s.quiet_until,
            following: s.following,
            fabric_id: s.fabric_id.clone(),
            turn_id: s.turn_id.clone(),
            queue: s.queue.clone(),
            queue_paused: s.queue_paused.clone(),
            text_off: false,
        }
    }
}

/// One person's whole fleet: runtimes oldest first, sessions sorted by how
/// much each needs its person.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FleetView {
    pub rev: u64,
    pub runtimes: Vec<RuntimeView>,
    pub sessions: Vec<SessionView>,
    /// Open attention items, newest first.
    pub attention: Vec<AttentionView>,
}

impl FleetView {
    pub fn empty() -> Self {
        FleetView { rev: 0, runtimes: vec![], sessions: vec![], attention: vec![] }
    }
}

/// Where a state sorts in the fleet: what needs the person first. A state
/// this build does not know sits beside working.
pub fn attention_rank(state: SessionStatus) -> u8 {
    match state {
        SessionStatus::NeedsYou => 0,
        SessionStatus::Stalled => 1,
        SessionStatus::Finished => 2,
        SessionStatus::WaitingOnPeer => 3,
        SessionStatus::Working => 4,
        SessionStatus::Unknown => 5,
        SessionStatus::Idle => 6,
        SessionStatus::Asleep => 7,
        SessionStatus::Exited => 8,
    }
}

/// By attention, then the most recent change first, then by address.
pub fn sort_sessions(sessions: &mut [SessionView]) {
    sessions.sort_by(|a, b| {
        attention_rank(a.state)
            .cmp(&attention_rank(b.state))
            .then(b.since.cmp(&a.since))
            .then_with(|| a.address.cmp(&b.address))
            .then(a.id.cmp(&b.id))
    });
}

/// One change to a fleet.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Session(SessionView),
    SessionGone { runtime_id: Uuid, id: Uuid },
    Runtime(RuntimeView),
    /// The runtime was removed, and its sessions with it.
    RuntimeGone { id: Uuid },
    Turn(TurnView),
    Attention(AttentionView),
    AttentionCleared { id: String, runtime_id: Uuid, session_id: Uuid, resolution: String },
    Order(OrderView),
    /// The settings view, whole.
    Settings(serde_json::Value),
    Message(MessageView),
    Conversation(ConversationView),
    Pairing(PairingView),
    Project(ProjectView),
}

/// A [`Change`] to one person's fleet, which is at `rev` once it is
/// applied. It names no person: each fleet publishes on a channel of its
/// own, so a subscriber only ever holds its own person's events.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetEvent {
    pub rev: u64,
    pub change: Change,
}

// ---------------------------------------------------------------------
// Slices 2-5 (ADR 0008 §11): the shapes the new routes and stream events
// carry. Bytes are standard base64; a field with nothing is null.
// ---------------------------------------------------------------------

use base64::Engine as _;
use taisce_store::fabric::{
    FabricAttention, FabricConversation, FabricMessage, FabricOrder, FabricPairing, FabricProject, FabricTurn, Party,
};

pub fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn json_or(text: &str, empty: serde_json::Value) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or(empty)
}

/// One device paired with a runtime, as its RuntimeStatus said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub fingerprint: String,
    pub name: String,
    /// 65 bytes, base64.
    pub public_key: String,
    pub paired_at: i64,
    pub counter: u64,
}

/// What a runtime last said of itself (RuntimeStatus), as the hub keeps it
/// (`fabric_runtimes.status`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RuntimeInfo {
    /// 65 bytes, base64; empty before the first report.
    pub public_key: String,
    pub held: bool,
    pub held_since: Option<i64>,
    pub settings_version: u64,
    pub present: bool,
    pub devices: Vec<DeviceInfo>,
}

impl RuntimeInfo {
    pub fn device(&self, id: &str) -> Option<&DeviceInfo> {
        self.devices.iter().find(|d| d.device_id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnView {
    pub runtime_id: Uuid,
    pub session_id: Uuid,
    pub address: String,
    pub turn_id: String,
    pub prompt: String,
    pub reply: String,
    pub tool_count: u32,
    pub outcome: serde_json::Value,
    pub started: i64,
    pub ended: i64,
    pub truncated: bool,
    pub context_used_pct: Option<u8>,
    pub end: String,
    pub source: String,
    pub order_id: Option<String>,
    pub text_off: bool,
    /// The server's insertion sequence (`after_seq` / `since_seq`).
    pub seq: i64,
}

impl From<&FabricTurn> for TurnView {
    fn from(t: &FabricTurn) -> Self {
        TurnView {
            runtime_id: t.runtime_id,
            session_id: t.session_id,
            address: t.address.clone(),
            turn_id: t.turn_id.clone(),
            prompt: t.prompt.clone(),
            reply: t.reply.clone(),
            tool_count: t.tool_count,
            outcome: json_or(&t.outcome, serde_json::json!({})),
            started: t.started,
            ended: t.ended,
            truncated: t.truncated,
            context_used_pct: t.context_used_pct,
            end: t.end_kind.clone(),
            source: t.source.clone(),
            order_id: t.order_id.clone(),
            text_off: t.text_off,
            seq: t.seq,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SignedView {
    pub request: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttentionView {
    pub id: String,
    pub source: String,
    pub runtime_id: Uuid,
    pub session_id: Uuid,
    pub address: String,
    pub kind: String,
    pub text: String,
    pub choices: serde_json::Value,
    pub tool: String,
    pub guardrail_label: String,
    pub turn_id: String,
    pub raised_at: i64,
    pub expires_at: Option<i64>,
    pub signed: Option<SignedView>,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
    pub from_address: Option<String>,
    pub cleared_at: Option<i64>,
    pub resolution: Option<String>,
}

impl From<&FabricAttention> for AttentionView {
    fn from(a: &FabricAttention) -> Self {
        let signed = match (&a.request, &a.signature) {
            (Some(r), Some(s)) => Some(SignedView { request: b64(r), signature: b64(s) }),
            _ => None,
        };
        AttentionView {
            id: a.id.clone(),
            source: a.source.clone(),
            runtime_id: a.runtime_id,
            session_id: a.session_id,
            address: a.address.clone(),
            kind: a.kind.clone(),
            text: a.text.clone(),
            choices: json_or(&a.choices, serde_json::json!([])),
            tool: a.tool.clone(),
            guardrail_label: a.guardrail_label.clone(),
            turn_id: a.turn_id.clone(),
            raised_at: a.raised_at,
            expires_at: a.expires_at,
            signed,
            conversation_id: a.conversation_id.clone(),
            message_id: a.message_id.clone(),
            from_address: a.from_address.clone(),
            cleared_at: a.cleared_at,
            resolution: a.resolution.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrderView {
    pub id: String,
    pub runtime_id: Uuid,
    pub session_id: Option<Uuid>,
    pub device_id: String,
    pub counter: u64,
    pub kind: String,
    pub verb: Option<String>,
    pub hold: Option<bool>,
    pub text: Option<String>,
    pub after_turn: String,
    pub queued: bool,
    pub request_id: String,
    pub choice_id: String,
    pub order_ids: serde_json::Value,
    pub issued_at: i64,
    pub expires_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub state: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
}

impl From<&FabricOrder> for OrderView {
    fn from(o: &FabricOrder) -> Self {
        OrderView {
            id: o.id.clone(),
            runtime_id: o.runtime_id,
            session_id: o.session_id,
            device_id: o.device_id.clone(),
            counter: o.counter,
            kind: o.kind.clone(),
            verb: o.verb.clone(),
            hold: o.hold,
            text: o.text.clone(),
            after_turn: o.after_turn.clone(),
            queued: o.queued,
            request_id: o.request_id.clone(),
            choice_id: o.choice_id.clone(),
            order_ids: json_or(&o.order_ids, serde_json::json!([])),
            issued_at: o.issued_at,
            expires_at: o.expires_at,
            created_at: o.created_at,
            updated_at: o.updated_at,
            state: o.state.clone(),
            reason: o.reason.clone(),
            detail: o.detail.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PairingView {
    pub pairing_id: String,
    pub runtime_id: Uuid,
    pub state: String,
    pub device_id: Option<String>,
    pub detail: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
}

impl From<&FabricPairing> for PairingView {
    fn from(p: &FabricPairing) -> Self {
        PairingView {
            pairing_id: p.id.clone(),
            runtime_id: p.runtime_id,
            state: p.state.clone(),
            device_id: p.device_id.clone(),
            detail: p.detail.clone(),
            created_at: p.created_at,
            expires_at: p.expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConversationView {
    pub id: String,
    pub a: Party,
    pub b: Party,
    pub count: u32,
    pub total: u32,
    pub last_at: i64,
    pub paused: bool,
}

impl From<&FabricConversation> for ConversationView {
    fn from(c: &FabricConversation) -> Self {
        ConversationView { id: c.id.clone(), a: c.a.clone(), b: c.b.clone(), count: c.count, total: c.total, last_at: c.last_at, paused: c.paused }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MessageView {
    pub id: String,
    pub conversation_id: Option<String>,
    pub from: Party,
    pub to: Party,
    pub kind: String,
    pub body: Option<String>,
    pub doc: String,
    pub reply_to: String,
    pub sent_at: i64,
    pub read_at: Option<i64>,
    pub state: String,
    pub bounce_reason: Option<String>,
    pub answered: bool,
}

impl From<&FabricMessage> for MessageView {
    fn from(m: &FabricMessage) -> Self {
        MessageView {
            id: m.id.clone(),
            conversation_id: m.conversation_id.clone(),
            from: m.from.clone(),
            to: m.to.clone(),
            kind: m.kind.clone(),
            body: m.body.clone(),
            doc: m.doc.clone(),
            reply_to: m.reply_to.clone(),
            sent_at: m.sent_at,
            read_at: m.read_at,
            state: m.state.clone(),
            bounce_reason: m.bounce_reason.clone(),
            answered: m.answered,
        }
    }
}

/// A project's switches as the routes show them, with its live sessions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectView {
    pub name: String,
    pub text_off: bool,
    pub private_notifications: bool,
    pub sessions: usize,
}

impl ProjectView {
    pub fn of(p: &FabricProject, sessions: usize) -> Self {
        ProjectView { name: p.name.clone(), text_off: p.text_off, private_notifications: p.private_notifications, sessions }
    }
}
