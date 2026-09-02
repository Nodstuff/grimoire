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
}

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
}
