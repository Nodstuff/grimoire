//! Agent Fabric, Slice 1 (Watch): a person's runtimes report their Claude
//! sessions here and the person's own devices watch them (ADR 0007). SERVER
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
#[cfg(test)]
mod hub_tests;
pub mod down;
pub mod mail;
pub mod record;
pub mod routes;
#[cfg(test)]
pub(crate) mod tests;
pub mod view;

use crate::auth::AUDIT;
use taisce_store::SqliteStore;
use taisce_store::fabric::FabricRuntime;

pub use hub::Hub;
pub use view::{AlertKind, Alerter, SessionAlert};
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
    let r = store.fabric_runtime_revoke(hit.id, now)?;
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

/// A fabric alert as a push.
pub fn alert_push(a: &SessionAlert) -> crate::push::Push {
    crate::push::Push::Fabric(Box::new(a.clone()))
}

/// APNs caps a collapse id at 64 bytes.
fn collapse(id: String) -> String {
    let mut id = id;
    while id.len() > 64 {
        id.pop();
    }
    id
}

/// One session's state alerts replace each other; a finished turn among
/// several is the one "N finished" line; each attention item and each
/// refused order is its own.
pub fn alert_collapse_id(a: &SessionAlert) -> String {
    match a.kind {
        AlertKind::Finished if !a.finished.is_empty() => "fabric:finished".into(),
        AlertKind::NeedsYou | AlertKind::Stalled | AlertKind::Finished => format!("fabric:{}", a.session_id),
        AlertKind::OrderRefused => collapse(format!("fabric:order:{}", a.order_id.as_deref().unwrap_or_default())),
        _ => collapse(format!("fabric:{}", a.request_id.clone().unwrap_or_else(|| a.session_id.to_string()))),
    }
}

/// What an alert says (ADR 0008 §10). A preview only when the hub gave one
/// (never under private notifications); the app fetches and verifies
/// anything it acts on.
pub fn alert_payload(a: &SessionAlert) -> serde_json::Value {
    use serde_json::json;
    let summary = a.kind == AlertKind::Finished && !a.finished.is_empty();
    let (title, body) = if summary {
        (format!("{} finished", a.finished.len()), a.finished.join(", "))
    } else {
        (a.address.clone(), a.preview.clone().unwrap_or_else(|| a.kind.words().to_string()))
    };
    let mut aps = json!({
        "alert": {"title": title, "body": body},
        "thread-id": format!("fabric:{}", a.session_id),
        "category": a.kind.category(),
    });
    if summary {
        aps["interruption-level"] = json!("passive");
    } else {
        aps["sound"] = json!("default");
    }
    let mut v = json!({"aps": aps, "session_id": a.session_id, "runtime_id": a.runtime_id});
    match a.kind {
        AlertKind::NeedsYou | AlertKind::Stalled | AlertKind::Finished => {
            v["kind"] = json!("fabric_session");
            v["state"] = json!(a.kind);
        }
        AlertKind::OrderRefused => {
            v["kind"] = json!("fabric_order");
            v["order_id"] = json!(a.order_id);
        }
        _ => {
            v["kind"] = json!("fabric_attention");
            v["attention_kind"] = json!(a.kind);
            v["request_id"] = json!(a.request_id);
            if a.kind == AlertKind::Question && a.choices.as_array().is_some_and(|c| !c.is_empty()) {
                v["choices"] = a.choices.clone();
            }
        }
    }
    v
}
