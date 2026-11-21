//! The hub's record (ADR 0008): the durable events a runtime sends (turns,
//! attention items, results, pairing outcomes), and what the person's own
//! devices submit (signed orders, signed settings, pairing offers, their
//! project switches). Every store call runs in the person's `Scope::User`.
//!
//! - A durable event is written before it is acknowledged, and the stream's
//!   `last_up_seq` moves past it only once it is written: a store that
//!   fails ends the stream (UNAVAILABLE) and the runtime replays it. Every
//!   write is idempotent, so a replay after a crash changes nothing.
//! - A signed order is checked against the device keys the runtime last
//!   reported, then kept byte for byte and sent down as it came. The hub
//!   cannot make one; its checks only keep garbage out of the queue.

use super::hub::{Hub, Refused};
use super::view::{
    AlertKind, AttentionView, Change, ConversationView, OrderView, PairingView, ProjectView, RuntimeInfo, SessionAlert, SessionView, TurnView,
};
use crate::store_ext::with_store;
use taisce_runtime_proto::prost::Message as _;
use serde_json::{Value, json};
use taisce_runtime_proto::signing;
use taisce_runtime_proto::v1 as pb;
use taisce_store::fabric::{FabricAttention, FabricOrder, FabricSettings, FabricTurn};
use taisce_store::{Scope, StoreError};
use uuid::Uuid;

/// The furthest ahead an order's expiry may lie.
pub const MAX_ORDER_LIFE: i64 = 86400;
/// Orders one runtime may have waiting at the hub.
pub const MAX_UNDELIVERED: usize = 256;
/// The text of a `send`, `answer` or `deny`.
pub const MAX_ORDER_TEXT: usize = 16 * 1024;
/// How long a pairing code lives.
pub const PAIRING_LIFE: i64 = 600;
/// The largest sealed pairing offer taken.
pub const MAX_SEALED: usize = 4096;
/// Builtin guardrails a runtime implements (`Guardrail.builtin`).
pub const KNOWN_BUILTINS: [&str; 1] = ["delete_outside_worktree"];

/// A refusal of something a person's device submitted: the HTTP status,
/// the code the app switches on, and words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    /// `stale_settings`: the version the hub holds.
    pub version: Option<u64>,
}

pub(super) fn reject(status: u16, code: &'static str, message: impl Into<String>) -> Reject {
    Reject { status, code, message: message.into(), version: None }
}

fn store_failed(e: StoreError) -> Reject {
    match e {
        StoreError::NotFound(_) => reject(404, "not_found", "not found"),
        e => {
            tracing::warn!("fabric: store error: {e}");
            reject(500, "store", "the hub could not take that; try again")
        }
    }
}

/// `text` as a lowercase canonical UUID, the form signed payloads carry.
pub(super) fn lower_uuid(text: &str) -> Option<Uuid> {
    Uuid::parse_str(text).ok().filter(|u| u.to_string() == text)
}

/// An enum's proto name without its prefix, lowercased: `VERB_SEND` → `send`.
pub(super) fn short_name(full: &str, prefix: &str) -> String {
    full.strip_prefix(prefix).unwrap_or(full).to_ascii_lowercase()
}

fn verb_name(v: i32) -> Option<String> {
    pb::Verb::try_from(v).ok().filter(|v| *v != pb::Verb::Unspecified).map(|v| short_name(v.as_str_name(), "VERB_"))
}

/// A signed envelope as a device posts it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SignedJson {
    pub payload: String,
    pub device_id: String,
    pub signature: String,
}

fn unb64(field: &str, text: &str) -> Result<Vec<u8>, Reject> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|_| reject(400, "malformed", format!("{field}: standard base64")))
}

impl SignedJson {
    fn bytes(&self) -> Result<(Vec<u8>, Vec<u8>), Reject> {
        let payload = unb64("payload", &self.payload)?;
        if payload.len() > signing::MAX_PAYLOAD {
            return Err(reject(400, "malformed", "payload: at most 64 KiB"));
        }
        Ok((payload, unb64("signature", &self.signature)?))
    }
}

fn opened(r: signing::Refusal) -> Reject {
    match r {
        signing::Refusal::BadSignature | signing::Refusal::BadPublicKey => {
            reject(422, "bad_signature", "the signature does not verify against that device's key")
        }
        signing::Refusal::Malformed => reject(400, "malformed", "the payload is not the message it should be"),
        signing::Refusal::NonCanonical => reject(400, "non_canonical", "the payload is not canonically encoded (or carries a field this hub does not know)"),
    }
}

fn device_key(info: &RuntimeInfo, device: &str) -> Result<Vec<u8>, Reject> {
    use base64::Engine as _;
    let d = info.device(device).ok_or_else(|| reject(422, "unknown_device", "that device is not paired with this runtime"))?;
    base64::engine::general_purpose::STANDARD
        .decode(&d.public_key)
        .map_err(|_| reject(422, "unknown_device", "that device's key as the runtime reported it is unreadable"))
}

/// What a runtime's durable event says, checked.
pub enum Durable {
    Turn(Box<pb::Turn>),
    Raised { request: Vec<u8>, signature: Vec<u8>, decoded: Box<pb::AttentionRequest> },
    Cleared(pb::AttentionCleared),
    Result(pb::Result),
    Pairing(pb::Pairing),
}

fn bad(field: &str, what: &str) -> Refused {
    Refused::Invalid(format!("{field}: {what}"))
}

fn text_ok(field: &str, v: &str, min: usize, max: usize) -> Result<(), Refused> {
    if v.len() < min || v.len() > max {
        return Err(bad(field, &format!("{min}-{max} bytes")));
    }
    Ok(())
}

/// Check a durable event's fields (the stream ends INVALID_ARGUMENT,
/// naming the field, when one breaks a limit).
pub fn durable(body: pb::up::Body, runtime: Uuid) -> Result<Durable, Refused> {
    match body {
        pb::up::Body::Turn(t) => {
            Uuid::parse_str(&t.session_id).map_err(|_| bad("turn.session_id", "a UUID"))?;
            text_ok("turn.turn_id", &t.turn_id, 1, 64)?;
            text_ok("turn.prompt", &t.prompt, 0, 32 * 1024)?;
            text_ok("turn.reply", &t.reply, 0, 64 * 1024)?;
            if t.context_used_pct.is_some_and(|p| p > 100) {
                return Err(bad("turn.context_used_pct", "0-100"));
            }
            if let Some(o) = &t.outcome {
                text_ok("turn.outcome.pull_request_url", &o.pull_request_url, 0, 500)?;
            }
            text_ok("turn.order_id", &t.order_id, 0, 64)?;
            Ok(Durable::Turn(Box::new(t)))
        }
        pb::up::Body::Attention(a) => match a.change {
            Some(pb::attention::Change::Raised(r)) => {
                if r.request.len() > signing::MAX_PAYLOAD {
                    return Err(bad("attention.raised.request", "at most 64 KiB"));
                }
                let d = pb::AttentionRequest::decode(&r.request[..]).map_err(|_| bad("attention.raised.request", "an AttentionRequest"))?;
                if Uuid::parse_str(&d.runtime_id).ok() != Some(runtime) {
                    return Err(bad("attention.raised.request.runtime_id", "this runtime's id"));
                }
                Uuid::parse_str(&d.session_id).map_err(|_| bad("attention.raised.request.session_id", "a UUID"))?;
                text_ok("attention.raised.request.request_id", &d.request_id, 1, 64)?;
                text_ok("attention.raised.request.text", &d.text, 1, 8 * 1024)?;
                text_ok("attention.raised.request.tool", &d.tool, 0, 100)?;
                text_ok("attention.raised.request.guardrail_id", &d.guardrail_id, 0, 64)?;
                text_ok("attention.raised.request.guardrail_label", &d.guardrail_label, 0, 100)?;
                text_ok("attention.raised.request.turn_id", &d.turn_id, 0, 64)?;
                if d.choices.len() > 6 {
                    return Err(bad("attention.raised.request.choices", "at most 6"));
                }
                for c in &d.choices {
                    text_ok("attention.raised.request.choices.id", &c.id, 1, 64)?;
                    text_ok("attention.raised.request.choices.label", &c.label, 1, 100)?;
                }
                if r.signature.len() != signing::SIGNATURE_LEN {
                    return Err(bad("attention.raised.signature", "64 bytes"));
                }
                Ok(Durable::Raised { request: r.request, signature: r.signature, decoded: Box::new(d) })
            }
            Some(pb::attention::Change::Cleared(c)) => {
                Uuid::parse_str(&c.session_id).map_err(|_| bad("attention.cleared.session_id", "a UUID"))?;
                text_ok("attention.cleared.request_id", &c.request_id, 1, 64)?;
                Ok(Durable::Cleared(c))
            }
            None => Err(bad("attention", "raised or cleared")),
        },
        pb::up::Body::Result(r) => {
            match &r.subject {
                Some(pb::result::Subject::OrderId(id) | pb::result::Subject::MessageId(id)) => text_ok("result.subject", id, 1, 64)?,
                Some(pb::result::Subject::SettingsVersion(_)) => {}
                None => return Err(bad("result.subject", "an order, a message or a settings version")),
            }
            text_ok("result.reason", &r.reason, 0, 64)?;
            text_ok("result.detail", &r.detail, 0, 500)?;
            Ok(Durable::Result(r))
        }
        pb::up::Body::Pairing(p) => {
            text_ok("pairing.pairing_id", &p.pairing_id, 1, 64)?;
            text_ok("pairing.device_id", &p.device_id, 0, 64)?;
            text_ok("pairing.detail", &p.detail, 0, 500)?;
            Ok(Durable::Pairing(p))
        }
        _ => Err(bad("body", "not a durable event")),
    }
}

/// A RuntimeStatus as the hub keeps it.
pub fn runtime_info(s: &pb::RuntimeStatus) -> Result<RuntimeInfo, Refused> {
    use base64::Engine as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    if !s.public_key.is_empty() && s.public_key.len() != signing::PUBLIC_KEY_LEN {
        return Err(bad("runtime.public_key", "65 bytes"));
    }
    if s.devices.len() > 16 {
        return Err(bad("runtime.devices", "at most 16"));
    }
    let mut devices = vec![];
    for d in &s.devices {
        if d.public_key.len() != signing::PUBLIC_KEY_LEN {
            return Err(bad("runtime.devices.public_key", "65 bytes"));
        }
        let id = signing::device_id(&d.public_key);
        if d.device_id != id {
            return Err(bad("runtime.devices.device_id", "hex SHA-256 of the public key"));
        }
        text_ok("runtime.devices.name", &d.name, 0, 100)?;
        devices.push(super::view::DeviceInfo {
            fingerprint: signing::fingerprint(&id),
            device_id: id,
            name: d.name.clone(),
            public_key: b64(&d.public_key),
            paired_at: d.paired_at,
            counter: d.counter,
        });
    }
    Ok(RuntimeInfo {
        public_key: b64(&s.public_key),
        held: s.held,
        held_since: (s.held && s.held_since > 0).then_some(s.held_since),
        settings_version: s.settings_version,
        present: s.present,
        devices,
    })
}

fn attention_kind(k: i32) -> String {
    pb::AttentionKind::try_from(k)
        .ok()
        .filter(|k| *k != pb::AttentionKind::Unspecified)
        .map(|k| short_name(k.as_str_name(), "ATTENTION_KIND_"))
        .unwrap_or_else(|| "unknown".into())
}

fn resolution_name(r: i32) -> String {
    pb::AttentionResolution::try_from(r)
        .ok()
        .filter(|r| *r != pb::AttentionResolution::Unspecified)
        .map(|r| short_name(r.as_str_name(), "ATTENTION_RESOLUTION_"))
        .unwrap_or_else(|| "withdrawn".into())
}

fn outcome_json(o: Option<&pb::GitOutcome>) -> String {
    let o = o.cloned().unwrap_or_default();
    json!({
        "files_changed": o.files_changed,
        "lines_added": o.lines_added,
        "lines_removed": o.lines_removed,
        "commits": o.commits,
        "pull_request_url": (!o.pull_request_url.is_empty()).then_some(o.pull_request_url),
        "pull_request_number": (o.pull_request_number > 0).then_some(o.pull_request_number),
    })
    .to_string()
}

/// The first line of a command, for a lock screen.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

/// What a durable event led to, applied once its write succeeded.
#[derive(Default)]
struct After {
    changes: Vec<Change>,
    alerts: Vec<SessionAlert>,
    /// An answer was carried out: the decision goes into the fabric's doc.
    decision: Option<(Uuid, String)>,
    /// Message ids to bounce, with why.
    bounce: Vec<(String, String)>,
}

impl Hub {
    /// Who a session is, as the fleet holds it: (address, project, fabric id,
    /// text off). Unknown sessions have empty names.
    fn who(&self, user: Uuid, runtime: Uuid, session: Uuid) -> (String, String, String, bool) {
        let fleets = self.fleets();
        let Some(f) = fleets.get(&user) else { return Default::default() };
        match f.session(runtime, session) {
            Some(s) => (s.address.clone(), s.project.clone(), s.fabric_id.clone(), f.text_off(&s.project)),
            None => Default::default(),
        }
    }

    /// Take a durable event from the stream `conn` of `runtime`: written
    /// before anything is published, and only then counted as taken.
    pub async fn record(&self, user: Uuid, runtime: Uuid, conn: u64, seq: u64, event: Durable, now: i64) -> Result<(), Refused> {
        let runtime_key = {
            let fleets = self.fleets();
            let rt = fleets.get(&user).and_then(|f| f.runtimes.get(&runtime)).ok_or(Refused::NotLive)?;
            if rt.conn.as_ref().map(|c| c.id) != Some(conn) {
                return Err(Refused::NotLive);
            }
            // a replay of what is already written: acknowledged, not applied
            if seq <= rt.row.last_up_seq {
                return Ok(());
            }
            rt.info.public_key.clone()
        };
        let after = self.write_durable(user, runtime, &runtime_key, event, now).await?;
        let alerts = {
            let mut fleets = self.fleets();
            let fleet = fleets.get_mut(&user).ok_or(Refused::NotLive)?;
            if let Some(rt) = fleet.runtimes.get_mut(&runtime) {
                rt.row.last_up_seq = rt.row.last_up_seq.max(seq);
            }
            Self::publish(fleet, after.changes);
            fleet.finish_alerts(after.alerts, now)
        };
        self.alert_all(alerts);
        for (id, why) in after.bounce {
            self.bounce(user, &id, &why, now).await;
        }
        if let Some((session, line)) = after.decision {
            self.record_decision(user, runtime, session, line);
        }
        Ok(())
    }

    pub(super) fn alert_all(&self, alerts: Vec<SessionAlert>) {
        if let Some(alerter) = &self.0.alerter {
            for a in alerts {
                alerter(a);
            }
        }
    }

    async fn write_durable(&self, user: Uuid, runtime: Uuid, runtime_key: &str, event: Durable, now: i64) -> Result<After, Refused> {
        let store = self.0.store.clone();
        let mut after = After::default();
        match event {
            Durable::Turn(t) => {
                let session = Uuid::parse_str(&t.session_id).map_err(|_| bad("turn.session_id", "a UUID"))?;
                let (address, project, _, text_off) = self.who(user, runtime, session);
                let source = pb::PromptSource::try_from(t.source)
                    .ok()
                    .filter(|s| *s != pb::PromptSource::Unspecified)
                    .map(|s| short_name(s.as_str_name(), "PROMPT_SOURCE_"))
                    .unwrap_or_else(|| "unknown".into());
                let end_kind = pb::TurnEnd::try_from(t.end)
                    .ok()
                    .filter(|e| *e != pb::TurnEnd::Unspecified)
                    .map(|e| short_name(e.as_str_name(), "TURN_END_"))
                    .unwrap_or_else(|| "unknown".into());
                let row = FabricTurn {
                    runtime_id: runtime,
                    session_id: session,
                    user_id: user,
                    turn_id: t.turn_id.clone(),
                    address: address.clone(),
                    project: project.clone(),
                    prompt: t.prompt.clone(),
                    reply: t.reply.clone(),
                    text_off,
                    tool_count: t.tool_count,
                    outcome: outcome_json(t.outcome.as_ref()),
                    started: t.started,
                    ended: t.ended,
                    truncated: t.truncated,
                    context_used_pct: t.context_used_pct.map(|p| p as u8),
                    end_kind,
                    source: source.clone(),
                    order_id: (!t.order_id.is_empty()).then(|| t.order_id.clone()),
                    created_at: now,
                };
                // Text off: the live stream shows it, the store never holds it
                let mut stored = row.clone();
                if text_off {
                    stored.prompt.clear();
                    stored.reply.clear();
                }
                let fresh = with_store(&store, Scope::User(user), move |s| s.fabric_turn_put(runtime, &stored)).await.map_err(Refused::Store)?;
                if fresh {
                    after.changes.push(Change::Turn(TurnView::from(&row)));
                    // a turn the person started from the phone tells the
                    // phone when it ends, even while they are at the Mac
                    // (when they are not, the state change already did)
                    let present = self.fleets().get(&user).and_then(|f| f.runtimes.get(&runtime)).is_some_and(|r| r.info.present);
                    if present && matches!(source.as_str(), "order" | "answer") {
                        let mut a = SessionAlert::new(user, runtime, session, &address, AlertKind::Finished);
                        a.preview = t.reply.lines().rev().find(|l| !l.trim().is_empty()).map(|l| first_line(l, 180));
                        a.from_phone = true;
                        after.alerts.push(a);
                    }
                }
            }
            Durable::Raised { request, signature, decoded } => {
                // the phone checks this against the key from the pairing
                // code; a mismatch here is a runtime bug worth saying at once
                if let Ok(key) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, runtime_key)
                    && !key.is_empty()
                    && signing::verify(signing::ATTENTION, &key, &request, &signature).is_err()
                {
                    return Err(bad("attention.raised.signature", "does not verify against RuntimeStatus.public_key"));
                }
                let session = Uuid::parse_str(&decoded.session_id).map_err(|_| bad("attention.raised.request.session_id", "a UUID"))?;
                let (address, project, _, _) = self.who(user, runtime, session);
                let kind = attention_kind(decoded.kind);
                let choices: Vec<Value> = decoded.choices.iter().map(|c| json!({"id": c.id, "label": c.label})).collect();
                let row = FabricAttention {
                    runtime_id: runtime,
                    id: decoded.request_id.clone(),
                    user_id: user,
                    session_id: session,
                    address: address.clone(),
                    project,
                    source: "runtime".into(),
                    kind: kind.clone(),
                    text: decoded.text.clone(),
                    choices: Value::Array(choices.clone()).to_string(),
                    tool: decoded.tool.clone(),
                    guardrail_id: decoded.guardrail_id.clone(),
                    guardrail_label: decoded.guardrail_label.clone(),
                    turn_id: decoded.turn_id.clone(),
                    raised_at: if decoded.raised_at > 0 { decoded.raised_at } else { now },
                    expires_at: (decoded.expires_at > 0).then_some(decoded.expires_at),
                    request: Some(request),
                    signature: Some(signature),
                    conversation_id: None,
                    message_id: None,
                    from_address: None,
                    cleared_at: None,
                    resolution: None,
                    updated_at: now,
                };
                let raised = with_store(&store, Scope::User(user), move |s| s.fabric_attention_raise(runtime, &row)).await.map_err(Refused::Store)?;
                if let Some((row, trimmed)) = raised {
                    for t in &trimmed {
                        after.changes.push(cleared(t));
                    }
                    after.changes.push(Change::Attention(AttentionView::from(&row)));
                    let alert_kind = match kind.as_str() {
                        "approval" => Some(AlertKind::Approval),
                        "question" => Some(AlertKind::Question),
                        "tamper" => Some(AlertKind::NeedsYou),
                        // a dialog is the session's own needs-you state
                        _ => None,
                    };
                    if let Some(k) = alert_kind {
                        let mut a = SessionAlert::new(user, runtime, session, &address, k);
                        a.request_id = Some(row.id.clone());
                        a.preview = Some(match k {
                            AlertKind::Approval => format!("Wants to run: {}", first_line(&row.text, 160)),
                            _ => first_line(&row.text, 240),
                        });
                        if k == AlertKind::Question {
                            a.choices = Value::Array(choices);
                        }
                        after.alerts.push(a);
                    }
                }
            }
            Durable::Cleared(c) => {
                let session = Uuid::parse_str(&c.session_id).map_err(|_| bad("attention.cleared.session_id", "a UUID"))?;
                let (_, _, _, text_off) = self.who(user, runtime, session);
                let resolution = resolution_name(c.resolution);
                let at = if c.at > 0 { c.at } else { now };
                let id = c.request_id.clone();
                let got = with_store(&store, Scope::User(user), move |s| s.fabric_attention_clear(runtime, &id, &resolution, at, text_off))
                    .await
                    .map_err(Refused::Store)?;
                if let Some(a) = got {
                    after.changes.push(cleared(&a));
                }
            }
            Durable::Result(r) => self.take_result(user, runtime, r, now, &mut after).await?,
            Durable::Pairing(p) => {
                let state = pb::PairingOutcome::try_from(p.outcome)
                    .ok()
                    .filter(|o| *o != pb::PairingOutcome::Unspecified)
                    .map(|o| short_name(o.as_str_name(), "PAIRING_OUTCOME_"))
                    .unwrap_or_else(|| "invalid".into());
                let (id, device, detail) = (p.pairing_id.clone(), p.device_id.clone(), p.detail.clone());
                let got = with_store(&store, Scope::User(user), move |s| {
                    s.fabric_pairing_set(runtime, &id, &state, (!device.is_empty()).then_some(device.as_str()), (!detail.is_empty()).then_some(detail.as_str()), now)
                })
                .await
                .map_err(Refused::Store)?;
                if let Some(p) = got {
                    after.changes.push(Change::Pairing(PairingView::from(&p)));
                }
            }
        }
        Ok(after)
    }

    async fn take_result(&self, user: Uuid, runtime: Uuid, r: pb::Result, now: i64, after: &mut After) -> Result<(), Refused> {
        let store = self.0.store.clone();
        let outcome = pb::Outcome::try_from(r.outcome).unwrap_or(pb::Outcome::Unspecified);
        let reason = (!r.reason.is_empty()).then(|| r.reason.clone());
        let detail = (!r.detail.is_empty()).then(|| r.detail.clone());
        match r.subject {
            Some(pb::result::Subject::OrderId(id)) => {
                let state = match outcome {
                    pb::Outcome::Done => "done",
                    pb::Outcome::Queued => "queued",
                    pb::Outcome::Refused => "refused",
                    pb::Outcome::Failed => "failed",
                    pb::Outcome::Unspecified => return Ok(()),
                };
                let fetch = id.clone();
                let Ok(order) = with_store(&store, Scope::User(user), move |s| s.fabric_order(runtime, &fetch)).await else {
                    // an order this hub does not hold (expired here, say): nothing to update
                    return Ok(());
                };
                // the first result wins: a repeat the runtime refused as
                // replayed says nothing new about an order it already answered
                let answered = matches!(order.state.as_str(), "queued" | "done" | "refused" | "failed");
                let finished = matches!(order.state.as_str(), "done" | "refused" | "failed");
                if finished || (answered && reason.as_deref() == Some("replayed")) {
                    return Ok(());
                }
                let final_state = state != "queued";
                let text_off = self.fleets().get(&user).is_some_and(|f| f.text_off(&order.project));
                let (st, rs, dt) = (state.to_string(), reason.clone(), detail.clone());
                let updated = with_store(&store, Scope::User(user), move |s| {
                    s.fabric_order_set(runtime, &id, &st, rs.as_deref(), dt.as_deref(), None, final_state && text_off, now)
                })
                .await
                .map_err(Refused::Store)?;
                let Some(o) = updated else { return Ok(()) };
                after.changes.push(Change::Order(OrderView::from(&o)));
                let session = o.session_id.unwrap_or_default();
                if matches!(state, "refused" | "failed") && reason.as_deref() != Some("cancelled") {
                    let (address, _, _, _) = self.who(user, runtime, session);
                    let mut a = SessionAlert::new(user, runtime, session, &address, AlertKind::OrderRefused);
                    a.order_id = Some(o.id.clone());
                    a.preview = Some(detail.clone().or(reason.clone()).unwrap_or_else(|| state.to_string()));
                    after.alerts.push(a);
                }
                if state == "done" && o.verb.as_deref() == Some("answer") {
                    after.decision = Some((session, o.request_id.clone()));
                }
            }
            Some(pb::result::Subject::MessageId(id)) => {
                let outcome_name = short_name(outcome.as_str_name(), "OUTCOME_");
                let (fetch, rs) = (id.clone(), reason.clone());
                let got = with_store(&store, Scope::User(user), move |s| s.fabric_message_nudged(runtime, &fetch, &outcome_name, rs.as_deref(), now))
                    .await
                    .map_err(Refused::Store)?;
                let Some(m) = got else { return Ok(()) };
                if outcome == pb::Outcome::Refused {
                    match reason.as_deref() {
                        Some("exited") => after.bounce.push((m.id.clone(), "the receiver's session has exited".into())),
                        Some("cold") => self.offer_wake(user, &m, now, after).await?,
                        _ => {}
                    }
                }
            }
            Some(pb::result::Subject::SettingsVersion(v)) if matches!(outcome, pb::Outcome::Refused | pb::Outcome::Failed) => {
                tracing::warn!(runtime = %runtime, version = v, reason = r.reason, "fabric: a runtime refused the settings");
            }
            Some(pb::result::Subject::SettingsVersion(_)) => {}
            None => {}
        }
        Ok(())
    }

    /// An idle session with a full context has mail it was not woken for:
    /// the phone offers a wake.
    async fn offer_wake(&self, user: Uuid, m: &taisce_store::fabric::FabricMessage, now: i64, after: &mut After) -> Result<(), Refused> {
        let row = hub_item(user, &m.to, "wake_offer", &format!("wake:{}", m.id), now, |a| {
            a.message_id = Some(m.id.clone());
            a.from_address = Some(m.from.address.clone());
            a.text = format!("Mail from {} waits; waking it re-reads its whole context.", m.from.address);
        });
        let runtime = m.to.runtime_id;
        let raised = with_store(&self.0.store, Scope::User(user), move |s| s.fabric_attention_raise(runtime, &row)).await.map_err(Refused::Store)?;
        if let Some((row, _)) = raised {
            after.changes.push(Change::Attention(AttentionView::from(&row)));
            let mut a = SessionAlert::new(user, runtime, m.to.session_id, &m.to.address, AlertKind::WakeOffer);
            a.request_id = Some(row.id.clone());
            a.preview = Some(format!("Mail from {}", m.from.address));
            after.alerts.push(a);
        }
        Ok(())
    }

    /// The question an answer was for, written into the session's fabric
    /// doc as a dated decision, through the review gate, as `fabric`.
    fn record_decision(&self, user: Uuid, runtime: Uuid, session: Uuid, request_id: String) {
        let hub = self.clone();
        tokio::spawn(async move {
            let (address, _, fabric_id, text_off) = hub.who(user, runtime, session);
            if fabric_id.is_empty() || text_off {
                return;
            }
            let store = hub.0.store.clone();
            let wrote = with_store(&store, Scope::User(user), move |s| -> Result<bool, StoreError> {
                let Some(settings) = s.fabric_settings()? else { return Ok(false) };
                let json: Value = serde_json::from_str(&settings.json).unwrap_or_default();
                let doc = json["fabrics"]
                    .as_array()
                    .and_then(|fs| fs.iter().find(|f| f["id"] == fabric_id.as_str()))
                    .and_then(|f| f["doc_id"].as_str())
                    .and_then(|d| Uuid::parse_str(d).ok());
                let Some(doc) = doc else { return Ok(false) };
                let item = s.fabric_attention_get(&request_id)?;
                let answer = s
                    .fabric_orders(Some(runtime), Some(session), None, 50)?
                    .into_iter()
                    .find(|o| o.request_id == request_id && o.verb.as_deref() == Some("answer"))
                    .and_then(|o| o.text)
                    .unwrap_or_default();
                let date = chrono::DateTime::from_timestamp(crate::auth::now(), 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
                let line = format!("- {date} · `{address}` asked: {} — answered: {}", first_line(&item.text, 300), first_line(&answer, 300));
                append_to_doc(s, doc, &line)
            })
            .await;
            match wrote {
                Ok(_) => {}
                Err(e) => tracing::warn!("fabric: recording a decision failed: {e}"),
            }
        });
    }

    // ---- what a person's devices submit ----

    /// A signed order from one of the person's devices: checked, kept and
    /// queued for its runtime. The same order again (same id, same bytes)
    /// answers what is held, with `false`.
    pub async fn submit_order(&self, user: Uuid, signed: SignedJson, now: i64) -> Result<(OrderView, bool), Reject> {
        let (payload, signature) = signed.bytes()?;
        let loose = pb::Order::decode(&payload[..]).map_err(|_| reject(400, "malformed", "payload: an Order"))?;
        let runtime = Uuid::parse_str(&loose.runtime_id).map_err(|_| reject(400, "malformed", "runtime_id: a UUID"))?;
        self.sync(user).await.map_err(store_failed)?;
        let info = {
            let fleets = self.fleets();
            let rt = fleets.get(&user).and_then(|f| f.runtimes.get(&runtime)).ok_or_else(|| reject(404, "not_found", "no such runtime"))?;
            rt.info.clone()
        };
        let key = device_key(&info, &signed.device_id)?;
        let order: pb::Order = signing::open(signing::ORDER, &key, &payload, &signature).map_err(opened)?;
        if lower_uuid(&order.runtime_id).is_none() {
            return Err(reject(400, "non_canonical", "runtime_id: a lowercase UUID"));
        }
        if lower_uuid(&order.order_id).is_none() {
            return Err(reject(400, "malformed", "order_id: a lowercase UUID"));
        }
        // the same order again: what is held
        let (oid, held) = (order.order_id.clone(), None::<FabricOrder>);
        let existing = with_store(&self.0.store, Scope::User(user), move |s| match s.fabric_order(runtime, &oid) {
            Ok(o) => Ok(Some(o)),
            Err(StoreError::NotFound(_)) => Ok(held),
            Err(e) => Err(e),
        })
        .await
        .map_err(store_failed)?;
        if let Some(o) = existing {
            let same = o.device_id == signed.device_id && o.counter == order.counter && o.payload.as_ref().is_none_or(|p| *p == payload);
            if same {
                return Ok((OrderView::from(&o), false));
            }
            return Err(reject(409, "order_id_reused", "another order already has that id"));
        }
        if order.expires_at <= now {
            return Err(reject(422, "expired", "expires_at has passed"));
        }
        if order.expires_at > now + MAX_ORDER_LIFE {
            return Err(reject(422, "too_far", "expires_at: at most 24 hours ahead"));
        }
        let mut row = FabricOrder {
            runtime_id: runtime,
            id: order.order_id.clone(),
            user_id: user,
            session_id: None,
            device_id: signed.device_id.clone(),
            counter: order.counter,
            kind: String::new(),
            verb: None,
            hold: None,
            text: None,
            after_turn: String::new(),
            queued: false,
            request_id: String::new(),
            choice_id: String::new(),
            order_ids: "[]".into(),
            issued_at: order.issued_at,
            expires_at: order.expires_at,
            created_at: now,
            updated_at: now,
            payload: Some(payload),
            signature: Some(signature),
            state: "pending".into(),
            reason: None,
            detail: None,
            down_instance: None,
            down_seq: None,
            received_at: None,
            project: String::new(),
        };
        let mut resets = None;
        match &order.what {
            Some(pb::order::What::Hold(h)) => {
                row.kind = "hold".into();
                row.hold = Some(h.on);
            }
            Some(pb::order::What::Command(c)) => {
                row.kind = "command".into();
                let session = lower_uuid(&c.session_id).ok_or_else(|| reject(400, "malformed", "command.session_id: a lowercase UUID"))?;
                let (project, known) = {
                    let fleets = self.fleets();
                    let s = fleets.get(&user).and_then(|f| f.session(runtime, session));
                    (s.map(|s| s.project.clone()).unwrap_or_default(), s.is_some())
                };
                if !known {
                    return Err(reject(404, "no_session", "no such session on that runtime"));
                }
                let verb = verb_name(c.verb).filter(|v| v != "spawn").ok_or_else(|| reject(422, "unsupported", "that verb is not one this hub takes"))?;
                match verb.as_str() {
                    "send" | "answer" => {
                        if c.text.is_empty() {
                            return Err(reject(422, "too_long", "text: 1 byte to 16 KiB"));
                        }
                        if c.text.starts_with('!') || c.text.starts_with('/') {
                            return Err(reject(422, "unsafe_text", "text may not start with ! or /"));
                        }
                    }
                    _ => {}
                }
                if c.text.len() > MAX_ORDER_TEXT {
                    return Err(reject(422, "too_long", "text: at most 16 KiB"));
                }
                if matches!(verb.as_str(), "approve" | "deny" | "answer") && (c.request_id.is_empty() || c.request_hash.len() != 32) {
                    return Err(reject(400, "malformed", "request_id and a 32-byte request_hash"));
                }
                if matches!(verb.as_str(), "cancel" | "reorder") && (c.order_ids.is_empty() || c.order_ids.len() > 32) {
                    return Err(reject(400, "malformed", "order_ids: 1 to 32"));
                }
                if [&c.after_turn, &c.request_id, &c.choice_id].iter().any(|v| v.len() > 64) || c.order_ids.iter().any(|v| v.len() > 64) {
                    return Err(reject(400, "malformed", "ids: at most 64 bytes"));
                }
                if matches!(verb.as_str(), "send" | "answer" | "wake") {
                    resets = Some(session);
                }
                row.session_id = Some(session);
                row.project = project;
                row.verb = Some(verb);
                row.text = Some(c.text.clone());
                row.after_turn = c.after_turn.clone();
                row.queued = c.queued;
                row.request_id = c.request_id.clone();
                row.choice_id = c.choice_id.clone();
                row.order_ids = serde_json::to_string(&c.order_ids).unwrap_or_else(|_| "[]".into());
            }
            None => return Err(reject(422, "unsupported", "an order carries a command or a hold")),
        }
        let device_counter = info.device(&signed.device_id).map(|d| d.counter).unwrap_or(0);
        let device = signed.device_id.clone();
        let stored = with_store(&self.0.store, Scope::User(user), move |s| -> Result<Result<FabricOrder, Reject>, StoreError> {
            let held = s.fabric_order_max_counter(runtime, &device)?.max(device_counter);
            if row.counter <= held {
                return Ok(Err(reject(409, "replayed", format!("counter: above {held}"))));
            }
            if s.fabric_orders_undelivered(runtime)?.len() >= MAX_UNDELIVERED {
                return Ok(Err(reject(429, "queue_full", "256 orders already wait for that runtime")));
            }
            Ok(Ok(s.fabric_order_insert(&row)?))
        })
        .await
        .map_err(store_failed)??;
        tracing::info!(target: crate::auth::AUDIT, event = "fabric.order", user = %user, runtime = %runtime, order = stored.id,
            device = stored.device_id, kind = stored.kind, verb = ?stored.verb);
        let mut changes = vec![Change::Order(OrderView::from(&stored))];
        if let Some(session) = resets {
            let reset = with_store(&self.0.store, Scope::User(user), move |s| s.fabric_conversations_reset(runtime, session, now)).await.unwrap_or_default();
            changes.extend(reset.iter().map(|c| Change::Conversation(ConversationView::from(c))));
        }
        {
            let mut fleets = self.fleets();
            if let Some(fleet) = fleets.get_mut(&user) {
                if let Some(rt) = fleet.runtimes.get(&runtime) {
                    rt.ring();
                }
                Self::publish(fleet, changes);
            }
        }
        Ok((OrderView::from(&stored), true))
    }

    /// Withdraw an order the hub has not sent yet.
    pub async fn withdraw_order(&self, user: Uuid, id: &str, now: i64) -> Result<OrderView, Reject> {
        let id = id.to_string();
        let got = with_store(&self.0.store, Scope::User(user), move |s| -> Result<Option<FabricOrder>, StoreError> {
            let o = s.fabric_order_find(&id)?;
            s.fabric_order_set(o.runtime_id, &id, "withdrawn", Some("withdrawn"), None, Some(&["pending"]), true, now)
        })
        .await
        .map_err(store_failed)?;
        let o = got.ok_or_else(|| reject(409, "already_sent", "it has gone down to the runtime: send a cancel"))?;
        self.publish_to(user, vec![Change::Order(OrderView::from(&o))]);
        Ok(OrderView::from(&o))
    }

    pub(super) fn publish_to(&self, user: Uuid, changes: Vec<Change>) {
        let mut fleets = self.fleets();
        if let Some(fleet) = fleets.get_mut(&user) {
            Self::publish(fleet, changes);
        }
    }

    /// New signed settings from one of the person's devices.
    pub async fn put_settings(&self, user: Uuid, signed: SignedJson, now: i64) -> Result<Value, Reject> {
        let (payload, signature) = signed.bytes()?;
        self.sync(user).await.map_err(store_failed)?;
        let key = {
            let fleets = self.fleets();
            let f = fleets.get(&user).ok_or_else(|| reject(422, "unknown_device", "that device is not paired with any of your runtimes"))?;
            let info = f
                .runtimes
                .values()
                .find(|r| r.info.device(&signed.device_id).is_some())
                .map(|r| r.info.clone())
                .ok_or_else(|| reject(422, "unknown_device", "that device is not paired with any of your runtimes"))?;
            device_key(&info, &signed.device_id)?
        };
        let settings: pb::Settings = signing::open(signing::SETTINGS, &key, &payload, &signature).map_err(opened)?;
        let json = settings_json(&settings)?;
        let row = FabricSettings {
            user_id: user,
            version: settings.version,
            issued_at: settings.issued_at,
            payload,
            device_id: signed.device_id.clone(),
            signature,
            json: json.to_string(),
            updated_at: now,
        };
        let put = with_store(&self.0.store, Scope::User(user), move |s| match s.fabric_settings_put(&row) {
            Ok(()) => Ok(Ok(())),
            Err(StoreError::InvalidOp(m)) => Ok(Err(m)),
            Err(e) => Err(e),
        })
        .await
        .map_err(store_failed)?;
        if let Err(m) = put {
            let held = with_store(&self.0.store, Scope::User(user), |s| s.fabric_settings()).await.ok().flatten().map(|s| s.version).unwrap_or(0);
            return Err(Reject { status: 409, code: "stale_settings", message: m, version: Some(held) });
        }
        tracing::info!(target: crate::auth::AUDIT, event = "fabric.settings", user = %user, version = settings.version, device = signed.device_id);
        let view = self.settings_view(user).await.map_err(store_failed)?;
        {
            let mut fleets = self.fleets();
            if let Some(fleet) = fleets.get_mut(&user) {
                for rt in fleet.runtimes.values() {
                    rt.ring();
                }
                Self::publish(fleet, vec![Change::Settings(view.clone())]);
            }
        }
        Ok(view)
    }

    /// The person's settings as the routes show them: the newest signed
    /// ones, or the defaults every runtime starts with.
    pub async fn settings_view(&self, user: Uuid) -> Result<Value, StoreError> {
        self.sync(user).await?;
        let held = with_store(&self.0.store, Scope::User(user), |s| s.fabric_settings()).await?;
        let runtimes: Vec<Value> = self
            .fleets()
            .get(&user)
            .map(|f| f.runtimes.values().map(|r| json!({"runtime_id": r.row.id, "settings_version": r.info.settings_version})).collect())
            .unwrap_or_default();
        Ok(match held {
            Some(s) => {
                let settings: Value = serde_json::from_str(&s.json).unwrap_or_default();
                json!({
                    "version": s.version,
                    "issued_at": s.issued_at,
                    "defaults": false,
                    "settings": settings,
                    "signed": {"payload": super::view::b64(&s.payload), "device_id": s.device_id, "signature": super::view::b64(&s.signature)},
                    "runtimes": runtimes,
                })
            }
            None => {
                let defaults: Value = serde_json::from_str(taisce_runtime_proto::DEFAULT_SETTINGS_JSON).unwrap_or_default();
                json!({
                    "version": 0,
                    "issued_at": null,
                    "defaults": true,
                    "settings": defaults["settings"],
                    "signed": null,
                    "runtimes": runtimes,
                })
            }
        })
    }

    /// The newest signed settings, when a runtime holds an older version.
    pub(super) async fn settings_for(&self, user: Uuid, runtime_version: u64) -> Option<(u64, pb::Signed)> {
        let held = with_store(&self.0.store, Scope::User(user), |s| s.fabric_settings()).await.ok().flatten()?;
        (held.version > runtime_version).then_some((held.version, pb::Signed { payload: held.payload, device_id: held.device_id, signature: held.signature }))
    }

    /// A pairing offer from the person's phone, for one of their runtimes.
    pub async fn offer_pairing(&self, user: Uuid, runtime: Uuid, pairing_id: &str, sealed: &str, now: i64) -> Result<PairingView, Reject> {
        if lower_uuid(pairing_id).is_none() {
            return Err(reject(400, "malformed", "pairing_id: a lowercase UUID"));
        }
        let sealed = unb64("sealed", sealed)?;
        if sealed.len() > MAX_SEALED || sealed.len() < 12 + 16 + 1 {
            return Err(reject(400, "malformed", "sealed: nonce, ciphertext and tag, at most 4 KiB"));
        }
        self.sync(user).await.map_err(store_failed)?;
        let id = pairing_id.to_string();
        let made = with_store(&self.0.store, Scope::User(user), move |s| match s.fabric_pairing_create(runtime, &id, &sealed, now, now + PAIRING_LIFE) {
            Ok(p) => Ok(Ok(p)),
            Err(StoreError::InvalidOp(_)) => Ok(Err(reject(409, "pairing_id_reused", "that pairing code was used already"))),
            Err(e) => Err(e),
        })
        .await
        .map_err(store_failed)??;
        let view = PairingView::from(&made);
        {
            let mut fleets = self.fleets();
            if let Some(fleet) = fleets.get_mut(&user) {
                if let Some(rt) = fleet.runtimes.get(&runtime) {
                    rt.ring();
                }
                Self::publish(fleet, vec![Change::Pairing(view.clone())]);
            }
        }
        Ok(view)
    }

    /// Set a project's switches; its sessions are shown again with the new
    /// Text off mark.
    pub async fn set_project(&self, user: Uuid, name: &str, text_off: Option<bool>, private: Option<bool>, now: i64) -> Result<ProjectView, Reject> {
        self.sync(user).await.map_err(store_failed)?;
        let n = name.to_string();
        let p = with_store(&self.0.store, Scope::User(user), move |s| s.fabric_project_put(&n, text_off, private, now)).await.map_err(store_failed)?;
        let mut fleets = self.fleets();
        let fleet = fleets.entry(user).or_default();
        fleet.projects.insert(p.name.clone(), p.clone());
        let sessions: Vec<SessionView> = fleet
            .runtimes
            .values()
            .flat_map(|r| r.sessions.values().map(|s| &s.row))
            .filter(|s| s.project == p.name)
            .map(|s| SessionView::of(s, p.text_off))
            .collect();
        let view = ProjectView::of(&p, sessions.len());
        let mut changes = vec![Change::Project(view.clone())];
        changes.extend(sessions.into_iter().map(Change::Session));
        Self::publish(fleet, changes);
        Ok(view)
    }

    /// The person's projects: every project a session names, and any they
    /// have set a switch for.
    pub async fn projects(&self, user: Uuid) -> Result<Vec<ProjectView>, StoreError> {
        self.sync(user).await?;
        let fleets = self.fleets();
        let Some(fleet) = fleets.get(&user) else { return Ok(vec![]) };
        let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for s in fleet.runtimes.values().flat_map(|r| r.sessions.values()) {
            if !s.row.project.is_empty() {
                *counts.entry(s.row.project.clone()).or_default() += 1;
            }
        }
        for name in fleet.projects.keys() {
            counts.entry(name.clone()).or_default();
        }
        Ok(counts
            .into_iter()
            .map(|(name, n)| match fleet.projects.get(&name) {
                Some(p) => ProjectView::of(p, n),
                None => ProjectView { name, text_off: false, private_notifications: false, sessions: n },
            })
            .collect())
    }

    /// Follow a session (or stop): every finished turn of it alerts.
    pub async fn follow(&self, user: Uuid, runtime: Uuid, session: Uuid, following: bool) -> Result<SessionView, StoreError> {
        let gone = || StoreError::NotFound(format!("session {session}"));
        self.sync(user).await?;
        let writing = self
            .fleets()
            .get(&user)
            .and_then(|f| f.runtimes.get(&runtime))
            .filter(|r| r.sessions.contains_key(&session))
            .map(|r| r.writing.clone())
            .ok_or_else(gone)?;
        self.write_runtime(user, runtime).await;
        let _turn = writing.lock().await;
        with_store(&self.0.store, Scope::User(user), move |s| s.fabric_session_follow(runtime, session, following).map(|_| ())).await?;
        let mut fleets = self.fleets();
        let fleet = fleets.get_mut(&user).ok_or_else(gone)?;
        let text_off = fleet.session(runtime, session).is_some_and(|s| fleet.text_off(&s.project));
        let slot = fleet.runtimes.get_mut(&runtime).and_then(|r| r.sessions.get_mut(&session)).ok_or_else(gone)?;
        let changed = slot.row.following != following;
        slot.row.following = following;
        let view = SessionView::of(&slot.row, text_off);
        if changed {
            Self::publish(fleet, vec![Change::Session(view.clone())]);
        }
        Ok(view)
    }

    /// Dismiss one of the hub's own attention items (a wake offer, an ask
    /// waiting). A runtime's item is answered by an order, not dismissed.
    pub async fn dismiss(&self, user: Uuid, id: &str, now: i64) -> Result<(), Reject> {
        let id = id.to_string();
        let got = with_store(&self.0.store, Scope::User(user), move |s| -> Result<Result<Option<FabricAttention>, Reject>, StoreError> {
            let a = s.fabric_attention_get(&id)?;
            if a.source != "hub" {
                return Ok(Err(reject(409, "answer_it", "a runtime's item is answered by an order")));
            }
            Ok(Ok(s.fabric_attention_clear(a.runtime_id, &id, "dismissed", now, false)?))
        })
        .await
        .map_err(store_failed)??;
        if let Some(a) = got {
            self.publish_to(user, vec![cleared(&a)]);
        }
        Ok(())
    }

    /// The snapshot that opened a stream said which items are still open:
    /// any other this runtime raised is over.
    pub async fn clear_stale_attention(&self, user: Uuid, runtime: Uuid, open: Vec<String>, now: i64) -> Result<(), Refused> {
        let gone = with_store(&self.0.store, Scope::User(user), move |s| s.fabric_attention_clear_others(runtime, &open, now)).await.map_err(Refused::Store)?;
        if !gone.is_empty() {
            self.publish_to(user, gone.iter().map(cleared).collect());
        }
        Ok(())
    }
}

/// The change saying an item is over.
pub(super) fn cleared(a: &FabricAttention) -> Change {
    Change::AttentionCleared {
        id: a.id.clone(),
        runtime_id: a.runtime_id,
        session_id: a.session_id,
        resolution: a.resolution.clone().unwrap_or_else(|| "withdrawn".into()),
    }
}

/// An item the hub raises itself, about one session.
pub(super) fn hub_item(
    user: Uuid,
    about: &taisce_store::fabric::Party,
    kind: &str,
    id: &str,
    now: i64,
    fill: impl FnOnce(&mut FabricAttention),
) -> FabricAttention {
    let mut a = FabricAttention {
        runtime_id: about.runtime_id,
        id: id.to_string(),
        user_id: user,
        session_id: about.session_id,
        address: about.address.clone(),
        project: String::new(),
        source: "hub".into(),
        kind: kind.into(),
        text: String::new(),
        choices: "[]".into(),
        tool: String::new(),
        guardrail_id: String::new(),
        guardrail_label: String::new(),
        turn_id: String::new(),
        raised_at: now,
        expires_at: None,
        request: None,
        signature: None,
        conversation_id: None,
        message_id: None,
        from_address: None,
        cleared_at: None,
        resolution: None,
        updated_at: now,
    };
    fill(&mut a);
    a
}

/// Append one markdown line at the end of `doc`, as the `fabric` agent,
/// through the review gate. False when there was nothing to change.
fn append_to_doc(s: &mut taisce_store::SqliteStore, doc: Uuid, line: &str) -> Result<bool, StoreError> {
    use taisce_store::BlockStore as _;
    let principal = crate::mcp::agent_principal_by_name(s, "fabric").map_err(StoreError::InvalidOp)?;
    let tree = s.read_doc(doc)?;
    let epoch = tree.doc.current_epoch;
    let (export, _) = taisce_store::locate::export_with_offsets(&tree.roots);
    let mut text = String::new();
    if !export.is_empty() && !export.ends_with("\n\n") {
        text.push_str(if export.ends_with('\n') { "\n" } else { "\n\n" });
    }
    text.push_str(line);
    let new_md = taisce_store::locate::splice_insert(&export, export.len(), &text);
    let ops = taisce_store::mddiff::markdown_to_ops_from(&tree.roots, &new_md, "fabric");
    if ops.is_empty() {
        return Ok(false);
    }
    s.propose(doc, epoch, principal, ops)?;
    Ok(true)
}

/// Check settings and render them as the routes show them: proto names,
/// every limit at its effective value.
pub fn settings_json(s: &pb::Settings) -> Result<Value, Reject> {
    let invalid = |m: String| reject(422, "invalid", m);
    if s.guardrails.len() > 100 {
        return Err(invalid("guardrails: at most 100".into()));
    }
    if s.fabrics.len() > 50 {
        return Err(invalid("fabrics: at most 50".into()));
    }
    let mut rails = vec![];
    let mut ids = std::collections::HashSet::new();
    for g in &s.guardrails {
        if g.id.is_empty() || g.id.len() > 64 || !ids.insert(g.id.clone()) {
            return Err(invalid(format!("guardrail id {:?}: 1-64 bytes, unique", g.id)));
        }
        if g.label.len() > 100 || g.tools.len() > 16 || g.tools.iter().any(|t| t.is_empty() || t.len() > 64) {
            return Err(invalid(format!("guardrail {}: label at most 100 bytes, at most 16 tools", g.id)));
        }
        match (g.pattern.is_empty(), g.builtin.is_empty()) {
            (false, true) => {
                if g.pattern.len() > 1000 {
                    return Err(invalid(format!("guardrail {}: pattern at most 1000 bytes", g.id)));
                }
                regex::Regex::new(&g.pattern).map_err(|e| invalid(format!("guardrail {}: the pattern does not compile: {e}", g.id)))?;
            }
            (true, false) => {
                if !KNOWN_BUILTINS.contains(&g.builtin.as_str()) {
                    return Err(invalid(format!("guardrail {}: no builtin {:?}", g.id, g.builtin)));
                }
            }
            _ => return Err(invalid(format!("guardrail {}: a pattern or a builtin, not both or neither", g.id))),
        }
        rails.push(json!({"id": g.id, "label": g.label, "enabled": g.enabled, "tools": g.tools, "pattern": g.pattern, "builtin": g.builtin}));
    }
    let mut fabrics = vec![];
    let mut names = std::collections::HashSet::new();
    let mut fids = std::collections::HashSet::new();
    for f in &s.fabrics {
        if lower_uuid(&f.id).is_none() || !fids.insert(f.id.clone()) {
            return Err(invalid(format!("fabric id {:?}: a lowercase UUID, unique", f.id)));
        }
        if f.name.is_empty() || f.name.len() > 100 || f.name.contains('/') || f.name.contains(':') || !names.insert(f.name.clone()) {
            return Err(invalid(format!("fabric name {:?}: 1-100 bytes, no / or :, unique", f.name)));
        }
        if f.purpose.len() > 500 || f.doc_title.len() > 200 || !(f.doc_id.is_empty() || Uuid::parse_str(&f.doc_id).is_ok()) {
            return Err(invalid(format!("fabric {}: purpose at most 500 bytes, doc_id a UUID", f.name)));
        }
        fabrics.push(json!({"id": f.id, "name": f.name, "purpose": f.purpose, "doc_id": f.doc_id, "doc_title": f.doc_title}));
    }
    let l = s.limits.unwrap_or_default();
    let or = |v: u32, d: u32| if v == 0 { d } else { v };
    Ok(json!({
        "version": s.version,
        "issued_at": s.issued_at,
        "guardrails": rails,
        "limits": {
            "pair_messages": or(l.pair_messages, 12),
            "agent_turns_per_hour": or(l.agent_turns_per_hour, 20),
            "wakes_per_session_per_hour": or(l.wakes_per_session_per_hour, 4),
            "cold_context_pct": or(l.cold_context_pct, 80),
            "cold_idle_minutes": or(l.cold_idle_minutes, 30),
        },
        "fabrics": fabrics,
    }))
}
