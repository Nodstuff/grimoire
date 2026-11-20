//! The hub's picture of every fleet, in memory, fed by the runtime channel
//! and read by the viewer routes.
//!
//! - A person's fleet is loaded from the store the first time anything asks
//!   for it (in their own `Scope::User`), with every runtime offline: online
//!   is the hub's own observation and lasts only as long as a stream does.
//! - The runtime channel changes it ([`Hub::connect`], [`Hub::apply`],
//!   [`Hub::disconnect`]); each change bumps the fleet's `rev` and is
//!   published to that person's open streams ([`Hub::watch`]).
//! - SQLite hears about it sparingly, because the file is replicated and a
//!   chatty writer is a costly one. A session that changes state (or
//!   appears) is written at once. Any other change marks the row dirty and
//!   [`Hub::flush`] writes it within [`FLUSH_EVERY`]. A report identical to
//!   what is held writes nothing.
//! - Every store write of a runtime goes through [`Hub::write_runtime`],
//!   one at a time per runtime, each taking the rows as they are at that
//!   moment: an older picture can never land over a newer one.
//! - [`Hub::sweep`] expires sessions 7 days after their last update, after
//!   saying that the ones a connected runtime still reports exist.

use crate::store_ext::with_store;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use taisce_store::fabric::{FabricRuntime, FabricSession, MAX_SESSIONS_PER_RUNTIME, RuntimeLink, SESSION_RETENTION, SessionStatus};
use taisce_store::{Scope, SharedStore, StoreError};
use tokio::sync::{broadcast, oneshot};
use uuid::Uuid;

/// A dirty row waits at most this long for its write.
pub const FLUSH_EVERY: Duration = Duration::from_secs(5);
/// How often old sessions are expired.
pub const SWEEP_EVERY: Duration = Duration::from_secs(3600);
/// Events a slow stream may fall behind by before it is told to start over.
const EVENT_BUFFER: usize = 1024;

/// A session came to need its person: what a visible push says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAlert {
    pub user: Uuid,
    pub runtime_id: Uuid,
    pub session_id: Uuid,
    pub address: String,
    pub state: SessionStatus,
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
    pub state: &'static str,
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
            state: s.state.as_str(),
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

/// Where a state sorts in the fleet: what needs the person first.
pub fn attention_rank(state: SessionStatus) -> u8 {
    match state {
        SessionStatus::NeedsYou => 0,
        SessionStatus::Stalled => 1,
        SessionStatus::Finished => 2,
        SessionStatus::WaitingOnPeer => 3,
        SessionStatus::Working => 4,
        SessionStatus::Idle => 5,
        SessionStatus::Asleep => 6,
        SessionStatus::Exited => 7,
    }
}

/// By attention, then the most recent change first, then by address.
fn sort_sessions(sessions: &mut [SessionView]) {
    let rank = |s: &SessionView| SessionStatus::parse(s.state).map_or(u8::MAX, attention_rank);
    sessions.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then(b.since.cmp(&a.since)).then_with(|| a.address.cmp(&b.address)).then(a.id.cmp(&b.id))
    });
}

/// One change to a fleet.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Session(SessionView),
    SessionGone { runtime_id: Uuid, id: Uuid },
    Runtime(RuntimeView),
    RuntimeGone { id: Uuid },
}

/// A [`Change`] to `user`'s fleet, which is at `rev` once it is applied.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetEvent {
    pub user: Uuid,
    pub rev: u64,
    pub change: Change,
}

/// What a runtime sent, already validated.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// Every session it has; replaces the hub's picture of it.
    Snapshot { replaces_seq: u64, sessions: Vec<FabricSession> },
    State { seq: u64, session: Box<FabricSession> },
    /// The headroom report, as the JSON the viewer routes serve.
    Accounts { seq: u64, accounts: serde_json::Value },
}

/// Why a stream is being ended from the hub's side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kill {
    /// A newer stream for the same runtime took over.
    Superseded,
    /// The runtime was removed.
    Removed,
}

/// Why the hub refused something from a runtime.
#[derive(Debug)]
pub enum Refused {
    /// The stream is no longer this runtime's live one, or the runtime is
    /// gone: nothing was applied.
    NotLive,
    /// More sessions than a runtime may hold.
    TooManySessions,
    /// A snapshot naming one session twice.
    DuplicateSession(Uuid),
    Store(StoreError),
}

/// A stream the hub accepted.
#[derive(Debug)]
pub struct Connected {
    /// Names this stream in every later call.
    pub conn: u64,
    /// Fires when the hub ends the stream.
    pub kill: oneshot::Receiver<Kill>,
    /// The last event the hub holds from this instance.
    pub last_up_seq: u64,
}

struct Conn {
    id: u64,
    kill: oneshot::Sender<Kill>,
}

struct Slot {
    row: FabricSession,
    /// Changed since its last write.
    dirty: bool,
}

struct RuntimeState {
    row: FabricRuntime,
    /// The headroom report, parsed once.
    accounts: serde_json::Value,
    conn: Option<Conn>,
    /// The last snapshot's `replaces_seq`: a state event at or below it is
    /// older than the snapshot.
    state_floor: u64,
    sessions: HashMap<Uuid, Slot>,
    /// Sessions evicted to make room, not yet deleted from the store.
    evicted: Vec<Uuid>,
    link_dirty: bool,
    /// Serialises this runtime's store writes ([`Hub::write_runtime`]).
    writing: Arc<tokio::sync::Mutex<()>>,
}

impl RuntimeState {
    fn new(row: FabricRuntime, sessions: Vec<FabricSession>) -> Self {
        let accounts = serde_json::from_str(&row.accounts).unwrap_or_else(|_| serde_json::json!([]));
        Self {
            row,
            accounts,
            conn: None,
            state_floor: 0,
            sessions: sessions.into_iter().map(|s| (s.id, Slot { row: s, dirty: false })).collect(),
            evicted: vec![],
            link_dirty: false,
            writing: Default::default(),
        }
    }

    fn view(&self, now: i64) -> RuntimeView {
        let online = self.conn.is_some();
        RuntimeView {
            id: self.row.id,
            name: self.row.name.clone(),
            created_at: self.row.created_at,
            online,
            last_seen_at: if online { Some(now) } else { self.row.last_seen_at },
            version: self.row.version.clone(),
            accounts: self.accounts.clone(),
            accounts_at: self.row.accounts_at,
        }
    }

    fn has_writes(&self) -> bool {
        self.link_dirty || !self.evicted.is_empty() || self.sessions.values().any(|s| s.dirty)
    }

    /// Make room for `incoming` more sessions by dropping the exited ones
    /// longest unchanged, never one in `keep`. Err when even that is not
    /// enough.
    fn make_room(&mut self, incoming: usize, keep: &HashSet<Uuid>, changes: &mut Vec<Change>) -> Result<(), Refused> {
        let over = (self.sessions.len() + incoming).saturating_sub(MAX_SESSIONS_PER_RUNTIME);
        if over == 0 {
            return Ok(());
        }
        let mut exited: Vec<(i64, Uuid)> =
            self.sessions
                .values()
                .filter(|s| s.row.state == SessionStatus::Exited && !keep.contains(&s.row.id))
                .map(|s| (s.row.updated_at, s.row.id))
                .collect();
        if exited.len() < over {
            return Err(Refused::TooManySessions);
        }
        exited.sort();
        for (_, id) in exited.into_iter().take(over) {
            self.sessions.remove(&id);
            self.evicted.push(id);
            changes.push(Change::SessionGone { runtime_id: self.row.id, id });
        }
        Ok(())
    }

    /// Take a session as reported at `now`. Says whether it must be
    /// written at once.
    fn put(
        &mut self,
        mut next: FabricSession,
        now: i64,
        changes: &mut Vec<Change>,
        alerts: &mut Vec<SessionAlert>,
    ) -> Result<bool, Refused> {
        let prev = match self.sessions.get(&next.id) {
            Some(slot) => {
                // the same report again, only later: nothing to do
                next.updated_at = slot.row.updated_at;
                if slot.row == next {
                    return Ok(false);
                }
                Some(slot.row.state)
            }
            None => {
                self.make_room(1, &HashSet::new(), changes)?;
                None
            }
        };
        next.updated_at = now;
        let moved = prev != Some(next.state);
        if moved && alerts_on(prev, next.state) {
            alerts.push(SessionAlert {
                user: next.user_id,
                runtime_id: next.runtime_id,
                session_id: next.id,
                address: next.address.clone(),
                state: next.state,
            });
        }
        changes.push(Change::Session(SessionView::from(&next)));
        self.sessions.insert(next.id, Slot { row: next, dirty: true });
        Ok(moved)
    }
}

/// Does arriving in `next` from `prev` (None = first sight) warrant an
/// alert? Needing the person and stalling always do. Finishing does only at
/// the end of a turn, so a session seen finished for the first time, or
/// flipping between finished and idle, says nothing.
fn alerts_on(prev: Option<SessionStatus>, next: SessionStatus) -> bool {
    use SessionStatus::*;
    match next {
        NeedsYou | Stalled => true,
        Finished => matches!(prev, Some(Working | Stalled | WaitingOnPeer | NeedsYou)),
        _ => false,
    }
}

#[derive(Default)]
struct UserFleet {
    runtimes: BTreeMap<Uuid, RuntimeState>,
    rev: u64,
}

impl UserFleet {
    fn view(&self, now: i64) -> FleetView {
        let mut runtimes: Vec<RuntimeView> = self.runtimes.values().map(|r| r.view(now)).collect();
        runtimes.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let mut sessions: Vec<SessionView> =
            self.runtimes.values().flat_map(|r| r.sessions.values().map(|s| SessionView::from(&s.row))).collect();
        sort_sessions(&mut sessions);
        FleetView { rev: self.rev, runtimes, sessions }
    }
}

struct Inner {
    store: SharedStore,
    fleets: Mutex<HashMap<Uuid, UserFleet>>,
    events: broadcast::Sender<Arc<FleetEvent>>,
    alerter: Option<Alerter>,
    next_conn: std::sync::atomic::AtomicU64,
    /// One [`Hub::sync`] at a time: its read of the store and its merge
    /// into the fleet must not interleave with another's, or a list read
    /// before a runtime was enrolled could drop that runtime after a later
    /// sync had added it.
    syncing: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
pub struct Hub(Arc<Inner>);

impl Hub {
    pub fn new(store: SharedStore, alerter: Option<Alerter>) -> Self {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        Hub(Arc::new(Inner { store, fleets: Mutex::default(), events, alerter, next_conn: 1.into(), syncing: Default::default() }))
    }

    fn fleets(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, UserFleet>> {
        self.0.fleets.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Apply `changes` to `fleet`'s rev and publish them, in order. Called
    /// with the fleets lock held, so revs and events never disagree.
    fn publish(&self, user: Uuid, fleet: &mut UserFleet, changes: Vec<Change>) {
        for change in changes {
            fleet.rev += 1;
            // no stream open is not an error
            let _ = self.0.events.send(Arc::new(FleetEvent { user, rev: fleet.rev, change }));
        }
    }

    /// Bring `user`'s fleet in line with the store's runtimes: load it the
    /// first time, then pick up a runtime enrolled since and drop one
    /// removed since (the box CLI edits the db under a running daemon).
    async fn sync(&self, user: Uuid) -> Result<(), StoreError> {
        let _one_at_a_time = self.0.syncing.lock().await;
        let loaded = self.fleets().contains_key(&user);
        let (runtimes, sessions) = with_store(&self.0.store, Scope::User(user), move |s| {
            Ok::<_, StoreError>((s.fabric_runtimes(false)?, if loaded { vec![] } else { s.fabric_sessions()? }))
        })
        .await?;
        let now = crate::auth::now();
        let mut fleets = self.fleets();
        // a fleet being loaded for the first time has no watchers to tell
        let watched = fleets.contains_key(&user);
        let fleet = fleets.entry(user).or_default();
        let live: HashSet<Uuid> = runtimes.iter().map(|r| r.id).collect();
        let mut changes = vec![];
        let gone: Vec<Uuid> = fleet.runtimes.keys().filter(|id| !live.contains(id)).copied().collect();
        for id in gone {
            if let Some(rt) = fleet.runtimes.remove(&id) {
                if let Some(conn) = rt.conn {
                    let _ = conn.kill.send(Kill::Removed);
                }
                changes.push(Change::RuntimeGone { id });
            }
        }
        let mut by_runtime: HashMap<Uuid, Vec<FabricSession>> = HashMap::new();
        for s in sessions {
            by_runtime.entry(s.runtime_id).or_default().push(s);
        }
        for row in runtimes {
            if !fleet.runtimes.contains_key(&row.id) {
                let sessions = by_runtime.remove(&row.id).unwrap_or_default();
                let rt = RuntimeState::new(row, sessions);
                if watched {
                    changes.push(Change::Runtime(rt.view(now)));
                }
                fleet.runtimes.insert(rt.row.id, rt);
            }
        }
        self.publish(user, fleet, changes);
        Ok(())
    }

    /// `user`'s fleet as it stands.
    pub async fn fleet(&self, user: Uuid) -> Result<FleetView, StoreError> {
        self.sync(user).await?;
        let now = crate::auth::now();
        Ok(self.fleets().get(&user).map(|f| f.view(now)).unwrap_or(FleetView { rev: 0, runtimes: vec![], sessions: vec![] }))
    }

    /// `user`'s fleet and every change after it. The receiver carries every
    /// person's events: the caller keeps the ones for `user` whose rev is
    /// above the fleet's.
    pub async fn watch(&self, user: Uuid) -> Result<(FleetView, broadcast::Receiver<Arc<FleetEvent>>), StoreError> {
        self.sync(user).await?;
        let now = crate::auth::now();
        let fleets = self.fleets();
        // subscribed under the lock: no change can fall between the two
        let rx = self.0.events.subscribe();
        let view = fleets.get(&user).map(|f| f.view(now)).unwrap_or(FleetView { rev: 0, runtimes: vec![], sessions: vec![] });
        Ok((view, rx))
    }

    /// A runtime's stream opened and said Hello. Any stream it already had
    /// is ended: the newest one is the live one.
    pub async fn connect(&self, runtime: &FabricRuntime, instance_id: &str, version: &str, now: i64) -> Result<Connected, Refused> {
        let (user, id) = (runtime.user_id, runtime.id);
        self.sync(user).await.map_err(Refused::Store)?;
        let conn = self.0.next_conn.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (kill_tx, kill) = oneshot::channel();
        let last_up_seq = {
            let mut fleets = self.fleets();
            let fleet = fleets.get_mut(&user).ok_or(Refused::NotLive)?;
            let rt = fleet.runtimes.get_mut(&id).ok_or(Refused::NotLive)?;
            if let Some(old) = rt.conn.replace(Conn { id: conn, kill: kill_tx }) {
                let _ = old.kill.send(Kill::Superseded);
            }
            if rt.row.instance_id.as_deref() != Some(instance_id) {
                // a new launch numbers its events from 1 again
                rt.row.instance_id = Some(instance_id.to_string());
                rt.row.last_up_seq = 0;
            }
            rt.state_floor = 0;
            rt.row.version = version.to_string();
            rt.row.last_seen_at = Some(now);
            rt.link_dirty = true;
            let (view, seq) = (rt.view(now), rt.row.last_up_seq);
            self.publish(user, fleet, vec![Change::Runtime(view)]);
            seq
        };
        self.write_runtime(user, id).await;
        Ok(Connected { conn, kill, last_up_seq })
    }

    /// Take one message from the stream `conn` of `runtime`.
    pub async fn apply(&self, user: Uuid, runtime: Uuid, conn: u64, incoming: Incoming, now: i64) -> Result<(), Refused> {
        let mut alerts = vec![];
        let write_now = {
            let mut fleets = self.fleets();
            let fleet = fleets.get_mut(&user).ok_or(Refused::NotLive)?;
            let rt = fleet.runtimes.get_mut(&runtime).ok_or(Refused::NotLive)?;
            if rt.conn.as_ref().map(|c| c.id) != Some(conn) {
                return Err(Refused::NotLive);
            }
            let mut changes = vec![];
            let applied = Self::apply_to(rt, incoming, now, &mut changes, &mut alerts);
            // what was changed before a refusal is still a change
            self.publish(user, fleet, changes);
            applied?
        };
        if write_now {
            self.write_runtime(user, runtime).await;
        }
        if let Some(alerter) = &self.0.alerter {
            for a in alerts {
                alerter(a);
            }
        }
        Ok(())
    }

    /// The fleet half of [`Hub::apply`]. True when something must be
    /// written at once.
    fn apply_to(
        rt: &mut RuntimeState,
        incoming: Incoming,
        now: i64,
        changes: &mut Vec<Change>,
        alerts: &mut Vec<SessionAlert>,
    ) -> Result<bool, Refused> {
        // an event at or below the last one taken is a replay
        let fresh = |rt: &mut RuntimeState, seq: u64| {
            if seq <= rt.row.last_up_seq {
                return false;
            }
            rt.row.last_up_seq = seq;
            rt.link_dirty = true;
            true
        };
        match incoming {
            Incoming::Snapshot { replaces_seq, sessions } => {
                if sessions.len() > MAX_SESSIONS_PER_RUNTIME {
                    return Err(Refused::TooManySessions);
                }
                let mut named = HashSet::new();
                if let Some(twice) = sessions.iter().find(|s| !named.insert(s.id)) {
                    return Err(Refused::DuplicateSession(twice.id));
                }
                rt.state_floor = replaces_seq;
                let mut write_now = false;
                // what the runtime no longer has is over: shown as exited,
                // kept as history
                for slot in rt.sessions.values_mut() {
                    if !named.contains(&slot.row.id) && slot.row.state != SessionStatus::Exited {
                        slot.row.state = SessionStatus::Exited;
                        slot.row.tool.clear();
                        slot.row.since = now;
                        slot.row.updated_at = now;
                        slot.dirty = true;
                        write_now = true;
                        changes.push(Change::Session(SessionView::from(&slot.row)));
                    }
                }
                let new = sessions.iter().filter(|s| !rt.sessions.contains_key(&s.id)).count();
                rt.make_room(new, &named, changes)?;
                for s in sessions {
                    write_now |= rt.put(s, now, changes, alerts)?;
                }
                Ok(write_now || !rt.evicted.is_empty())
            }
            Incoming::State { seq, session } => {
                if !fresh(rt, seq) || seq <= rt.state_floor {
                    return Ok(false);
                }
                let moved = rt.put(*session, now, changes, alerts)?;
                Ok(moved || !rt.evicted.is_empty())
            }
            Incoming::Accounts { seq, accounts } => {
                if fresh(rt, seq) && rt.accounts != accounts {
                    rt.row.accounts = accounts.to_string();
                    rt.accounts = accounts;
                    rt.row.accounts_at = Some(now);
                    changes.push(Change::Runtime(rt.view(now)));
                }
                Ok(false)
            }
        }
    }

    /// The stream `conn` of `runtime` ended. A stream that was superseded
    /// changes nothing: the runtime is still online on its newer one.
    pub async fn disconnect(&self, user: Uuid, runtime: Uuid, conn: u64, now: i64) {
        {
            let mut fleets = self.fleets();
            let Some(fleet) = fleets.get_mut(&user) else { return };
            let Some(rt) = fleet.runtimes.get_mut(&runtime) else { return };
            if rt.conn.as_ref().map(|c| c.id) != Some(conn) {
                return;
            }
            rt.conn = None;
            rt.row.last_seen_at = Some(now);
            rt.link_dirty = true;
            let view = rt.view(now);
            self.publish(user, fleet, vec![Change::Runtime(view)]);
        }
        self.write_runtime(user, runtime).await;
    }

    /// Is `runtime` still enrolled? When it is not, the hub forgets it.
    pub async fn still_enrolled(&self, user: Uuid, runtime: Uuid) -> bool {
        match with_store(&self.0.store, Scope::User(user), move |s| s.fabric_runtime(runtime).map(|_| ())).await {
            Ok(()) => true,
            Err(StoreError::NotFound(_)) => {
                self.forget(user, runtime);
                false
            }
            // a store that cannot answer is not a removal
            Err(e) => {
                tracing::warn!("fabric: checking runtime {runtime} failed: {e}");
                true
            }
        }
    }

    fn forget(&self, user: Uuid, runtime: Uuid) {
        let mut fleets = self.fleets();
        let Some(fleet) = fleets.get_mut(&user) else { return };
        if let Some(rt) = fleet.runtimes.remove(&runtime) {
            if let Some(conn) = rt.conn {
                let _ = conn.kill.send(Kill::Removed);
            }
            self.publish(user, fleet, vec![Change::RuntimeGone { id: runtime }]);
        }
    }

    /// Write what `runtime` has pending: evictions, dirty sessions, its
    /// link. The one path to the store for a runtime's state: writers queue
    /// on the runtime's own lock and each takes the rows as they are when
    /// its turn comes.
    async fn write_runtime(&self, user: Uuid, runtime: Uuid) {
        let Some(writing) = self.fleets().get(&user).and_then(|f| f.runtimes.get(&runtime)).map(|r| r.writing.clone()) else { return };
        let _turn = writing.lock().await;
        let (evicted, rows, link) = {
            let mut fleets = self.fleets();
            let Some(rt) = fleets.get_mut(&user).and_then(|f| f.runtimes.get_mut(&runtime)) else { return };
            let rows: Vec<FabricSession> = rt
                .sessions
                .values_mut()
                .filter(|s| s.dirty)
                .map(|s| {
                    s.dirty = false;
                    s.row.clone()
                })
                .collect();
            let link = std::mem::take(&mut rt.link_dirty).then(|| rt.row.clone());
            (std::mem::take(&mut rt.evicted), rows, link)
        };
        if evicted.is_empty() && rows.is_empty() && link.is_none() {
            return;
        }
        let (ids, gone) = (rows.iter().map(|r| r.id).collect::<Vec<_>>(), evicted.clone());
        let relink = link.is_some();
        let wrote = with_store(&self.0.store, Scope::User(user), move |s| {
            s.fabric_sessions_delete(runtime, &evicted)?;
            s.fabric_sessions_put(runtime, &rows)?;
            if let Some(row) = link {
                s.fabric_runtime_link(
                    runtime,
                    &RuntimeLink {
                        instance_id: row.instance_id.as_deref(),
                        last_up_seq: row.last_up_seq,
                        version: &row.version,
                        last_seen_at: row.last_seen_at,
                        accounts: &row.accounts,
                        accounts_at: row.accounts_at,
                    },
                )?;
            }
            Ok::<_, StoreError>(())
        })
        .await;
        match wrote {
            Ok(()) => {}
            // removed under us: there is nothing to write to any more
            Err(StoreError::NotFound(_)) => self.forget(user, runtime),
            Err(e) => {
                tracing::warn!("fabric: writing runtime {runtime} failed, will retry: {e}");
                let mut fleets = self.fleets();
                if let Some(rt) = fleets.get_mut(&user).and_then(|f| f.runtimes.get_mut(&runtime)) {
                    for id in ids {
                        if let Some(slot) = rt.sessions.get_mut(&id) {
                            slot.dirty = true;
                        }
                    }
                    rt.evicted.extend(gone);
                    rt.link_dirty |= relink;
                }
            }
        }
    }

    /// Write everything pending, every runtime.
    pub async fn flush(&self) {
        let pending: Vec<(Uuid, Uuid)> = self
            .fleets()
            .iter()
            .flat_map(|(user, f)| f.runtimes.values().filter(|r| r.has_writes()).map(|r| (*user, r.row.id)))
            .collect();
        for (user, runtime) in pending {
            self.write_runtime(user, runtime).await;
        }
    }

    /// The daemon is stopping: every connected runtime was last seen now,
    /// and everything pending is written.
    pub async fn close(&self, now: i64) {
        for fleet in self.fleets().values_mut() {
            for rt in fleet.runtimes.values_mut().filter(|r| r.conn.is_some()) {
                rt.row.last_seen_at = Some(now);
                rt.link_dirty = true;
            }
        }
        self.flush().await;
    }

    /// Expire sessions last updated more than 7 days before `now`. A
    /// session a connected runtime still reports is not history: it is
    /// said to exist at `now` first.
    pub async fn sweep(&self, now: i64) {
        let online: Vec<(Uuid, Uuid)> = {
            let mut fleets = self.fleets();
            let mut online = vec![];
            for (user, fleet) in fleets.iter_mut() {
                for rt in fleet.runtimes.values_mut().filter(|r| r.conn.is_some()) {
                    for slot in rt.sessions.values_mut().filter(|s| s.row.state != SessionStatus::Exited) {
                        slot.row.updated_at = slot.row.updated_at.max(now);
                    }
                    online.push((*user, rt.row.id));
                }
            }
            online
        };
        for (user, runtime) in online {
            if let Err(e) = with_store(&self.0.store, Scope::User(user), move |s| s.fabric_sessions_touch(runtime, now)).await {
                tracing::warn!("fabric: touching runtime {runtime} failed: {e}");
            }
        }
        // every person's rows at once: maintenance, so System
        let gone = match with_store(&self.0.store, Scope::System, move |s| s.fabric_sessions_expire(now - SESSION_RETENTION)).await {
            Ok(gone) => gone,
            Err(e) => return tracing::warn!("fabric: expiring sessions failed: {e}"),
        };
        if !gone.is_empty() {
            tracing::info!(sessions = gone.len(), "fabric: expired sessions past 7 days");
        }
        let mut fleets = self.fleets();
        for (user, runtime, id) in gone {
            let Some(fleet) = fleets.get_mut(&user) else { continue };
            let Some(rt) = fleet.runtimes.get_mut(&runtime) else { continue };
            if rt.sessions.remove(&id).is_some() {
                self.publish(user, fleet, vec![Change::SessionGone { runtime_id: runtime, id }]);
            }
        }
    }

    /// The hub's housekeeping: a flush every [`FLUSH_EVERY`], a sweep at
    /// start and every [`SWEEP_EVERY`].
    pub async fn maintenance_loop(self) {
        let mut flush = tokio::time::interval(FLUSH_EVERY);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sweep = tokio::time::interval(SWEEP_EVERY);
        loop {
            tokio::select! {
                _ = flush.tick() => self.flush().await,
                _ = sweep.tick() => self.sweep(crate::auth::now()).await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taisce_store::{BlockStore, PrincipalKind, SqliteStore};

    struct T {
        hub: Hub,
        store: SharedStore,
        user: Uuid,
        rt: FabricRuntime,
        alerts: Arc<Mutex<Vec<SessionAlert>>>,
    }

    fn setup() -> T {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let user = s.auth_ensure_owner(p, "tom", 1).unwrap().id;
        let rt = s.fabric_runtime_create(user, "mbp", "h1", 1).unwrap();
        let store = SharedStore::new(s);
        let alerts: Arc<Mutex<Vec<SessionAlert>>> = Default::default();
        let sink = alerts.clone();
        let hub = Hub::new(store.clone(), Some(Arc::new(move |a| sink.lock().unwrap().push(a))));
        T { hub, store, user, rt, alerts }
    }

    fn session(t: &T, id: Uuid, address: &str, state: SessionStatus, now: i64) -> FabricSession {
        FabricSession {
            runtime_id: t.rt.id,
            id,
            user_id: t.user,
            address: address.into(),
            fabric: String::new(),
            project: String::new(),
            worktree: String::new(),
            branch: String::new(),
            account: String::new(),
            state,
            tool: String::new(),
            note: String::new(),
            last_line: String::new(),
            context_used_pct: None,
            since: now,
            updated_at: now,
        }
    }

    impl T {
        /// What SQLite holds for a session.
        fn stored(&self, id: Uuid) -> Option<FabricSession> {
            self.store.lock(Scope::System).fabric_sessions().unwrap().into_iter().find(|s| s.id == id)
        }
        fn stored_runtime(&self) -> FabricRuntime {
            self.store.lock(Scope::System).fabric_runtime(self.rt.id).unwrap()
        }
        async fn connect(&self, instance: &str, now: i64) -> Connected {
            let c = self.hub.connect(&self.rt, instance, "1.0", now).await.unwrap();
            self.hub.apply(self.user, self.rt.id, c.conn, Incoming::Snapshot { replaces_seq: 0, sessions: vec![] }, now).await.unwrap();
            c
        }
        async fn state(&self, c: &Connected, seq: u64, s: FabricSession, now: i64) -> Result<(), Refused> {
            self.hub.apply(self.user, self.rt.id, c.conn, Incoming::State { seq, session: Box::new(s) }, now).await
        }
        fn alerted(&self) -> Vec<SessionStatus> {
            self.alerts.lock().unwrap().iter().map(|a| a.state).collect()
        }
    }

    #[tokio::test]
    async fn a_change_of_state_is_written_at_once_and_anything_else_waits_for_the_flush() {
        let t = setup();
        let c = t.connect("launch-1", 100).await;
        let id = Uuid::now_v7();
        // a new session: written at once
        t.state(&c, 1, session(&t, id, "taisce/main", SessionStatus::Working, 100), 100).await.unwrap();
        assert_eq!(t.stored(id).unwrap().state, SessionStatus::Working);
        // the same state with a new tool: held in memory, not yet written
        let mut s = session(&t, id, "taisce/main", SessionStatus::Working, 100);
        s.tool = "Bash".into();
        t.state(&c, 2, s.clone(), 101).await.unwrap();
        assert_eq!(t.stored(id).unwrap().tool, "");
        assert_eq!(t.hub.fleet(t.user).await.unwrap().sessions[0].tool, "Bash");
        // the same report again: nothing changes, not even its time
        let rev = t.hub.fleet(t.user).await.unwrap().rev;
        t.state(&c, 3, s.clone(), 150).await.unwrap();
        let fleet = t.hub.fleet(t.user).await.unwrap();
        assert_eq!((fleet.rev, fleet.sessions[0].updated_at), (rev, 101));
        // the flush writes it, and the last event taken with it
        t.hub.flush().await;
        let stored = t.stored(id).unwrap();
        assert_eq!((stored.tool.as_str(), stored.updated_at), ("Bash", 101));
        assert_eq!(t.stored_runtime().last_up_seq, 3);
        // a change of state does not wait
        t.state(&c, 4, session(&t, id, "taisce/main", SessionStatus::NeedsYou, 102), 102).await.unwrap();
        let stored = t.stored(id).unwrap();
        assert_eq!((stored.state, stored.tool.as_str()), (SessionStatus::NeedsYou, ""));
    }

    #[tokio::test]
    async fn alerts_fire_on_needing_you_stalling_and_the_end_of_a_turn() {
        let t = setup();
        let c = t.connect("launch-1", 100).await;
        let id = Uuid::now_v7();
        let mut seq = 0;
        let mut go = async |state| {
            seq += 1;
            t.state(&c, seq, session(&t, id, "taisce/main", state, 100 + seq as i64), 100).await.unwrap();
        };
        use SessionStatus::*;
        go(Idle).await;
        go(Working).await;
        assert!(t.alerted().is_empty(), "appearing and working say nothing");
        go(Finished).await;
        go(Idle).await;
        go(Finished).await;
        assert_eq!(t.alerted(), vec![Finished], "finished after a turn, not after idling");
        go(Working).await;
        go(NeedsYou).await;
        go(Working).await;
        go(Stalled).await;
        go(Working).await;
        go(WaitingOnPeer).await;
        go(Finished).await;
        go(Asleep).await;
        go(Exited).await;
        assert_eq!(t.alerted(), vec![Finished, NeedsYou, Stalled, Finished]);
        let a = t.alerts.lock().unwrap()[1].clone();
        assert_eq!((a.user, a.runtime_id, a.session_id, a.address.as_str()), (t.user, t.rt.id, id, "taisce/main"));
        // a session first seen needing its person says so; first seen finished does not
        let (asks, done) = (Uuid::now_v7(), Uuid::now_v7());
        t.state(&c, 50, session(&t, asks, "taisce/asks", NeedsYou, 200), 200).await.unwrap();
        t.state(&c, 51, session(&t, done, "taisce/done", Finished, 200), 200).await.unwrap();
        assert_eq!(t.alerted().len(), 5);
        assert_eq!(t.alerts.lock().unwrap()[4].session_id, asks);
    }

    #[tokio::test]
    async fn a_snapshot_replaces_the_picture_and_outranks_older_events() {
        let t = setup();
        let c = t.connect("launch-1", 100).await;
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        t.state(&c, 1, session(&t, a, "p/a", SessionStatus::Working, 100), 100).await.unwrap();
        t.state(&c, 2, session(&t, b, "p/b", SessionStatus::Working, 100), 100).await.unwrap();
        // the stream drops; the runtime reconnects as the same launch
        t.hub.disconnect(t.user, t.rt.id, c.conn, 110).await;
        assert_eq!(t.stored_runtime().last_seen_at, Some(110));
        let c = t.hub.connect(&t.rt, "launch-1", "1.0", 120).await.unwrap();
        assert_eq!(c.last_up_seq, 2, "the same launch resumes after the last event taken");
        // its snapshot was taken at event 5: `a` finished meanwhile, `b` is gone
        let snap = Incoming::Snapshot { replaces_seq: 5, sessions: vec![session(&t, a, "p/a", SessionStatus::Finished, 115)] };
        t.hub.apply(t.user, t.rt.id, c.conn, snap, 120).await.unwrap();
        assert_eq!(t.alerted(), vec![SessionStatus::Finished], "the turn that ended while away is announced");
        assert_eq!(t.stored(b).unwrap().state, SessionStatus::Exited, "what the runtime no longer has is over, and kept");
        // replayed events 3-5 are older than the snapshot; 6 is news
        t.state(&c, 4, session(&t, a, "p/a", SessionStatus::Working, 112), 121).await.unwrap();
        assert_eq!(t.stored(a).unwrap().state, SessionStatus::Finished);
        t.state(&c, 6, session(&t, a, "p/a", SessionStatus::Idle, 122), 122).await.unwrap();
        assert_eq!(t.stored(a).unwrap().state, SessionStatus::Idle);
        // and an event already taken is a replay, whatever it says
        t.state(&c, 6, session(&t, a, "p/a", SessionStatus::NeedsYou, 123), 123).await.unwrap();
        assert_eq!(t.stored(a).unwrap().state, SessionStatus::Idle);
        // a new launch numbers from 1 again
        t.hub.disconnect(t.user, t.rt.id, c.conn, 130).await;
        let c = t.hub.connect(&t.rt, "launch-2", "1.1", 140).await.unwrap();
        assert_eq!(c.last_up_seq, 0);
        t.hub.apply(t.user, t.rt.id, c.conn, Incoming::Snapshot { replaces_seq: 0, sessions: vec![] }, 140).await.unwrap();
        t.state(&c, 1, session(&t, a, "p/a", SessionStatus::Working, 141), 141).await.unwrap();
        assert_eq!(t.stored(a).unwrap().state, SessionStatus::Working);
        let rt = t.stored_runtime();
        assert_eq!((rt.instance_id.as_deref(), rt.version.as_str()), (Some("launch-2"), "1.1"));
        // a snapshot naming a session twice is refused whole
        let twice = Incoming::Snapshot {
            replaces_seq: 1,
            sessions: vec![session(&t, a, "p/a", SessionStatus::Idle, 1), session(&t, a, "p/a", SessionStatus::Idle, 1)],
        };
        assert!(matches!(t.hub.apply(t.user, t.rt.id, c.conn, twice, 150).await, Err(Refused::DuplicateSession(id)) if id == a));
    }

    #[tokio::test]
    async fn the_newest_stream_is_the_live_one() {
        let t = setup();
        let mut old = t.connect("launch-1", 100).await;
        let new = t.connect("launch-1", 101).await;
        assert_eq!(old.kill.try_recv(), Ok(Kill::Superseded));
        let id = Uuid::now_v7();
        // the old stream can no longer write, and its end changes nothing
        assert!(matches!(t.state(&old, 1, session(&t, id, "p/a", SessionStatus::Working, 1), 102).await, Err(Refused::NotLive)));
        t.hub.disconnect(t.user, t.rt.id, old.conn, 103).await;
        assert!(t.hub.fleet(t.user).await.unwrap().runtimes[0].online);
        t.state(&new, 1, session(&t, id, "p/a", SessionStatus::Working, 1), 104).await.unwrap();
        t.hub.disconnect(t.user, t.rt.id, new.conn, 105).await;
        let fleet = t.hub.fleet(t.user).await.unwrap();
        assert!(!fleet.runtimes[0].online);
        assert_eq!((fleet.runtimes[0].last_seen_at, fleet.sessions.len()), (Some(105), 1), "offline keeps its sessions");
    }

    #[tokio::test]
    async fn a_removed_runtime_is_forgotten_and_its_stream_ended() {
        let t = setup();
        let mut c = t.connect("launch-1", 100).await;
        t.state(&c, 1, session(&t, Uuid::now_v7(), "p/a", SessionStatus::Working, 1), 100).await.unwrap();
        assert!(t.hub.still_enrolled(t.user, t.rt.id).await);
        // the box CLI removes it under the running daemon
        t.store.lock(Scope::System).fabric_runtime_revoke("mbp", 110).unwrap().unwrap();
        assert!(!t.hub.still_enrolled(t.user, t.rt.id).await);
        assert_eq!(c.kill.try_recv(), Ok(Kill::Removed));
        let fleet = t.hub.fleet(t.user).await.unwrap();
        assert!(fleet.runtimes.is_empty() && fleet.sessions.is_empty());
        assert!(matches!(t.state(&c, 2, session(&t, Uuid::now_v7(), "p/b", SessionStatus::Working, 1), 111).await, Err(Refused::NotLive)));
        // and one enrolled under it shows up, offline, without a restart
        let again = t.store.lock(Scope::System).fabric_runtime_create(t.user, "air", "h2", 120).unwrap();
        let fleet = t.hub.fleet(t.user).await.unwrap();
        assert_eq!(fleet.runtimes.iter().map(|r| (r.id, r.online)).collect::<Vec<_>>(), vec![(again.id, false)]);
    }

    #[tokio::test]
    async fn a_full_runtime_drops_its_oldest_history_and_refuses_beyond_that() {
        let t = setup();
        let c = t.connect("launch-1", 100).await;
        let old = Uuid::now_v7();
        t.state(&c, 1, session(&t, old, "p/old", SessionStatus::Exited, 1), 100).await.unwrap();
        let mut live: Vec<FabricSession> = (1..MAX_SESSIONS_PER_RUNTIME)
            .map(|i| session(&t, Uuid::now_v7(), &format!("p/w:{i}"), SessionStatus::Idle, 100))
            .collect();
        for (i, s) in live.iter().enumerate() {
            t.state(&c, 2 + i as u64, s.clone(), 101).await.unwrap();
        }
        assert_eq!(t.hub.fleet(t.user).await.unwrap().sessions.len(), MAX_SESSIONS_PER_RUNTIME);
        // one more: the exited one makes room, in memory and in the store
        let extra = session(&t, Uuid::now_v7(), "p/extra", SessionStatus::Working, 102);
        t.state(&c, 1000, extra.clone(), 102).await.unwrap();
        assert!(t.stored(old).is_none() && t.stored(extra.id).is_some());
        // and one more again: nothing left to drop
        let over = session(&t, Uuid::now_v7(), "p/over", SessionStatus::Working, 103);
        assert!(matches!(t.state(&c, 1001, over.clone(), 103).await, Err(Refused::TooManySessions)));
        // a snapshot of a full, different fleet turns the old one into
        // history and drops as much of it as it must: never the snapshot's own
        live.push(extra);
        let fresh: Vec<FabricSession> =
            (0..MAX_SESSIONS_PER_RUNTIME).map(|i| session(&t, Uuid::now_v7(), &format!("q/w:{i}"), SessionStatus::Idle, 104)).collect();
        let named: HashSet<Uuid> = fresh.iter().map(|s| s.id).collect();
        t.hub.apply(t.user, t.rt.id, c.conn, Incoming::Snapshot { replaces_seq: 1001, sessions: fresh }, 104).await.unwrap();
        t.hub.flush().await;
        let stored: HashSet<Uuid> = t.store.lock(Scope::System).fabric_sessions().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(stored, named);
        let more = Incoming::Snapshot {
            replaces_seq: 1001,
            sessions: (0..=MAX_SESSIONS_PER_RUNTIME).map(|i| session(&t, Uuid::now_v7(), &format!("r/{i}"), SessionStatus::Idle, 1)).collect(),
        };
        assert!(matches!(t.hub.apply(t.user, t.rt.id, c.conn, more, 105).await, Err(Refused::TooManySessions)));
    }

    #[tokio::test]
    async fn sessions_expire_after_seven_days_unless_their_runtime_still_reports_them() {
        let t = setup();
        let c = t.connect("launch-1", 100).await;
        let (live, exited) = (Uuid::now_v7(), Uuid::now_v7());
        t.state(&c, 1, session(&t, live, "p/live", SessionStatus::Idle, 100), 100).await.unwrap();
        t.state(&c, 2, session(&t, exited, "p/exited", SessionStatus::Exited, 100), 100).await.unwrap();
        let (_, mut rx) = t.hub.watch(t.user).await.unwrap();
        let week = SESSION_RETENTION;
        // within the week nothing goes
        t.hub.sweep(100 + week - 1).await;
        assert_eq!(t.hub.fleet(t.user).await.unwrap().sessions.len(), 2);
        // past it: the exited one goes; the one a connected runtime holds stays
        t.hub.sweep(100 + week + 10).await;
        let fleet = t.hub.fleet(t.user).await.unwrap();
        assert_eq!(fleet.sessions.iter().map(|s| s.id).collect::<Vec<_>>(), vec![live]);
        assert!(t.stored(exited).is_none());
        assert_eq!(rx.recv().await.unwrap().change, Change::SessionGone { runtime_id: t.rt.id, id: exited });
        // once its runtime is offline it ages like any other
        t.hub.disconnect(t.user, t.rt.id, c.conn, 100 + week + 20).await;
        t.hub.sweep(100 + week + 10 + week).await;
        assert_eq!(t.hub.fleet(t.user).await.unwrap().sessions.len(), 1, "seven days from when it was last known to exist");
        t.hub.sweep(100 + week + 10 + week + 1).await;
        assert!(t.hub.fleet(t.user).await.unwrap().sessions.is_empty());
        assert!(t.stored(live).is_none());
        // the runtime itself is a setting: it stays
        assert_eq!(t.hub.fleet(t.user).await.unwrap().runtimes.len(), 1);
    }

    #[tokio::test]
    async fn a_restart_loads_the_fleet_with_every_runtime_offline() {
        let t = setup();
        let c = t.connect("launch-1", 100).await;
        let id = Uuid::now_v7();
        t.state(&c, 1, session(&t, id, "p/a", SessionStatus::NeedsYou, 100), 100).await.unwrap();
        let accounts = serde_json::json!([{"name": "personal", "session_used_pct": 40}]);
        t.hub.apply(t.user, t.rt.id, c.conn, Incoming::Accounts { seq: 2, accounts: accounts.clone() }, 101).await.unwrap();
        assert_eq!(t.stored_runtime().accounts, "[]", "a headroom report waits for the flush");
        // the daemon stops: what was pending is written
        t.hub.close(150).await;
        let fresh = Hub::new(t.store.clone(), None);
        let fleet = fresh.fleet(t.user).await.unwrap();
        let rt = &fleet.runtimes[0];
        assert_eq!((rt.online, rt.last_seen_at, &rt.accounts, rt.accounts_at), (false, Some(150), &accounts, Some(101)));
        assert_eq!(fleet.sessions.iter().map(|s| (s.id, s.state)).collect::<Vec<_>>(), vec![(id, "needs_you")]);
        // the same launch reconnecting to the new daemon resumes where it was
        let c = fresh.connect(&t.rt, "launch-1", "1.0", 160).await.unwrap();
        assert_eq!(c.last_up_seq, 2);
    }

    #[tokio::test]
    async fn the_fleet_sorts_by_attention() {
        let t = setup();
        let c = t.connect("launch-1", 100).await;
        use SessionStatus::*;
        for (i, state) in [Exited, Asleep, Idle, Working, WaitingOnPeer, Finished, Stalled, NeedsYou].into_iter().enumerate() {
            t.state(&c, 1 + i as u64, session(&t, Uuid::now_v7(), state.as_str(), state, 100), 100).await.unwrap();
        }
        // two in one state: the most recent change first
        let mut newer = session(&t, Uuid::now_v7(), "newer", NeedsYou, 200);
        newer.since = 200;
        t.state(&c, 20, newer, 200).await.unwrap();
        let order: Vec<String> = t.hub.fleet(t.user).await.unwrap().sessions.into_iter().map(|s| s.address).collect();
        assert_eq!(order, ["newer", "needs_you", "stalled", "finished", "waiting_on_peer", "working", "idle", "asleep", "exited"]);
    }

    #[tokio::test]
    async fn every_change_is_published_once_in_order_and_only_changes_are() {
        let t = setup();
        let (fleet, mut rx) = t.hub.watch(t.user).await.unwrap();
        assert_eq!((fleet.rev, fleet.runtimes.len(), fleet.runtimes[0].online), (0, 1, false));
        let c = t.connect("launch-1", 100).await;
        let id = Uuid::now_v7();
        t.state(&c, 1, session(&t, id, "p/a", SessionStatus::Working, 100), 100).await.unwrap();
        t.state(&c, 2, session(&t, id, "p/a", SessionStatus::Working, 100), 101).await.unwrap();
        t.hub.disconnect(t.user, t.rt.id, c.conn, 102).await;
        let mut got = vec![];
        while let Ok(e) = rx.try_recv() {
            assert_eq!(e.user, t.user);
            got.push((e.rev, match &e.change {
                Change::Runtime(r) => format!("runtime online={}", r.online),
                Change::Session(s) => format!("session {}", s.state),
                other => format!("{other:?}"),
            }));
        }
        assert_eq!(
            got,
            vec![(1, "runtime online=true".to_string()), (2, "session working".to_string()), (3, "runtime online=false".to_string())]
        );
        assert_eq!(t.hub.fleet(t.user).await.unwrap().rev, 3);
    }
}
