//! The hub's picture of every fleet, in memory, fed by the runtime channel
//! and read by the viewer routes.
//!
//! - A person's fleet is loaded from the store the first time anything asks
//!   for it (in their own `Scope::User`), with every runtime offline: online
//!   is the hub's own observation and lasts only as long as a stream does.
//! - The runtime channel changes it ([`Hub::connect`], [`Hub::apply`],
//!   [`Hub::disconnect`]); each change bumps the fleet's `rev` and is
//!   published on that fleet's own channel ([`Hub::watch`]): a subscriber
//!   holds one person's events and no other's.
//! - SQLite hears about it sparingly, because the file is replicated and a
//!   chatty writer is a costly one. A session that changes state (or
//!   appears) is written at once, but at most once per [`URGENT_FLOOR`] per
//!   session: a session that flaps rides the flush. Any other change marks
//!   the row dirty and [`Hub::flush`] writes it within [`FLUSH_EVERY`]. A
//!   report identical to what is held writes nothing.
//! - An alert is raised when a session comes to need its person
//!   ([`AlertKind::on`]), at most once per [`ALERT_FLOOR`] per session and
//!   kind: APNs throttles a whole provider, so one flapping session must
//!   not spend everyone's pushes.
//! - A person can snooze a session ([`Hub::snooze`]): until the time they
//!   gave, it raises no alert. It stays in the fleet, marked. The snooze is
//!   theirs, not the runtime's: no report changes it, and it is lifted when
//!   its time passes ([`Hub::wake`], on the flush tick).
//! - Every store write of a runtime goes through [`Hub::write_runtime`],
//!   one at a time per runtime, each taking the rows as they are at that
//!   moment and writing them in one transaction: an older picture can
//!   never land over a newer one.
//! - [`Hub::sweep`] expires sessions 7 days after their last update. A
//!   session is "updated" whenever its runtime reports it changed, and at
//!   least daily while its runtime keeps reporting it at all.

use super::view::{
    AlertKind, Alerter, Change, FleetEvent, FleetView, RuntimeInfo, RuntimeView, SessionAlert, SessionView, sort_sessions,
};
use crate::store_ext::with_store;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use taisce_store::fabric::{
    FabricProject, FabricRuntime, FabricSession, MAX_SESSIONS_PER_RUNTIME, RuntimeLink, SESSION_RETENTION, SessionStatus,
};
use taisce_store::{Scope, SharedStore, StoreError};
use tokio::sync::{broadcast, oneshot};
use uuid::Uuid;

/// A dirty row waits at most this long for its write.
pub const FLUSH_EVERY: Duration = Duration::from_secs(5);
/// How often old sessions are expired.
pub const SWEEP_EVERY: Duration = Duration::from_secs(3600);
/// How often orders, offers and mail that ran out of time are dealt with.
pub const EXPIRE_EVERY: Duration = Duration::from_secs(60);
/// Events a slow stream may fall behind by before it is told to start over.
const EVENT_BUFFER: usize = 1024;

/// A session's change of state is written at once at most this often;
/// further changes inside it ride the flush.
pub const URGENT_FLOOR: i64 = 5;
/// A session is announced in the same alerting state at most this often.
pub const ALERT_FLOOR: i64 = 30;
/// A session reported unchanged is said to still exist once it has gone
/// this long without an update, so it never ages out while reported.
const STILL_THERE_AFTER: i64 = 86400;

/// What a runtime sent, already validated.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// Every session it has; replaces the hub's picture of it.
    Snapshot { replaces_seq: u64, sessions: Vec<FabricSession> },
    State { seq: u64, session: Box<FabricSession> },
    /// The headroom report, as the JSON the viewer routes serve.
    Accounts { seq: u64, accounts: serde_json::Value },
    /// The runtime's own status (RuntimeStatus), whole.
    Status { seq: u64, info: Box<RuntimeInfo> },
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
    /// A message broke a limit or was malformed: the field, and what it
    /// should have been.
    Invalid(String),
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
    /// Rung when something waits to go down this stream.
    pub wake: Arc<tokio::sync::Notify>,
}

pub(super) struct Conn {
    pub(super) id: u64,
    kill: oneshot::Sender<Kill>,
    pub(super) wake: Arc<tokio::sync::Notify>,
}

pub(super) struct Slot {
    pub(super) row: FabricSession,
    /// Changed since its last write.
    dirty: bool,
    /// When each kind of alert was last raised for it.
    alerted: [Option<i64>; 3],
    /// When a change of its state was last written at once (its first
    /// appearance does not count: the first change after it is prompt).
    urgent_at: Option<i64>,
}

impl Slot {
    fn stored(row: FabricSession) -> Self {
        Slot { row, dirty: false, alerted: [None; 3], urgent_at: None }
    }
}

pub(super) struct RuntimeState {
    pub(super) row: FabricRuntime,
    /// The headroom report, parsed once.
    accounts: serde_json::Value,
    /// What the runtime last said of itself (RuntimeStatus).
    pub(super) info: RuntimeInfo,
    pub(super) conn: Option<Conn>,
    /// The last snapshot's `replaces_seq`: a state event at or below it is
    /// older than the snapshot.
    state_floor: u64,
    pub(super) sessions: HashMap<Uuid, Slot>,
    /// Sessions evicted to make room, not yet deleted from the store.
    evicted: Vec<Uuid>,
    pub(super) link_dirty: bool,
    /// Serialises this runtime's store writes ([`Hub::write_runtime`]).
    pub(super) writing: Arc<tokio::sync::Mutex<()>>,
    /// The last Down event number taken on this runtime's instance, and
    /// which instance: a new stream of the same launch numbers above it.
    pub(super) down: (Option<String>, u64),
}

impl RuntimeState {
    fn new(row: FabricRuntime, sessions: Vec<FabricSession>) -> Self {
        let accounts = serde_json::from_str(&row.accounts).unwrap_or_else(|_| serde_json::json!([]));
        let info = serde_json::from_str(&row.status).unwrap_or_default();
        Self {
            row,
            accounts,
            info,
            conn: None,
            state_floor: 0,
            sessions: sessions.into_iter().map(|s| (s.id, Slot::stored(s))).collect(),
            evicted: vec![],
            link_dirty: false,
            writing: Default::default(),
            down: (None, 0),
        }
    }

    pub(super) fn view(&self, now: i64) -> RuntimeView {
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
            held: self.info.held,
            held_since: self.info.held_since,
            settings_version: self.info.settings_version,
            present: self.info.present,
            public_key: (!self.info.public_key.is_empty()).then(|| self.info.public_key.clone()),
            devices: self.info.devices.clone(),
        }
    }

    /// Ring this runtime's live stream, if it has one: something waits to
    /// go down.
    pub(super) fn ring(&self) {
        if let Some(c) = &self.conn {
            c.wake.notify_one();
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
        if !self.sessions.contains_key(&next.id) {
            self.make_room(1, &HashSet::new(), changes)?;
        }
        let Some(slot) = self.sessions.get_mut(&next.id) else {
            // first sight
            next.updated_at = now;
            next.quiet_until = None;
            next.following = false;
            let mut slot = Slot { row: next, dirty: true, alerted: [None; 3], urgent_at: None };
            if let Some(kind) = AlertKind::on(None, slot.row.state)
                && let Some(clock) = kind.clock()
            {
                slot.alerted[clock] = Some(now);
                alerts.push(alert(&slot.row, kind));
            }
            changes.push(Change::Session(SessionView::from(&slot.row)));
            self.sessions.insert(slot.row.id, slot);
            return Ok(true);
        };
        // the same report again, only later: nothing changed. It does say
        // the session still exists, which is recorded about once a day
        next.updated_at = slot.row.updated_at;
        // a snooze and a follow are the person's: no report carries or changes them
        next.quiet_until = slot.row.quiet_until;
        next.following = slot.row.following;
        if slot.row == next {
            if now - slot.row.updated_at >= STILL_THERE_AFTER {
                slot.row.updated_at = now;
                slot.dirty = true;
            }
            return Ok(false);
        }
        next.updated_at = now;
        let prev = slot.row.state;
        let moved = prev != next.state;
        if moved
            && let Some(kind) = AlertKind::on(Some(prev), next.state)
            && let Some(clock) = kind.clock()
            // the same announcement again so soon is a session flapping
            && !slot.alerted[clock].is_some_and(|at| (now - at).abs() < ALERT_FLOOR)
            // snoozed: shown in the fleet as ever, announced to nobody
            && !slot.row.quiet_until.is_some_and(|until| until > now)
        {
            slot.alerted[clock] = Some(now);
            alerts.push(alert(&next, kind));
        }
        changes.push(Change::Session(SessionView::from(&next)));
        slot.row = next;
        slot.dirty = true;
        // a change of state is written at once, unless one just was
        let urgent = moved && !slot.urgent_at.is_some_and(|at| (now - at).abs() < URGENT_FLOOR);
        if urgent {
            slot.urgent_at = Some(now);
        }
        Ok(urgent)
    }
}

fn alert(s: &FabricSession, kind: AlertKind) -> SessionAlert {
    let mut a = SessionAlert::new(s.user_id, s.runtime_id, s.id, &s.address, kind);
    a.preview = (!s.last_line.is_empty()).then(|| s.last_line.clone());
    a
}

pub(super) struct UserFleet {
    pub(super) runtimes: BTreeMap<Uuid, RuntimeState>,
    rev: u64,
    /// This person's changes, and nobody else's.
    events: broadcast::Sender<Arc<FleetEvent>>,
    /// Their project switches (Text off, private notifications), by name.
    pub(super) projects: HashMap<String, FabricProject>,
}

impl Default for UserFleet {
    fn default() -> Self {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        Self { runtimes: BTreeMap::new(), rev: 0, events, projects: HashMap::new() }
    }
}

impl UserFleet {
    fn view(&self, now: i64) -> FleetView {
        let mut runtimes: Vec<RuntimeView> = self.runtimes.values().map(|r| r.view(now)).collect();
        runtimes.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let mut sessions: Vec<SessionView> = self
            .runtimes
            .values()
            .flat_map(|r| r.sessions.values().map(|s| SessionView::of(&s.row, self.text_off(&s.row.project))))
            .collect();
        sort_sessions(&mut sessions);
        FleetView { rev: self.rev, runtimes, sessions, attention: vec![] }
    }

    pub(super) fn text_off(&self, project: &str) -> bool {
        self.projects.get(project).is_some_and(|p| p.text_off)
    }

    pub(super) fn private(&self, project: &str) -> bool {
        self.projects.get(project).is_some_and(|p| p.private_notifications)
    }

    /// A session of this fleet, wherever it runs.
    pub(super) fn session(&self, runtime: Uuid, id: Uuid) -> Option<&FabricSession> {
        self.runtimes.get(&runtime).and_then(|r| r.sessions.get(&id)).map(|s| &s.row)
    }

    /// Make alerts fit to send: nothing while the person is at that Mac
    /// (but a followed session's finish), no text when the project's
    /// notifications are private, and a finished turn names every session
    /// now finished (a followed one alerts alone).
    pub(super) fn finish_alerts(&self, alerts: Vec<SessionAlert>, now: i64) -> Vec<SessionAlert> {
        let finished: Vec<String> = {
            let mut f: Vec<&FabricSession> = self
                .runtimes
                .values()
                .flat_map(|r| r.sessions.values().map(|s| &s.row))
                .filter(|s| s.state == SessionStatus::Finished && !s.following)
                .collect();
            f.sort_by(|a, b| b.since.cmp(&a.since).then_with(|| a.address.cmp(&b.address)));
            f.into_iter().map(|s| s.address.clone()).collect()
        };
        alerts
            .into_iter()
            .filter_map(|mut a| {
                let rt = self.runtimes.get(&a.runtime_id);
                let row = self.session(a.runtime_id, a.session_id);
                let following = row.is_some_and(|s| s.following);
                let snoozed = row.is_some_and(|s| s.quiet_until.is_some_and(|q| q > now));
                // a refused order is the person's own doing: told wherever they are
                let at_mac = rt.is_some_and(|r| r.info.present) && a.kind != AlertKind::OrderRefused && !a.from_phone;
                if snoozed || (at_mac && !(following && a.kind == AlertKind::Finished)) {
                    return None;
                }
                let project = row.map(|s| s.project.as_str()).unwrap_or("");
                if self.private(project) || self.text_off(project) {
                    a.preview = None;
                    a.choices = serde_json::json!([]);
                }
                if a.kind == AlertKind::Finished && !following {
                    a.finished = finished.clone();
                }
                Some(a)
            })
            .collect()
    }
}

pub(super) struct Inner {
    pub(super) store: SharedStore,
    fleets: Mutex<HashMap<Uuid, UserFleet>>,
    pub(super) alerter: Option<Alerter>,
    next_conn: std::sync::atomic::AtomicU64,
    /// One [`Hub::sync`] at a time: its read of the store and its merge
    /// into the fleet must not interleave with another's, or a list read
    /// before a runtime was enrolled could drop that runtime after a later
    /// sync had added it.
    syncing: tokio::sync::Mutex<()>,
    /// Fabric tool calls answered lately, by (runtime, call id): a retry
    /// gets the first answer.
    pub(super) calls: Mutex<HashMap<(Uuid, String), (i64, taisce_runtime_proto::v1::CallReply)>>,
}

#[derive(Clone)]
pub struct Hub(pub(super) Arc<Inner>);

impl Hub {
    pub fn new(store: SharedStore, alerter: Option<Alerter>) -> Self {
        Hub(Arc::new(Inner {
            store,
            fleets: Mutex::default(),
            alerter,
            next_conn: 1.into(),
            syncing: Default::default(),
            calls: Mutex::default(),
        }))
    }

    pub(super) fn fleets(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, UserFleet>> {
        self.0.fleets.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Apply `changes` to `fleet`'s rev and publish them on its own
    /// channel, in order. Called with the fleets lock held, so revs and
    /// events never disagree.
    pub(super) fn publish(fleet: &mut UserFleet, changes: Vec<Change>) {
        for mut change in changes {
            // a session's view says whether its project has Text off
            if let Change::Session(v) = &mut change {
                v.text_off = fleet.text_off(&v.project);
            }
            fleet.rev += 1;
            // no stream open is not an error
            let _ = fleet.events.send(Arc::new(FleetEvent { rev: fleet.rev, change }));
        }
    }

    /// Bring `user`'s fleet in line with the store's runtimes: load it the
    /// first time, then pick up a runtime enrolled since and drop one
    /// removed since (the box CLI edits the db under a running daemon).
    pub(super) async fn sync(&self, user: Uuid) -> Result<(), StoreError> {
        let _one_at_a_time = self.0.syncing.lock().await;
        let loaded = self.fleets().contains_key(&user);
        let (runtimes, sessions, projects) = with_store(&self.0.store, Scope::User(user), move |s| {
            Ok::<_, StoreError>((
                s.fabric_runtimes(false)?,
                if loaded { vec![] } else { s.fabric_sessions()? },
                if loaded { None } else { Some(s.fabric_projects()?) },
            ))
        })
        .await?;
        let now = crate::auth::now();
        let mut fleets = self.fleets();
        // a fleet being loaded for the first time has no watchers to tell
        let watched = fleets.contains_key(&user);
        let fleet = fleets.entry(user).or_default();
        if let Some(projects) = projects {
            fleet.projects = projects.into_iter().map(|p| (p.name.clone(), p)).collect();
        }
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
        Self::publish(fleet, changes);
        Ok(())
    }

    /// `user`'s fleet as it stands.
    pub async fn fleet(&self, user: Uuid) -> Result<FleetView, StoreError> {
        self.sync(user).await?;
        let now = crate::auth::now();
        let mut view = self.fleets().get(&user).map(|f| f.view(now)).unwrap_or_else(FleetView::empty);
        view.attention = self.open_attention(user).await?;
        Ok(view)
    }

    /// `user`'s open attention items, newest first.
    pub async fn open_attention(&self, user: Uuid) -> Result<Vec<super::view::AttentionView>, StoreError> {
        let open = with_store(&self.0.store, Scope::User(user), |s| s.fabric_attention_open()).await?;
        Ok(open.iter().map(super::view::AttentionView::from).collect())
    }

    /// `user`'s fleet and every change to it from then on: the receiver is
    /// on that fleet's own channel. (A receiver that falls
    /// [`EVENT_BUFFER`] events behind is told so, and starts over.)
    pub async fn watch(&self, user: Uuid) -> Result<(FleetView, broadcast::Receiver<Arc<FleetEvent>>), StoreError> {
        self.sync(user).await?;
        let now = crate::auth::now();
        let (mut view, rx) = {
            let mut fleets = self.fleets();
            let fleet = fleets.entry(user).or_default();
            // subscribed under the lock: no change can fall between the two
            (fleet.view(now), fleet.events.subscribe())
        };
        // read after subscribing: an item raised meanwhile is in the list,
        // or on the stream, or both (the client takes it by id)
        view.attention = self.open_attention(user).await?;
        Ok((view, rx))
    }

    /// A runtime's stream opened and said Hello. Any stream it already had
    /// is ended: the newest one is the live one.
    pub async fn connect(&self, runtime: &FabricRuntime, instance_id: &str, version: &str, now: i64) -> Result<Connected, Refused> {
        let (user, id) = (runtime.user_id, runtime.id);
        self.sync(user).await.map_err(Refused::Store)?;
        let conn = self.0.next_conn.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (kill_tx, kill) = oneshot::channel();
        let wake = Arc::new(tokio::sync::Notify::new());
        let last_up_seq = {
            let mut fleets = self.fleets();
            let fleet = fleets.get_mut(&user).ok_or(Refused::NotLive)?;
            let rt = fleet.runtimes.get_mut(&id).ok_or(Refused::NotLive)?;
            if let Some(old) = rt.conn.replace(Conn { id: conn, kill: kill_tx, wake: wake.clone() }) {
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
            Self::publish(fleet, vec![Change::Runtime(view)]);
            seq
        };
        self.write_runtime(user, id).await;
        Ok(Connected { conn, kill, last_up_seq, wake })
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
            Self::publish(fleet, changes);
            alerts = fleet.finish_alerts(std::mem::take(&mut alerts), now);
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
        // an event at or below the last one taken is a replay. The number
        // is kept in memory and stored when the stream ends: in this slice
        // a snapshot restores everything, so it is not worth a write
        let fresh = |rt: &mut RuntimeState, seq: u64| {
            if seq <= rt.row.last_up_seq {
                return false;
            }
            rt.row.last_up_seq = seq;
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
            Incoming::Status { seq, info } => {
                if fresh(rt, seq) && rt.info != *info {
                    rt.link_dirty = true;
                    rt.row.status = serde_json::to_string(&*info).unwrap_or_else(|_| "{}".into());
                    rt.info = *info;
                    changes.push(Change::Runtime(rt.view(now)));
                }
                Ok(false)
            }
            Incoming::Accounts { seq, accounts } => {
                if fresh(rt, seq) && rt.accounts != accounts {
                    rt.link_dirty = true;
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
            Self::publish(fleet, vec![Change::Runtime(view)]);
        }
        self.write_runtime(user, runtime).await;
    }

    /// Snooze `session` of `user`'s `runtime` until `until`, or lift its
    /// snooze (None). Stored in the person's own scope and announced on
    /// their stream; a session that is not theirs, or not there, is
    /// NotFound. The store and the fleet are changed together under the
    /// runtime's write lock, so two snoozes (or a snooze and [`Hub::wake`])
    /// cannot leave them saying different things.
    pub async fn snooze(&self, user: Uuid, runtime: Uuid, session: Uuid, until: Option<i64>) -> Result<SessionView, StoreError> {
        let gone = || StoreError::NotFound(format!("session {session}"));
        self.sync(user).await?;
        let writing = self
            .fleets()
            .get(&user)
            .and_then(|f| f.runtimes.get(&runtime))
            .filter(|r| r.sessions.contains_key(&session))
            .map(|r| r.writing.clone())
            .ok_or_else(gone)?;
        // a session just reported may not be in the store yet
        self.write_runtime(user, runtime).await;
        let _turn = writing.lock().await;
        with_store(&self.0.store, Scope::User(user), move |s| s.fabric_session_quiet(runtime, session, until).map(|_| ())).await?;
        let mut fleets = self.fleets();
        let fleet = fleets.get_mut(&user).ok_or_else(gone)?;
        let slot = fleet.runtimes.get_mut(&runtime).and_then(|r| r.sessions.get_mut(&session)).ok_or_else(gone)?;
        let changed = slot.row.quiet_until != until;
        slot.row.quiet_until = until;
        let view = SessionView::from(&slot.row);
        if changed {
            Self::publish(fleet, vec![Change::Session(view.clone())]);
        }
        Ok(view)
    }

    /// Lift every snooze whose time has passed at `now`, and say so on its
    /// person's stream. (Alerts go by the time itself, so one that has
    /// passed already silences nothing: this keeps the fleet's mark honest.)
    /// Per runtime, under its write lock: the store lifts only what has run
    /// out (`quiet_until <= now`), and so does the fleet, so a snooze set
    /// while this ran stays in both.
    pub async fn wake(&self, now: i64) {
        let ran_out = |s: &Slot| s.row.quiet_until.is_some_and(|until| until <= now);
        let due: Vec<(Uuid, Uuid, Arc<tokio::sync::Mutex<()>>)> = self
            .fleets()
            .iter()
            .flat_map(|(user, f)| f.runtimes.values().filter(|r| r.sessions.values().any(ran_out)).map(|r| (*user, r.row.id, r.writing.clone())))
            .collect();
        for (user, runtime, writing) in due {
            let _turn = writing.lock().await;
            match with_store(&self.0.store, Scope::User(user), move |s| s.fabric_sessions_wake(runtime, now).map(|_| ())).await {
                // removed meanwhile: there is nothing to lift
                Ok(()) | Err(StoreError::NotFound(_)) => {}
                // a time already passed silences nothing; the next load lifts it again
                Err(e) => tracing::warn!("fabric: lifting the snoozes of runtime {runtime} failed: {e}"),
            }
            let mut fleets = self.fleets();
            let Some(fleet) = fleets.get_mut(&user) else { continue };
            let Some(rt) = fleet.runtimes.get_mut(&runtime) else { continue };
            let mut changes = vec![];
            for slot in rt.sessions.values_mut().filter(|s| ran_out(s)) {
                slot.row.quiet_until = None;
                changes.push(Change::Session(SessionView::from(&slot.row)));
            }
            Self::publish(fleet, changes);
        }
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
            Self::publish(fleet, vec![Change::RuntimeGone { id: runtime }]);
        }
    }

    /// Write what `runtime` has pending: evictions, dirty sessions, its
    /// link. The one path to the store for a runtime's state: writers queue
    /// on the runtime's own lock and each takes the rows as they are when
    /// its turn comes.
    pub(super) async fn write_runtime(&self, user: Uuid, runtime: Uuid) {
        let Some(writing) = self.fleets().get(&user).and_then(|f| f.runtimes.get(&runtime)).map(|r| r.writing.clone()) else { return };
        let _turn = writing.lock().await;
        let (evicted, rows, link) = {
            let mut fleets = self.fleets();
            let off: HashSet<String> = fleets.get(&user).map(|f| f.projects.values().filter(|p| p.text_off).map(|p| p.name.clone()).collect()).unwrap_or_default();
            let Some(rt) = fleets.get_mut(&user).and_then(|f| f.runtimes.get_mut(&runtime)) else { return };
            let rows: Vec<FabricSession> = rt
                .sessions
                .values_mut()
                .filter(|s| s.dirty)
                .map(|s| {
                    s.dirty = false;
                    let mut row = s.row.clone();
                    // Text off: the hub shows it live, the store never holds it
                    if off.contains(&row.project) {
                        row.last_line.clear();
                        row.note.clear();
                    }
                    row
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
            let link = link.as_ref().map(|row| RuntimeLink {
                instance_id: row.instance_id.as_deref(),
                last_up_seq: row.last_up_seq,
                version: &row.version,
                last_seen_at: row.last_seen_at,
                accounts: &row.accounts,
                accounts_at: row.accounts_at,
                status: &row.status,
            });
            s.fabric_runtime_write(runtime, &evicted, &rows, link.as_ref())
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
                Self::publish(fleet, vec![Change::SessionGone { runtime_id: runtime, id }]);
            }
        }
    }

    /// The hub's housekeeping: a flush every [`FLUSH_EVERY`] (and the
    /// snoozes that ran out since the last one lifted), a sweep at start
    /// and every [`SWEEP_EVERY`], expiry every [`EXPIRE_EVERY`].
    pub async fn maintenance_loop(self) {
        let mut flush = tokio::time::interval(FLUSH_EVERY);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sweep = tokio::time::interval(SWEEP_EVERY);
        let mut expire = tokio::time::interval(EXPIRE_EVERY);
        expire.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = flush.tick() => {
                    self.flush().await;
                    self.wake(crate::auth::now()).await;
                }
                _ = sweep.tick() => self.sweep(crate::auth::now()).await,
                // orders that never reached a sleeping Mac in time, mail
                // nobody read for a day, rows past retention
                _ = expire.tick() => self.expire_work(crate::auth::now()).await,
            }
        }
    }
}
