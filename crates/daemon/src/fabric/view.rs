//! What the hub shows of a fleet: the shapes the viewer routes serve, the
//! order sessions are listed in, the changes a stream carries, and what an
//! alert says.

use serde::Serialize;
use std::sync::Arc;
use taisce_store::fabric::{FabricSession, SessionStatus};
use uuid::Uuid;

/// The three things a session can come to that its person is told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    NeedsYou,
    Stalled,
    Finished,
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

    /// What a notification says.
    pub fn words(self) -> &'static str {
        match self {
            AlertKind::NeedsYou => "Needs you",
            AlertKind::Stalled => "Stalled",
            AlertKind::Finished => "Finished",
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
    /// When its stream was last up: now, while it is.
    pub last_seen_at: Option<i64>,
    pub version: String,
    /// Headroom per Claude account, as last reported.
    pub accounts: serde_json::Value,
    pub accounts_at: Option<i64>,
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
}

impl FleetView {
    pub fn empty() -> Self {
        FleetView { rev: 0, runtimes: vec![], sessions: vec![] }
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
}

/// A [`Change`] to one person's fleet, which is at `rev` once it is
/// applied. It names no person: each fleet publishes on a channel of its
/// own, so a subscriber only ever holds its own person's events.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetEvent {
    pub rev: u64,
    pub change: Change,
}
