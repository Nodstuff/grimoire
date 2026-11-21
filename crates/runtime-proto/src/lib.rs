//! The runtime channel's wire contract: the types and service generated from
//! `proto/taisce/runtime/v1/runtime.proto`, which is the source of truth
//! (ADR 0007). The daemon implements the service (`server`); a runtime such
//! as Workbox takes its own copy of the `.proto`.
//!
//! `client` adds the client stub and [`fake`], a scripted stand-in for a
//! runtime: the daemon's tests drive the real service with it, and the
//! `fake_runtime` example drives a scratch daemon by hand.

/// `taisce.runtime.v1`
pub mod v1 {
    tonic::include_proto!("taisce.runtime.v1");
}

/// Every credential for the runtime channel starts with this, so it can
/// never be mistaken for a personal access token (`tsk_`), a book upload
/// token (`tbu_`) or an OAuth token.
pub const CREDENTIAL_PREFIX: &str = "trt_";

#[cfg(feature = "client")]
pub mod fake;
pub mod signing;
pub mod window;

impl v1::SessionStatus {
    /// The state's name on the HTTP side and in scripts: `needs_you`.
    /// None for `SESSION_STATUS_UNSPECIFIED`.
    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            v1::SessionStatus::Unspecified => return None,
            v1::SessionStatus::Working => "working",
            v1::SessionStatus::NeedsYou => "needs_you",
            v1::SessionStatus::Finished => "finished",
            v1::SessionStatus::Stalled => "stalled",
            v1::SessionStatus::WaitingOnPeer => "waiting_on_peer",
            v1::SessionStatus::Idle => "idle",
            v1::SessionStatus::Asleep => "asleep",
            v1::SessionStatus::Exited => "exited",
        })
    }

    pub fn from_name(name: &str) -> Option<Self> {
        (1..=8).filter_map(|n| v1::SessionStatus::try_from(n).ok()).find(|s| s.name() == Some(name))
    }
}

#[cfg(test)]
mod tests {
    use super::v1;

    #[test]
    fn status_names_round_trip_and_match_the_proto() {
        for n in 1..=8 {
            let s = v1::SessionStatus::try_from(n).expect("eight states");
            let name = s.name().unwrap();
            assert_eq!(v1::SessionStatus::from_name(name), Some(s));
            assert_eq!(s.as_str_name(), format!("SESSION_STATUS_{}", name.to_ascii_uppercase()));
        }
        assert!(v1::SessionStatus::try_from(9).is_err(), "a ninth state needs a name here");
        assert_eq!(v1::SessionStatus::Unspecified.name(), None);
        assert_eq!(v1::SessionStatus::from_name("offline"), None);
    }

    /// A body this build does not know (a newer peer's) decodes as no body,
    /// never as something else: the VERSIONS rule rests on it.
    #[test]
    fn unknown_bodies_decode_to_no_body() {
        use prost::Message as _;
        // field 13 (unassigned), wire type 2, empty; then seq = 7
        let bytes = [0x6a, 0x00, 0x08, 0x07];
        let up = v1::Up::decode(&bytes[..]).unwrap();
        assert_eq!((up.seq, up.body), (7, None));
        // field 8 (unassigned)
        let down = v1::Down::decode(&[0x42, 0x00][..]).unwrap();
        assert_eq!(down.body, None);
    }

    /// Slice 1's numbers did not move: a Slice 1 runtime's bytes still
    /// mean what they meant.
    #[test]
    fn slice_one_numbers_are_unchanged() {
        use prost::Message as _;
        let up = v1::Up::decode(&[0x08, 0x03, 0x5a, 0x02, 0x08, 0x02][..]).unwrap();
        assert_eq!((up.seq, up.body), (3, Some(v1::up::Body::Ack(v1::Ack { seq: 2 }))));
        let up = v1::Up::decode(&[0x52, 0x00][..]).unwrap();
        assert_eq!(up.body, Some(v1::up::Body::Heartbeat(v1::Heartbeat {})));
        let down = v1::Down::decode(&[0x32, 0x02, 0x08, 0x05][..]).unwrap();
        assert_eq!(down.body, Some(v1::down::Body::Ack(v1::Ack { seq: 5 })));
    }
}
