//! In-process per-IP token buckets for the unauthenticated endpoints. One
//! process serves everything, so a map behind a mutex is the whole design.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Which endpoint family a request draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// authorize page, passkey ceremonies, enrollment
    Login,
    /// token + revoke
    Token,
    /// dynamic client registration
    Register,
    /// the public share-link routes (`/s/…`): a page, its images, comments
    Share,
    /// the same routes per link (keyed on the token's hash): many addresses
    /// hammering one link
    ShareLink,
}

impl Class {
    /// (burst, seconds per token)
    fn shape(self) -> (f64, f64) {
        match self {
            Class::Login => (20.0, 3.0),
            Class::Token => (30.0, 1.0),
            Class::Register => (10.0, 30.0),
            Class::Share => (120.0, 0.5),
            Class::ShareLink => (300.0, 0.25),
        }
    }
}

#[derive(Clone, Default)]
pub struct Limiter(Arc<Mutex<HashMap<(Class, String), (f64, Instant)>>>);

impl Limiter {
    /// Take one token for `ip` in `class`; false = over the limit.
    pub fn allow(&self, class: Class, ip: &str) -> bool {
        self.allow_at(class, ip, Instant::now())
    }

    fn allow_at(&self, class: Class, ip: &str, now: Instant) -> bool {
        let (burst, per) = class.shape();
        let mut m = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (tokens, at) = m.entry((class, ip.to_string())).or_insert((burst, now));
        let refill = now.saturating_duration_since(*at).as_secs_f64() / per;
        *tokens = (*tokens + refill).min(burst);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Drop buckets idle long enough to be full again.
    pub fn sweep(&self) {
        let now = Instant::now();
        let mut m = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        m.retain(|(class, _), (_, at)| {
            let (burst, per) = class.shape();
            now.saturating_duration_since(*at).as_secs_f64() < burst * per
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn burst_then_refill_per_ip() {
        let l = Limiter::default();
        let t0 = Instant::now();
        for _ in 0..10 {
            assert!(l.allow_at(Class::Register, "1.2.3.4", t0));
        }
        assert!(!l.allow_at(Class::Register, "1.2.3.4", t0), "burst spent");
        assert!(l.allow_at(Class::Register, "5.6.7.8", t0), "another ip has its own bucket");
        assert!(l.allow_at(Class::Login, "1.2.3.4", t0), "another class has its own bucket");
        assert!(l.allow_at(Class::Register, "1.2.3.4", t0 + Duration::from_secs(31)), "refilled");
    }
}
