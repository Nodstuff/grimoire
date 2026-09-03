//! In-memory federation runtime state (grantee side): which shares the UI is
//! looking at right now (drives the adaptive pull cadence) and the nudges
//! received from owners (surfaced to the UI as events).

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long a focus heartbeat keeps a share in the fast (5s) pull tier.
pub const FOCUS_WINDOW: Duration = Duration::from_secs(30);
const EVENT_RING: usize = 200;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Event {
    pub seq: u64,
    /// `live_started` | `doc_added` | `doc_changed`
    pub kind: String,
    pub doc_id: Uuid,
    pub doc_title: String,
    /// The owner's petname (who nudged us).
    pub from: String,
    pub at: String,
}

#[derive(Default)]
struct Inner {
    focus: HashMap<Uuid, Instant>,
    events: VecDeque<Event>,
    seq: u64,
    /// Shares with a nudged pull in flight → whether another nudge arrived
    /// meanwhile (pull again when this one finishes).
    pulling: HashMap<Uuid, bool>,
    /// Grimoires visible on the LAN right now (mDNS): pubkey → (advertised
    /// name, last seen). Saves typing a node id; grants no trust.
    neighbours: HashMap<String, (Option<String>, Instant)>,
}

/// A LAN neighbour as the UI sees it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Neighbour {
    pub pubkey: String,
    /// The name the peer advertises (its profile name), if any.
    pub name: Option<String>,
    pub seen_secs_ago: u64,
}

/// A neighbour unseen for this long is dropped from the list.
const NEIGHBOUR_TTL: Duration = Duration::from_secs(120);

/// Cheap to clone; shared across the API, the fed server, and the loops.
#[derive(Clone, Default)]
pub struct Runtime(Arc<Mutex<Inner>>);

impl Runtime {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The UI has this share's doc open: pull it on the fast tier for a while.
    pub fn focus_share(&self, share_id: Uuid) {
        self.lock().focus.insert(share_id, Instant::now());
    }

    /// Shares heartbeated within the focus window.
    pub fn focused_shares(&self) -> Vec<Uuid> {
        let mut g = self.lock();
        g.focus.retain(|_, t| t.elapsed() < FOCUS_WINDOW);
        g.focus.keys().copied().collect()
    }

    /// mDNS saw (or refreshed) a peer.
    pub fn neighbour_seen(&self, pubkey: String, name: Option<String>) {
        self.lock().neighbours.insert(pubkey, (name, Instant::now()));
    }

    /// mDNS says a peer went away.
    pub fn neighbour_gone(&self, pubkey: &str) {
        self.lock().neighbours.remove(pubkey);
    }

    /// Peers on the LAN, most recently seen first; `exclude` = our own key.
    pub fn neighbours(&self, exclude: &str) -> Vec<Neighbour> {
        let mut g = self.lock();
        g.neighbours.retain(|_, (_, t)| t.elapsed() < NEIGHBOUR_TTL);
        let mut out: Vec<Neighbour> = g
            .neighbours
            .iter()
            .filter(|(pk, _)| pk.as_str() != exclude)
            .map(|(pk, (name, t))| Neighbour {
                pubkey: pk.clone(),
                name: name.clone(),
                seen_secs_ago: t.elapsed().as_secs(),
            })
            .collect();
        out.sort_by_key(|n| n.seen_secs_ago);
        out
    }

    /// Record a nudge from an owner; returns its sequence number.
    pub fn push_event(&self, kind: &str, doc_id: Uuid, doc_title: String, from: String) -> u64 {
        let mut g = self.lock();
        g.seq += 1;
        let seq = g.seq;
        g.events.push_back(Event {
            seq,
            kind: kind.to_string(),
            doc_id,
            doc_title,
            from,
            at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        });
        while g.events.len() > EVENT_RING {
            g.events.pop_front();
        }
        seq
    }

    /// Claim the nudged pull of a share. `true` = go ahead; `false` = one is
    /// already running (it will run again when done — the burst coalesces).
    pub fn begin_pull(&self, share_id: Uuid) -> bool {
        let mut g = self.lock();
        match g.pulling.get_mut(&share_id) {
            Some(again) => {
                *again = true;
                false
            }
            None => {
                g.pulling.insert(share_id, false);
                true
            }
        }
    }

    /// Release the claim. `true` = a nudge arrived while pulling: pull again
    /// (the claim is kept so the caller loops without a gap).
    pub fn finish_pull(&self, share_id: Uuid) -> bool {
        let mut g = self.lock();
        match g.pulling.get_mut(&share_id) {
            Some(again) if *again => {
                *again = false;
                true
            }
            _ => {
                g.pulling.remove(&share_id);
                false
            }
        }
    }

    /// Events after `since` (a previous `next`), plus the new cursor. A fresh
    /// client passes 0 and gets the cursor with everything currently buffered
    /// — it should baseline silently rather than toast history.
    pub fn events_since(&self, since: u64) -> (u64, Vec<Event>) {
        let g = self.lock();
        let events = g.events.iter().filter(|e| e.seq > since).cloned().collect();
        (g.seq, events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_expires_after_the_window() {
        let rt = Runtime::default();
        let a = Uuid::now_v7();
        assert!(rt.focused_shares().is_empty());
        rt.focus_share(a);
        assert_eq!(rt.focused_shares(), vec![a]);
        // simulate expiry by back-dating the heartbeat
        rt.lock().focus.insert(a, Instant::now() - FOCUS_WINDOW - Duration::from_secs(1));
        assert!(rt.focused_shares().is_empty(), "stale focus dropped");
    }

    #[test]
    fn events_are_sequenced_bounded_and_cursor_based() {
        let rt = Runtime::default();
        let d = Uuid::now_v7();
        let (next0, ev0) = rt.events_since(0);
        assert_eq!((next0, ev0.len()), (0, 0));
        let s1 = rt.push_event("live_started", d, "Doc".into(), "tom".into());
        let s2 = rt.push_event("doc_changed", d, "Doc".into(), "tom".into());
        assert_eq!((s1, s2), (1, 2));
        let (next, evs) = rt.events_since(0);
        assert_eq!(next, 2);
        assert_eq!(evs.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), vec!["live_started", "doc_changed"]);
        // cursor: only what's new
        let (next2, evs2) = rt.events_since(next);
        assert_eq!((next2, evs2.len()), (2, 0));
        rt.push_event("doc_added", d, "New".into(), "tom".into());
        let (_, evs3) = rt.events_since(next);
        assert_eq!(evs3.len(), 1);
        assert_eq!(evs3[0].kind, "doc_added");
        // bounded ring
        for _ in 0..(EVENT_RING + 10) {
            rt.push_event("doc_changed", d, "Doc".into(), "tom".into());
        }
        assert_eq!(rt.lock().events.len(), EVENT_RING);
    }

    #[test]
    fn nudged_pulls_coalesce_per_share_while_one_is_in_flight() {
        let rt = Runtime::default();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        assert!(rt.begin_pull(a), "first claim runs");
        assert!(!rt.begin_pull(a), "second nudge does not start a second pull");
        assert!(!rt.begin_pull(a), "nor a third");
        assert!(rt.begin_pull(b), "other shares are independent");
        // finishing a: exactly one re-run is owed, however many nudges arrived
        assert!(rt.finish_pull(a), "re-run owed");
        assert!(!rt.finish_pull(a), "then released");
        assert!(rt.begin_pull(a), "claimable again");
        assert!(!rt.finish_pull(a));
        assert!(!rt.finish_pull(b));
        assert!(rt.lock().pulling.is_empty(), "nothing leaks");
    }
}
