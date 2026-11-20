//! The send window both ends of the runtime channel obey (`runtime.proto`,
//! "Flow control"). Neither side may have more than [`WINDOW`] events in
//! flight that the other has not acknowledged, and each side acknowledges
//! what it receives promptly, coalesced.
//!
//! The cause is the proxy in front of the hub: it ends an HTTP/2
//! connection, and every stream on it, once about 145 small DATA frames are
//! buffered on it in either direction. A burst of small events is exactly
//! that, so the protocol bounds the burst itself.
//!
//! Both halves are plain state with the clock passed in, shared by the hub
//! and the fake runtime.

use std::time::{Duration, Instant};

/// Events one side may have sent beyond the last one acknowledged.
pub const WINDOW: usize = 64;
/// An acknowledgement is sent once this many events are unacknowledged…
pub const ACK_EVERY: usize = 16;
/// …or once the oldest of them has waited this long, whichever is first.
pub const ACK_WITHIN: Duration = Duration::from_millis(250);

// A full window of events plus the acks it earns stays well under the ~145
// small frames at which the proxy ends the connection.
const _: () = assert!(WINDOW + WINDOW / ACK_EVERY < 145 / 2);

/// The sending half: how many events are out, and whether one more may go.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SendWindow {
    /// The numbers sent on this stream and not yet acknowledged, in order.
    in_flight: std::collections::VecDeque<u64>,
    /// The most ever in flight at once.
    peak: usize,
}

impl SendWindow {
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// The most events ever in flight at once on this stream.
    pub fn peak(&self) -> usize {
        self.peak
    }

    /// May one more event be sent now?
    pub fn has_room(&self) -> bool {
        self.in_flight.len() < WINDOW
    }

    /// Event `seq` is being sent. False (and nothing recorded) when the
    /// window is full: the caller waits for an acknowledgement.
    pub fn sent(&mut self, seq: u64) -> bool {
        if !self.has_room() {
            return false;
        }
        self.in_flight.push_back(seq);
        self.peak = self.peak.max(self.in_flight.len());
        true
    }

    /// The other side has received every event up to `seq`.
    pub fn acked(&mut self, seq: u64) {
        while self.in_flight.front().is_some_and(|s| *s <= seq) {
            self.in_flight.pop_front();
        }
    }
}

/// The receiving half: what to acknowledge, and when.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Receipts {
    /// The highest event number received.
    highest: u64,
    /// Events received since the last acknowledgement.
    unacked: usize,
    /// When the oldest of those arrived.
    oldest: Option<Instant>,
}

impl Receipts {
    /// Receipts for a stream whose sender has already been told `seq` was
    /// received (the Welcome's resume point).
    pub fn starting_at(seq: u64) -> Self {
        Self { highest: seq, unacked: 0, oldest: None }
    }

    /// Event `seq` arrived at `now`.
    pub fn received(&mut self, seq: u64, now: Instant) {
        self.highest = self.highest.max(seq);
        self.unacked += 1;
        self.oldest.get_or_insert(now);
    }

    /// When an acknowledgement falls due, if one is owed: at once when
    /// [`ACK_EVERY`] events wait, else [`ACK_WITHIN`] after the oldest.
    pub fn due_at(&self) -> Option<Instant> {
        let oldest = self.oldest?;
        Some(if self.unacked >= ACK_EVERY { oldest } else { oldest + ACK_WITHIN })
    }

    pub fn is_due(&self, now: Instant) -> bool {
        self.due_at().is_some_and(|at| now >= at)
    }

    /// The number to acknowledge now; nothing is owed afterwards.
    pub fn ack(&mut self) -> u64 {
        self.unacked = 0;
        self.oldest = None;
        self.highest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sender_stops_at_the_window_and_an_ack_reopens_it() {
        let mut w = SendWindow::default();
        for seq in 1..=WINDOW as u64 {
            assert!(w.sent(seq));
        }
        assert!(!w.has_room());
        assert!(!w.sent(999), "the 65th waits");
        assert_eq!(w.in_flight(), WINDOW);
        // an ack is cumulative
        w.acked(16);
        assert_eq!(w.in_flight(), WINDOW - 16);
        assert!(w.sent(65));
        // an ack for something older, or beyond anything sent, is harmless
        w.acked(3);
        assert_eq!(w.in_flight(), WINDOW - 15);
        w.acked(u64::MAX);
        assert_eq!((w.in_flight(), w.peak()), (0, WINDOW));
    }

    #[test]
    fn numbers_need_not_be_contiguous() {
        let mut w = SendWindow::default();
        for seq in [5, 9, 40] {
            assert!(w.sent(seq));
        }
        w.acked(9);
        assert_eq!(w.in_flight(), 1);
    }

    #[test]
    fn an_ack_falls_due_after_sixteen_events_or_a_quarter_second() {
        let t0 = Instant::now();
        let mut r = Receipts::starting_at(7);
        assert_eq!(r.due_at(), None, "nothing received, nothing owed");
        r.received(8, t0);
        assert!(!r.is_due(t0));
        assert!(!r.is_due(t0 + ACK_WITHIN - Duration::from_millis(1)));
        assert!(r.is_due(t0 + ACK_WITHIN));
        for seq in 9..8 + ACK_EVERY as u64 {
            r.received(seq, t0);
        }
        assert!(r.is_due(t0), "sixteen wait: due at once");
        assert_eq!(r.ack(), 7 + ACK_EVERY as u64);
        assert_eq!(r.due_at(), None);
        // a replayed, lower number is still a receipt; the ack stays cumulative
        r.received(3, t0);
        assert_eq!(r.ack(), 7 + ACK_EVERY as u64);
    }
}
