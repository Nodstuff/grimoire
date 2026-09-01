//! Federation (ADR 0002): sharing subtrees between instances over iroh.
//!
//! An iroh endpoint on its own ALPN, alongside — never inside — the HTTP
//! surfaces. Deny-by-default: a connection from a pubkey that is not a
//! non-revoked contact may do exactly one thing, redeem an invite secret;
//! every other request from an unknown peer is refused. /api and /admin are
//! not reachable here by construction — this module only ever touches the
//! store through the specific calls in `server`.
//!
//! Layout:
//! - `wire`   — frames, requests/responses, typed refusal codes, invite tickets
//! - `server` — the accept loop, per-request auth, owner-side handlers, the
//!              hot-session bridge (owner side)
//! - `client` — grantee side: request, join, pull, propose/comment upstream,
//!              hot relay one-shots, mint_invite (owner CLI/admin helper)
//! - `loops`  — background pull / outbound-status / join-retry loops
//!
//! Wire format: one request per bi-stream. The opener writes one JSON frame
//! and finishes; the acceptor replies with one JSON frame. Every frame
//! carries `v` (protocol version) — refused loudly on mismatch.

pub mod client;
pub mod loops;
pub mod server;
pub mod wire;

// The facade the rest of the daemon calls as `fed::…`. `request`, `join_at`,
// `pull_share`, and the wire types are used within the fed modules and their
// tests (via `super::`/`crate::fed::`), not re-exported here.
pub use client::{
    comment_upstream, edit_ping_upstream, hot_end_upstream, hot_start_upstream, hot_status_upstream,
    join_once, mint_invite, open_hot_bridge, propose_upstream,
};
pub use loops::{join_retry_loop, pull_all_once, pull_loop, refresh_outbound};
pub use server::{bind, serve};
pub use wire::Ticket; // admin parses invite links

#[cfg(test)]
mod tests;
