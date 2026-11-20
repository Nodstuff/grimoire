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

/// The fully qualified service name, as the proxy routes it
/// (`/taisce.runtime.v1.Runtime/`).
pub const SERVICE: &str = "taisce.runtime.v1.Runtime";

#[cfg(feature = "client")]
pub mod fake;
pub mod window;

impl v1::Status {
    /// The state's name on the HTTP side and in scripts: `needs_you`.
    /// None for `STATUS_UNSPECIFIED`.
    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            v1::Status::Unspecified => return None,
            v1::Status::Working => "working",
            v1::Status::NeedsYou => "needs_you",
            v1::Status::Finished => "finished",
            v1::Status::Stalled => "stalled",
            v1::Status::WaitingOnPeer => "waiting_on_peer",
            v1::Status::Idle => "idle",
            v1::Status::Asleep => "asleep",
            v1::Status::Exited => "exited",
        })
    }

    pub fn from_name(name: &str) -> Option<Self> {
        (1..=8).filter_map(|n| v1::Status::try_from(n).ok()).find(|s| s.name() == Some(name))
    }
}

#[cfg(test)]
mod tests {
    use super::v1;

    #[test]
    fn status_names_round_trip_and_match_the_proto() {
        for n in 1..=8 {
            let s = v1::Status::try_from(n).expect("eight states");
            let name = s.name().unwrap();
            assert_eq!(v1::Status::from_name(name), Some(s));
            assert_eq!(s.as_str_name(), format!("STATUS_{}", name.to_ascii_uppercase()));
        }
        assert!(v1::Status::try_from(9).is_err(), "a ninth state needs a name here");
        assert_eq!(v1::Status::Unspecified.name(), None);
        assert_eq!(v1::Status::from_name("offline"), None);
    }

    /// The numbers later slices will use stay free: a message that carries
    /// one decodes with no body instead of being misread as something else.
    #[test]
    fn reserved_numbers_decode_to_no_body() {
        use prost::Message as _;
        // field 5 (turn), wire type 2, empty; then seq = 7
        let bytes = [0x2a, 0x00, 0x08, 0x07];
        let up = v1::Up::decode(&bytes[..]).unwrap();
        assert_eq!((up.seq, up.body), (7, None));
        let down = v1::Down::decode(&[0x1a, 0x00][..]).unwrap();
        assert_eq!(down.body, None);
    }
}
