//! The hub, driven directly: what is written when, what is announced,
//! what a snapshot replaces, what expires.

use super::hub::*;
use super::view::*;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use taisce_store::fabric::{FabricRuntime, FabricSession, MAX_SESSIONS_PER_RUNTIME, SESSION_RETENTION, SessionStatus};
use taisce_store::{BlockStore, PrincipalKind, Scope, SharedStore, SqliteStore};
use uuid::Uuid;

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
    FabricSession::blank(t.rt.id, id, t.user, address, state, now)
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
    fn alerted(&self) -> Vec<AlertKind> {
        self.alerts.lock().unwrap().iter().map(|a| a.kind).collect()
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
    // the flush writes it. The last event number is not worth a write of
    // its own (a snapshot restores this slice's events): it waits for the
    // stream's end
    t.hub.flush().await;
    let stored = t.stored(id).unwrap();
    assert_eq!((stored.tool.as_str(), stored.updated_at), ("Bash", 101));
    assert_eq!(t.stored_runtime().last_up_seq, 0);
    // a change of state does not wait
    t.state(&c, 4, session(&t, id, "taisce/main", SessionStatus::NeedsYou, 110), 110).await.unwrap();
    let stored = t.stored(id).unwrap();
    assert_eq!((stored.state, stored.tool.as_str()), (SessionStatus::NeedsYou, ""));
    t.hub.disconnect(t.user, t.rt.id, c.conn, 120, false).await;
    assert_eq!(t.stored_runtime().last_up_seq, 4);
}

#[tokio::test]
async fn alerts_fire_on_needing_you_stalling_and_the_end_of_a_turn() {
    let t = setup();
    let c = t.connect("launch-1", 100).await;
    let id = Uuid::now_v7();
    let mut seq = 0;
    // a minute apart: this test is about which changes announce, not how often
    let mut go = async |state| {
        seq += 1;
        let now = 100 + 60 * seq as i64;
        t.state(&c, seq, session(&t, id, "taisce/main", state, now), now).await.unwrap();
    };
    use SessionStatus::*;
    go(Idle).await;
    go(Working).await;
    assert!(t.alerted().is_empty(), "appearing and working say nothing");
    go(Finished).await;
    go(Idle).await;
    go(Finished).await;
    assert_eq!(t.alerted(), vec![AlertKind::Finished], "finished after a turn, not after idling");
    go(Working).await;
    go(NeedsYou).await;
    go(Working).await;
    go(Stalled).await;
    go(Working).await;
    go(WaitingOnPeer).await;
    go(Finished).await;
    go(Asleep).await;
    go(Exited).await;
    assert_eq!(t.alerted(), vec![AlertKind::Finished, AlertKind::NeedsYou, AlertKind::Stalled, AlertKind::Finished]);
    let a = t.alerts.lock().unwrap()[1].clone();
    assert_eq!((a.user, a.runtime_id, a.session_id, a.address.as_str()), (t.user, t.rt.id, id, "taisce/main"));
    // a session first seen needing its person says so; first seen finished does not
    let (asks, done) = (Uuid::now_v7(), Uuid::now_v7());
    t.state(&c, 50, session(&t, asks, "taisce/asks", NeedsYou, 2000), 2000).await.unwrap();
    t.state(&c, 51, session(&t, done, "taisce/done", Finished, 2000), 2000).await.unwrap();
    assert_eq!(t.alerted().len(), 5);
    assert_eq!(t.alerts.lock().unwrap()[4].session_id, asks);
    // a state this build has no name for is shown and says nothing, coming or going
    let odd = Uuid::now_v7();
    t.state(&c, 52, session(&t, odd, "taisce/odd", Working, 2100), 2100).await.unwrap();
    t.state(&c, 53, session(&t, odd, "taisce/odd", Unknown, 2200), 2200).await.unwrap();
    t.state(&c, 54, session(&t, odd, "taisce/odd", Finished, 2300), 2300).await.unwrap();
    assert_eq!(t.alerted().len(), 5, "unknown is not the end of a turn");
}

#[tokio::test]
async fn a_snapshot_replaces_the_picture_and_outranks_older_events() {
    let t = setup();
    let c = t.connect("launch-1", 100).await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    t.state(&c, 1, session(&t, a, "p/a", SessionStatus::Working, 100), 100).await.unwrap();
    t.state(&c, 2, session(&t, b, "p/b", SessionStatus::Working, 100), 100).await.unwrap();
    // the stream drops; the runtime reconnects as the same launch
    t.hub.disconnect(t.user, t.rt.id, c.conn, 110, false).await;
    assert_eq!(t.stored_runtime().last_seen_at, Some(110));
    let c = t.hub.connect(&t.rt, "launch-1", "1.0", 120).await.unwrap();
    assert_eq!(c.last_up_seq, 2, "the same launch resumes after the last event taken");
    // its snapshot was taken at event 5: `a` finished meanwhile, `b` is gone
    let snap = Incoming::Snapshot { replaces_seq: 5, sessions: vec![session(&t, a, "p/a", SessionStatus::Finished, 115)] };
    t.hub.apply(t.user, t.rt.id, c.conn, snap, 120).await.unwrap();
    assert_eq!(t.alerted(), vec![AlertKind::Finished], "the turn that ended while away is announced");
    assert_eq!(t.stored(b).unwrap().state, SessionStatus::Exited, "what the runtime no longer has is over, and kept");
    // replayed events 3-5 are older than the snapshot; 6 is news
    t.state(&c, 4, session(&t, a, "p/a", SessionStatus::Working, 112), 121).await.unwrap();
    assert_eq!(t.stored(a).unwrap().state, SessionStatus::Finished);
    t.state(&c, 6, session(&t, a, "p/a", SessionStatus::Idle, 130), 130).await.unwrap();
    assert_eq!(t.stored(a).unwrap().state, SessionStatus::Idle);
    // and an event already taken is a replay, whatever it says
    t.state(&c, 6, session(&t, a, "p/a", SessionStatus::NeedsYou, 140), 140).await.unwrap();
    assert_eq!(t.stored(a).unwrap().state, SessionStatus::Idle);
    // a new launch numbers from 1 again
    t.hub.disconnect(t.user, t.rt.id, c.conn, 150, false).await;
    let c = t.hub.connect(&t.rt, "launch-2", "1.1", 160).await.unwrap();
    assert_eq!(c.last_up_seq, 0);
    t.hub.apply(t.user, t.rt.id, c.conn, Incoming::Snapshot { replaces_seq: 0, sessions: vec![] }, 160).await.unwrap();
    t.state(&c, 1, session(&t, a, "p/a", SessionStatus::Working, 170), 170).await.unwrap();
    assert_eq!(t.stored(a).unwrap().state, SessionStatus::Working);
    let rt = t.stored_runtime();
    assert_eq!((rt.instance_id.as_deref(), rt.version.as_str()), (Some("launch-2"), "1.1"));
    // a snapshot naming a session twice is refused whole
    let twice = Incoming::Snapshot {
        replaces_seq: 1,
        sessions: vec![session(&t, a, "p/a", SessionStatus::Idle, 1), session(&t, a, "p/a", SessionStatus::Idle, 1)],
    };
    assert!(matches!(t.hub.apply(t.user, t.rt.id, c.conn, twice, 180).await, Err(Refused::DuplicateSession(id)) if id == a));
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
    t.hub.disconnect(t.user, t.rt.id, old.conn, 103, false).await;
    assert!(t.hub.fleet(t.user).await.unwrap().runtimes[0].online);
    t.state(&new, 1, session(&t, id, "p/a", SessionStatus::Working, 1), 104).await.unwrap();
    t.hub.disconnect(t.user, t.rt.id, new.conn, 105, false).await;
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
    t.store.lock(Scope::System).fabric_runtime_revoke(t.rt.id, 110).unwrap();
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
    t.hub.disconnect(t.user, t.rt.id, c.conn, 100 + week + 20, false).await;
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
    assert_eq!(fleet.sessions.iter().map(|s| (s.id, s.state)).collect::<Vec<_>>(), vec![(id, SessionStatus::NeedsYou)]);
    // the same launch reconnecting to the new daemon resumes where it was
    let c = fresh.connect(&t.rt, "launch-1", "1.0", 160).await.unwrap();
    assert_eq!(c.last_up_seq, 2);
}

#[tokio::test]
async fn the_fleet_sorts_by_attention() {
    let t = setup();
    let c = t.connect("launch-1", 100).await;
    use SessionStatus::*;
    for (i, state) in [Exited, Asleep, Idle, Unknown, Working, WaitingOnPeer, Finished, Stalled, NeedsYou].into_iter().enumerate() {
        t.state(&c, 1 + i as u64, session(&t, Uuid::now_v7(), state.as_str(), state, 100), 100).await.unwrap();
    }
    // two in one state: the most recent change first
    let mut newer = session(&t, Uuid::now_v7(), "newer", NeedsYou, 200);
    newer.since = 200;
    t.state(&c, 20, newer, 200).await.unwrap();
    let order: Vec<String> = t.hub.fleet(t.user).await.unwrap().sessions.into_iter().map(|s| s.address).collect();
    assert_eq!(order, ["newer", "needs_you", "stalled", "finished", "waiting_on_peer", "working", "unknown", "idle", "asleep", "exited"]);
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
    t.hub.disconnect(t.user, t.rt.id, c.conn, 102, false).await;
    let mut got = vec![];
    while let Ok(e) = rx.try_recv() {
        got.push((e.rev, match &e.change {
            Change::Runtime(r) => format!("runtime online={}", r.online),
            Change::Session(s) => format!("session {}", s.state.as_str()),
            other => format!("{other:?}"),
        }));
    }
    assert_eq!(
        got,
        vec![(1, "runtime online=true".to_string()), (2, "session working".to_string()), (3, "runtime online=false".to_string())]
    );
    assert_eq!(t.hub.fleet(t.user).await.unwrap().rev, 3);
}

/// A session flapping between needing its person and working, as fast as
/// the channel allows: one announcement per 30 seconds per alerting state,
/// one immediate write per 5 seconds; the rest ride the flush. Nothing is
/// lost: the stream and the fleet see every change.
#[tokio::test]
async fn a_flapping_session_is_not_a_flood_of_pushes_or_writes() {
    use SessionStatus::*;
    let t = setup();
    let c = t.connect("launch-1", 1000).await;
    let id = Uuid::now_v7();
    let (_, mut rx) = t.hub.watch(t.user).await.unwrap();
    t.state(&c, 1, session(&t, id, "p/a", Working, 1000), 1000).await.unwrap();
    // 40 flips inside one second
    for i in 0..40u64 {
        let state = if i % 2 == 0 { NeedsYou } else { Working };
        t.state(&c, 2 + i, session(&t, id, "p/a", state, 1001), 1001).await.unwrap();
    }
    assert_eq!(t.alerted(), vec![AlertKind::NeedsYou], "one announcement, not twenty");
    assert_eq!(t.stored(id).unwrap().state, NeedsYou, "the first flip was written at once; the other 39 wait");
    let mut published = 0;
    while rx.try_recv().is_ok() {
        published += 1;
    }
    assert_eq!(published, 41, "every change still reaches the stream");
    t.hub.flush().await;
    assert_eq!(t.stored(id).unwrap().state, Working, "the flush writes where it ended up");
    // a DIFFERENT alerting state is news at once (but flapping between two
    // alerting states is held to the same floor, each on its own clock)
    t.state(&c, 50, session(&t, id, "p/a", Stalled, 1002), 1002).await.unwrap();
    assert_eq!(t.alerted(), vec![AlertKind::NeedsYou, AlertKind::Stalled]);
    t.state(&c, 55, session(&t, id, "p/a", NeedsYou, 1003), 1003).await.unwrap();
    t.state(&c, 56, session(&t, id, "p/a", Stalled, 1004), 1004).await.unwrap();
    assert_eq!(t.alerted().len(), 2);
    // the same one again: quiet inside 30 seconds, announced after
    t.state(&c, 61, session(&t, id, "p/a", Working, 1010), 1010).await.unwrap();
    t.state(&c, 62, session(&t, id, "p/a", NeedsYou, 1020), 1020).await.unwrap();
    assert_eq!(t.alerted().len(), 2, "19 seconds after the last 'needs you'");
    assert_eq!(t.stored(id).unwrap().state, NeedsYou, "5 seconds on, a change of state is written at once again");
    t.state(&c, 63, session(&t, id, "p/a", Working, 1030), 1030).await.unwrap();
    t.state(&c, 64, session(&t, id, "p/a", NeedsYou, 1031), 1031).await.unwrap();
    assert_eq!(t.alerted(), vec![AlertKind::NeedsYou, AlertKind::Stalled, AlertKind::NeedsYou], "30 seconds on");
}

/// A fleet's changes are published on that fleet's own channel: another
/// person's subscriber is handed nothing to filter.
#[tokio::test]
async fn a_subscriber_holds_only_its_own_persons_events() {
    let t = setup();
    let ann = t.store.lock(Scope::System).auth_add_user("ann", 1).unwrap().id;
    let (_, mut anns) = t.hub.watch(ann).await.unwrap();
    let (_, mut toms) = t.hub.watch(t.user).await.unwrap();
    let c = t.connect("launch-1", 100).await;
    t.state(&c, 1, session(&t, Uuid::now_v7(), "p/a", SessionStatus::NeedsYou, 100), 100).await.unwrap();
    t.hub.disconnect(t.user, t.rt.id, c.conn, 101, false).await;
    assert_eq!(std::iter::from_fn(|| toms.try_recv().ok()).count(), 3);
    assert!(matches!(anns.try_recv(), Err(tokio::sync::broadcast::error::TryRecvError::Empty)), "nothing was ever sent her way");
}

/// A laptop that is online every day but never across the hourly sweep, with
/// a session that has not changed in a week: each reconnect's snapshot says
/// the session still exists, so it is not expired (and does not come back
/// as a first sight, announcing itself again).
#[tokio::test]
async fn a_session_reported_unchanged_every_day_never_expires() {
    let t = setup();
    let day = 86400;
    let id = Uuid::now_v7();
    let asks = |t: &T| session(t, id, "p/asks", SessionStatus::NeedsYou, 100);
    let c = t.connect("launch-1", 100).await;
    t.state(&c, 1, asks(&t), 100).await.unwrap();
    t.hub.disconnect(t.user, t.rt.id, c.conn, 200, false).await;
    for n in 1..=9 {
        let now = 100 + n * day;
        let c = t.hub.connect(&t.rt, "launch-1", "1.0", now).await.unwrap();
        t.hub.apply(t.user, t.rt.id, c.conn, Incoming::Snapshot { replaces_seq: 1, sessions: vec![asks(&t)] }, now).await.unwrap();
        t.hub.disconnect(t.user, t.rt.id, c.conn, now + 60, false).await;
        // the sweep runs while the laptop is away
        t.hub.sweep(now + 3600).await;
    }
    let fleet = t.hub.fleet(t.user).await.unwrap();
    assert_eq!(fleet.sessions.len(), 1, "nine days on, still reported, still there");
    assert_eq!(t.stored(id).unwrap().updated_at, 100 + 9 * day);
    assert_eq!(t.alerted(), vec![AlertKind::NeedsYou], "announced once, nine days ago");
    // an unchanged report inside a day costs nothing
    let c = t.hub.connect(&t.rt, "launch-1", "1.0", 100 + 9 * day + 600).await.unwrap();
    let rev = t.hub.fleet(t.user).await.unwrap().rev;
    t.hub.apply(t.user, t.rt.id, c.conn, Incoming::Snapshot { replaces_seq: 1, sessions: vec![asks(&t)] }, 100 + 9 * day + 600).await.unwrap();
    assert_eq!(t.hub.fleet(t.user).await.unwrap().rev, rev);
    assert_eq!(t.hub.fleet(t.user).await.unwrap().sessions[0].updated_at, 100 + 9 * day);
}

/// A snoozed session is in the fleet as ever, marked, and announced to
/// nobody until its time; no report changes the snooze; when it runs out
/// the mark is lifted and said on the stream.
#[tokio::test]
async fn a_snoozed_session_stays_in_the_fleet_and_alerts_nobody_until_its_time() {
    let t = setup();
    let c = t.connect("launch-1", 100).await;
    let (id, other) = (Uuid::now_v7(), Uuid::now_v7());
    use SessionStatus::*;
    t.state(&c, 1, session(&t, id, "taisce/main", Working, 100), 100).await.unwrap();
    t.state(&c, 2, session(&t, other, "taisce/other", Working, 100), 100).await.unwrap();
    let (_, mut events) = t.hub.watch(t.user).await.unwrap();

    // snoozed until 1000: stored in the person's scope, announced on the stream
    let view = t.hub.snooze(t.user, t.rt.id, id, Some(1000)).await.unwrap();
    assert_eq!((view.id, view.quiet_until, view.state), (id, Some(1000), Working));
    assert_eq!(t.stored(id).unwrap().quiet_until, Some(1000));
    let e = events.try_recv().unwrap();
    assert!(matches!(&e.change, Change::Session(s) if s.id == id && s.quiet_until == Some(1000)), "{e:?}");
    // the same snooze again changes nothing and says nothing
    t.hub.snooze(t.user, t.rt.id, id, Some(1000)).await.unwrap();
    assert!(events.try_recv().is_err());

    // it comes to need its person, stalls and finishes: nothing is sent,
    // and each report leaves the snooze where it was
    t.state(&c, 3, session(&t, id, "taisce/main", NeedsYou, 200), 200).await.unwrap();
    t.state(&c, 4, session(&t, id, "taisce/main", Working, 300), 300).await.unwrap();
    t.state(&c, 5, session(&t, id, "taisce/main", Stalled, 400), 400).await.unwrap();
    t.state(&c, 6, session(&t, id, "taisce/main", Finished, 500), 500).await.unwrap();
    assert!(t.alerted().is_empty(), "{:?}", t.alerted());
    let fleet = t.hub.fleet(t.user).await.unwrap();
    let mine = fleet.sessions.iter().find(|s| s.id == id).unwrap();
    assert_eq!((mine.state, mine.quiet_until), (Finished, Some(1000)), "shown as ever, still snoozed");
    t.hub.flush().await;
    assert_eq!(t.stored(id).unwrap().quiet_until, Some(1000), "a write of the session keeps its snooze");
    // a snapshot is a report too
    let snap = vec![session(&t, id, "taisce/main", Working, 600), session(&t, other, "taisce/other", Working, 100)];
    t.hub.apply(t.user, t.rt.id, c.conn, Incoming::Snapshot { replaces_seq: 6, sessions: snap }, 600).await.unwrap();
    assert_eq!(t.hub.fleet(t.user).await.unwrap().sessions.iter().find(|s| s.id == id).unwrap().quiet_until, Some(1000));
    // the other session is not snoozed
    t.state(&c, 7, session(&t, other, "taisce/other", NeedsYou, 700), 700).await.unwrap();
    assert_eq!(t.alerts.lock().unwrap().iter().map(|a| a.session_id).collect::<Vec<_>>(), vec![other]);

    // before its time a wake lifts nothing
    while events.try_recv().is_ok() {}
    t.hub.wake(999).await;
    assert!(events.try_recv().is_err());
    // an alert goes by the time itself, whenever the wake comes round
    t.state(&c, 8, session(&t, id, "taisce/main", NeedsYou, 1000), 1000).await.unwrap();
    assert_eq!(t.alerts.lock().unwrap().iter().map(|a| a.session_id).collect::<Vec<_>>(), vec![other, id]);
    while events.try_recv().is_ok() {}
    // at its time the mark is lifted, in the fleet, on the stream and in the store
    t.hub.wake(1000).await;
    let e = events.try_recv().unwrap();
    assert!(matches!(&e.change, Change::Session(s) if s.id == id && s.quiet_until.is_none()), "{e:?}");
    assert!(events.try_recv().is_err(), "only the one that ran out");
    assert_eq!(t.stored(id).unwrap().quiet_until, None);

    // lifted by hand: the next change announces again
    t.hub.snooze(t.user, t.rt.id, other, Some(5000)).await.unwrap();
    assert_eq!(t.hub.snooze(t.user, t.rt.id, other, None).await.unwrap().quiet_until, None);
    t.state(&c, 9, session(&t, other, "taisce/other", Working, 2000), 2000).await.unwrap();
    t.state(&c, 10, session(&t, other, "taisce/other", Stalled, 2100), 2100).await.unwrap();
    assert_eq!(t.alerted().last(), Some(&AlertKind::Stalled));

    // a snooze survives the hub: a new one loads it from the store
    t.hub.snooze(t.user, t.rt.id, id, Some(9000)).await.unwrap();
    t.hub.flush().await;
    let again = Hub::new(t.store.clone(), None);
    assert_eq!(again.fleet(t.user).await.unwrap().sessions.iter().find(|s| s.id == id).unwrap().quiet_until, Some(9000));
}

/// A snooze reaches only the caller's own sessions: anyone else's, or one
/// that is not there, does not exist.
#[tokio::test]
async fn nobody_snoozes_a_session_that_is_not_theirs() {
    let t = setup();
    let c = t.connect("launch-1", 100).await;
    let id = Uuid::now_v7();
    t.state(&c, 1, session(&t, id, "taisce/main", SessionStatus::Working, 100), 100).await.unwrap();
    let ann = t.store.lock(Scope::System).auth_add_user("ann", 1).unwrap().id;
    let ann_rt = t.store.lock(Scope::System).fabric_runtime_create(ann, "air", "h2", 1).unwrap();
    let not_found = |r: Result<SessionView, taisce_store::StoreError>| matches!(r, Err(taisce_store::StoreError::NotFound(_)));
    assert!(not_found(t.hub.snooze(ann, t.rt.id, id, Some(5000)).await), "Tom's runtime is not Ann's");
    assert!(not_found(t.hub.snooze(ann, ann_rt.id, id, Some(5000)).await), "nor is his session under her own");
    assert!(not_found(t.hub.snooze(t.user, t.rt.id, Uuid::now_v7(), Some(5000)).await), "no such session");
    assert!(not_found(t.hub.snooze(t.user, Uuid::now_v7(), id, Some(5000)).await), "no such runtime");
    assert_eq!(t.stored(id).unwrap().quiet_until, None);
    assert_eq!(t.hub.fleet(t.user).await.unwrap().sessions[0].quiet_until, None);
}

/// A snooze and the lifting of one that ran out, at the same moment and
/// many times over: whichever goes first, the store and the fleet end up
/// saying the same thing, and it is the new snooze.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_snooze_racing_a_wake_leaves_the_store_and_the_fleet_agreeing() {
    let t = setup();
    let c = t.connect("launch-1", 100).await;
    let id = Uuid::now_v7();
    t.state(&c, 1, session(&t, id, "taisce/main", SessionStatus::Working, 100), 100).await.unwrap();
    for round in 0..200i64 {
        // an old snooze that has run out at 1000 is in both
        t.hub.snooze(t.user, t.rt.id, id, Some(1000)).await.unwrap();
        let next = 5000 + round;
        let (wake, snooze) = (t.hub.clone(), t.hub.clone());
        let (user, rt) = (t.user, t.rt.id);
        let a = tokio::spawn(async move { wake.wake(1000).await });
        let b = tokio::spawn(async move { snooze.snooze(user, rt, id, Some(next)).await.map(|v| v.quiet_until) });
        a.await.unwrap();
        assert_eq!(b.await.unwrap().unwrap(), Some(next));
        let held = t.hub.fleet(t.user).await.unwrap().sessions[0].quiet_until;
        assert_eq!((held, t.stored(id).unwrap().quiet_until), (Some(next), Some(next)), "round {round}");
    }
    // two devices at once: the fleet and the store agree on one of them
    for round in 0..100i64 {
        let (one, two) = (t.hub.clone(), t.hub.clone());
        let (user, rt) = (t.user, t.rt.id);
        let a = tokio::spawn(async move { one.snooze(user, rt, id, Some(7000 + round)).await.unwrap() });
        let b = tokio::spawn(async move { two.snooze(user, rt, id, None).await.unwrap() });
        let _ = (a.await.unwrap(), b.await.unwrap());
        let held = t.hub.fleet(t.user).await.unwrap().sessions[0].quiet_until;
        assert_eq!(held, t.stored(id).unwrap().quiet_until, "round {round}");
    }
}

/// Code review 2026-11-22, item 9: a sync in progress for one person no
/// longer holds up another person's fleet read; the same person's still
/// waits (the per-fleet invariant the lock exists for).
#[tokio::test]
async fn one_persons_sync_does_not_wait_on_anothers() {
    let t = setup();
    let other = t.store.lock(Scope::System).auth_add_user("ann", 1).unwrap().id;
    let held = t.hub.sync_lock(t.user);
    let _busy = held.lock().await;
    let wait = std::time::Duration::from_secs(2);
    assert!(tokio::time::timeout(wait, t.hub.fleet(other)).await.is_ok(), "ann's read waited on tom's sync");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), t.hub.fleet(t.user)).await.is_err(),
        "tom's own reads still wait for his sync"
    );
}
