//! Agent Fabric, Slice 1 (Watch): a person's runtimes report their Claude
//! sessions here and the person's own devices watch them (ADR 0006). SERVER
//! mode only.
//!
//! - `grpc`: the runtime channel's service, on its own loopback port. A
//!   runtime (Workbox on a Mac) dials out, signs in with its own credential
//!   and reports its sessions over one long stream. gRPC is used on this leg
//!   and nowhere else.
//! - `hub`: every fleet in memory, fed by the channel; SQLite hears about a
//!   change of state at once and about anything else at most every 5
//!   seconds; sessions expire after 7 days.
//! - `routes`: `GET /api/fabric/{sessions,runtimes,stream}` for the
//!   person's app and web session.
//! - here: enrolling a runtime (the box CLI's `taisce fabric runtime
//!   add|list|revoke`) and the visible push when a session needs its person.
//!
//! Nothing in this slice goes down the channel but a Welcome: the hub
//! commands nothing. Enrolling and removing runtimes is a human surface
//! (the box CLI today): never MCP, never a connector.

pub mod grpc;
pub mod hub;
pub mod routes;
#[cfg(test)]
pub(crate) mod tests;

use crate::auth::AUDIT;
use taisce_store::SqliteStore;
use taisce_store::fabric::FabricRuntime;

pub use hub::{Hub, SessionAlert};
pub use routes::{FabricState, router};
pub use taisce_runtime_proto::CREDENTIAL_PREFIX;

/// `serve` flags. The runtime channel is off unless its port is set.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct FabricArgs {
    /// SERVER mode: serve the runtime channel (gRPC, cleartext HTTP/2) on
    /// this loopback port, for the proxy to route
    /// `/taisce.runtime.v1.Runtime/` to. Unset = no runtime can connect.
    #[arg(long, env = "TAISCE_RUNTIME_GRPC_PORT")]
    pub runtime_grpc_port: Option<u16>,
}

/// A fresh runtime credential: `trt_` + 256 random bits, base64url.
pub fn new_credential() -> String {
    format!("{CREDENTIAL_PREFIX}{}", crate::auth::random_token())
}

/// Enrol a runtime for `user` (or the owner) under `name`. The credential
/// is returned once and stored only as its hash; with `hash` (a credential
/// minted on the machine that will hold it, by its lowercase-hex SHA-256)
/// nothing is minted and no secret ever reaches this box.
pub fn enrol_runtime(
    store: &mut SqliteStore,
    user: Option<&str>,
    name: &str,
    hash: Option<&str>,
    now: i64,
) -> anyhow::Result<(FabricRuntime, Option<String>)> {
    let name = crate::auth::valid_token_name(name)?;
    let user_id = crate::auth::user_by_key(store, user)?;
    let (hash, secret) = crate::auth::secret_or_hash(hash, CREDENTIAL_PREFIX, new_credential)?;
    let r = store.fabric_runtime_create(user_id, name, &hash, now)?;
    tracing::info!(target: AUDIT, event = "runtime.enrol", runtime = %r.id, name = r.name, user = %r.user_id, minted_here = secret.is_some());
    Ok((r, secret))
}

/// Remove a live runtime by name or id (prefix), among `user`'s when given
/// (two people may each have a `mbp`).
pub fn revoke_runtime(store: &mut SqliteStore, key: &str, user: Option<&str>, now: i64) -> anyhow::Result<FabricRuntime> {
    let key = key.trim();
    let of = user.map(|u| crate::auth::user_by_key(store, Some(u))).transpose()?;
    let live: Vec<FabricRuntime> =
        store.fabric_runtimes(false)?.into_iter().filter(|r| of.is_none_or(|u| r.user_id == u)).collect();
    let by_name: Vec<&FabricRuntime> = live.iter().filter(|r| r.name == key).collect();
    let hits = if by_name.is_empty() {
        live.iter().filter(|r| !key.is_empty() && r.id.to_string().starts_with(key)).collect()
    } else {
        by_name
    };
    let [hit] = hits.as_slice() else {
        anyhow::bail!("no single live runtime matches {key:?} (give --user, or more of the id)");
    };
    let r = store
        .fabric_runtime_revoke(&hit.id.to_string(), now)?
        .ok_or_else(|| anyhow::anyhow!("runtime {} is already removed", hit.id))?;
    tracing::info!(target: AUDIT, event = "runtime.revoke", runtime = %r.id, name = r.name, user = %r.user_id);
    Ok(r)
}

/// `taisce fabric runtime list`: one line per runtime, removed ones
/// included. Only metadata is stored, so there is no secret here to leak.
pub fn runtime_lines(store: &SqliteStore) -> anyhow::Result<Vec<String>> {
    let at = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default();
    Ok(store
        .fabric_runtimes(true)?
        .into_iter()
        .map(|r| {
            let seen = r.last_seen_at.map(at).unwrap_or_else(|| "never".into());
            let state = r.revoked_at.map(|t| format!("removed {}", at(t))).unwrap_or_else(|| "live".into());
            format!("runtime {}  {:<20}  user {}  enrolled {}  last seen {}  {}", r.id, r.name, r.user_id, at(r.created_at), seen, state)
        })
        .collect())
}

/// What a session alert says on a lock screen: which session, and what it
/// came to. Never the session's text (a last line can quote a file).
pub fn alert_push(a: &SessionAlert) -> crate::push::Push {
    crate::push::Push::FabricSession {
        address: a.address.clone(),
        state: a.state.as_str(),
        session_id: a.session_id,
        runtime_id: a.runtime_id,
    }
}
