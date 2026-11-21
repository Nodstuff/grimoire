//! What the fake runtime does for Slices 2-5 (ADR 0008): the runtime's own
//! state ([`Side`]), the events it plays (status, turns, signed attention
//! requests, clears), what it does with what comes down (orders checked in
//! the contract's order and answered; settings taken by version; pairing
//! offers opened with the code's secret and confirmed as if at the Mac;
//! nudges answered by reading the mail), and fabric tool calls.
//!
//! It types nothing. A `send` it accepts is answered done and played back
//! as a finished turn whose prompt is the order's text, so a phone sees the
//! whole round trip.

use super::{Error, Fake, ScriptSession, now};
use crate::signing;
use crate::v1::{self as pb, Ack, Down, Up, down, up};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use prost::Message as _;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;

/// A UUID-shaped id (lowercase, version 4 layout) from 16 bytes.
fn uuid_text(b: &[u8]) -> String {
    let mut b: [u8; 16] = b[..16].try_into().unwrap_or([0; 16]);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn b64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// What a runtime keeps, across its streams.
pub struct Side {
    /// As the Welcome said.
    pub runtime_id: String,
    key: SigningKey,
    /// A seed every id this side makes is drawn from.
    seed: String,
    made: u64,
    pub devices: Vec<pb::PairedDevice>,
    pub held: bool,
    held_since: i64,
    pub settings: Option<pb::Settings>,
    pub settings_version: u64,
    pub present: bool,
    /// Every session reported, by id, as last reported.
    pub sessions: HashMap<String, ScriptSession>,
    /// Open attention items: the request as signed.
    pub open: HashMap<String, (pb::AttentionRequest, Vec<u8>)>,
    /// Pairing codes shown: pairing id → secret.
    pairings: HashMap<String, [u8; 32]>,
    /// What happened, one line each: for a person watching, and for tests.
    pub log: Vec<String>,
}

impl Side {
    /// A runtime whose key and ids come from `seed` (the same seed, the
    /// same key: a fake, not a secret).
    pub fn new(seed: &str) -> Self {
        let d: [u8; 32] = Sha256::digest(format!("fake runtime key: {seed}")).into();
        Side {
            runtime_id: String::new(),
            key: SigningKey::from_slice(&d).expect("a SHA-256 is a valid P-256 scalar but for 2^-128 of seeds"),
            seed: seed.to_string(),
            made: 0,
            devices: vec![],
            held: false,
            held_since: 0,
            settings: None,
            settings_version: 0,
            present: false,
            sessions: HashMap::new(),
            open: HashMap::new(),
            pairings: HashMap::new(),
            log: vec![],
        }
    }

    /// The runtime's signing key: 65 bytes, X9.63.
    pub fn public_key(&self) -> Vec<u8> {
        self.key.verifying_key().to_sec1_point(false).as_bytes().to_vec()
    }

    /// A fresh id.
    pub fn id(&mut self) -> String {
        self.made += 1;
        uuid_text(&Sha256::digest(format!("{} {} {}", self.seed, self.made, now())))
    }

    /// Pair a device directly, as if confirmed at the Mac (tests).
    pub fn pair(&mut self, public_key: &[u8], name: &str) -> String {
        let id = signing::device_id(public_key);
        self.devices.retain(|d| d.device_id != id);
        self.devices.push(pb::PairedDevice { device_id: id.clone(), public_key: public_key.to_vec(), name: name.into(), paired_at: now(), counter: 0 });
        id
    }

    fn status(&self) -> pb::RuntimeStatus {
        pb::RuntimeStatus {
            public_key: self.public_key(),
            held: self.held,
            held_since: if self.held { self.held_since } else { 0 },
            settings_version: self.settings_version,
            devices: self.devices.clone(),
            present: self.present,
        }
    }

    fn sign(&self, context: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut m = context.to_vec();
        m.extend_from_slice(payload);
        let sig: Signature = self.key.sign(&m);
        sig.to_bytes().to_vec()
    }
}

/// RuntimeStatus fields a script sets.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptStatus {
    pub present: bool,
    pub held: Option<bool>,
}

/// A finished turn, in a script.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptTurn {
    /// The session's id.
    pub session: String,
    /// Made up when absent.
    pub turn_id: Option<String>,
    pub prompt: String,
    pub reply: String,
    pub tool_count: u32,
    pub files_changed: u32,
    pub lines_added: u32,
    pub lines_removed: u32,
    pub commits: u32,
    pub pull_request_url: String,
    pub pull_request_number: u32,
    /// clean | question | error | interrupted | denied (default clean)
    pub end: String,
    /// mac | order | answer | wake (default mac)
    pub source: String,
    pub order_id: String,
    pub context_used_pct: Option<u32>,
}

/// An approval or a question, in a script; signed by the fake's key.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptAttention {
    pub session: String,
    /// Made up when absent.
    pub request_id: Option<String>,
    /// approval | question | dialog | tamper
    pub kind: String,
    pub text: String,
    pub tool: String,
    pub guardrail_id: String,
    pub guardrail_label: String,
    /// `[{"id": "fix", "label": "Fix the test"}]`
    pub choices: Vec<ScriptChoice>,
    /// Seconds from now the runtime would give up (0: no deadline).
    pub expires_in: i64,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptChoice {
    pub id: String,
    pub label: String,
}

/// A fabric `send`, in a script.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptSend {
    /// The sending session's id.
    pub from: String,
    pub to: String,
    /// note | ask | reply
    pub kind: String,
    pub body: String,
    pub doc: String,
    pub reply_to: String,
}

/// What handling one Down event came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Handled {
    Order { order_id: String, outcome: pb::Outcome, reason: String },
    Settings { version: u64, accepted: bool },
    Pairing { pairing_id: String, outcome: pb::PairingOutcome, device_id: String },
    /// A nudge: the mail it read.
    Nudge { message_id: String, read: Vec<pb::Mail> },
    Other,
}

fn named<T>(name: &str, prefix: &str, all: &[T], as_str: impl Fn(&T) -> &'static str) -> Option<usize> {
    let want = format!("{prefix}{}", name.to_ascii_uppercase());
    all.iter().position(|v| as_str(v) == want)
}

impl Fake {
    /// Say what this runtime is now (RuntimeStatus).
    pub async fn status(&mut self) -> Result<(), Error> {
        let s = self.side.status();
        self.event(up::Body::Runtime(s)).await
    }

    /// Play a finished turn; the session's latest turn becomes it.
    pub async fn turn(&mut self, t: &ScriptTurn) -> Result<(), Error> {
        let turn_id = t.turn_id.clone().unwrap_or_else(|| self.side.id());
        let ends = [pb::TurnEnd::Clean, pb::TurnEnd::Question, pb::TurnEnd::Error, pb::TurnEnd::Interrupted, pb::TurnEnd::Denied];
        let end = named(if t.end.is_empty() { "clean" } else { &t.end }, "TURN_END_", &ends, |e| e.as_str_name()).map(|i| ends[i] as i32).unwrap_or(0);
        let sources = [pb::PromptSource::Mac, pb::PromptSource::Order, pb::PromptSource::Answer, pb::PromptSource::Wake];
        let source =
            named(if t.source.is_empty() { "mac" } else { &t.source }, "PROMPT_SOURCE_", &sources, |e| e.as_str_name()).map(|i| sources[i] as i32).unwrap_or(0);
        let ended = now();
        let turn = pb::Turn {
            session_id: t.session.clone(),
            turn_id: turn_id.clone(),
            prompt: t.prompt.clone(),
            reply: t.reply.clone(),
            tool_count: t.tool_count,
            outcome: Some(pb::GitOutcome {
                files_changed: t.files_changed,
                lines_added: t.lines_added,
                lines_removed: t.lines_removed,
                commits: t.commits,
                pull_request_url: t.pull_request_url.clone(),
                pull_request_number: t.pull_request_number,
            }),
            started: ended - 30,
            ended,
            truncated: false,
            context_used_pct: t.context_used_pct,
            end,
            source,
            order_id: t.order_id.clone(),
        };
        self.event(up::Body::Turn(turn)).await?;
        if let Some(mut s) = self.side.sessions.get(&t.session).cloned() {
            s.turn_id = turn_id;
            s.since = Some(ended);
            if s.state == "working" {
                s.state = "finished".into();
            }
            self.state(&s).await?;
        }
        Ok(())
    }

    /// Raise an approval or a question, signed with this runtime's key.
    /// Returns its request id.
    pub async fn ask(&mut self, a: &ScriptAttention) -> Result<String, Error> {
        let kinds = [pb::AttentionKind::Approval, pb::AttentionKind::Question, pb::AttentionKind::Dialog, pb::AttentionKind::Tamper];
        let kind = named(&a.kind, "ATTENTION_KIND_", &kinds, |e| e.as_str_name()).ok_or_else(|| Error::Script(format!("unknown kind {:?}", a.kind)))?;
        let request_id = a.request_id.clone().unwrap_or_else(|| self.side.id());
        let req = pb::AttentionRequest {
            runtime_id: self.side.runtime_id.clone(),
            session_id: a.session.clone(),
            request_id: request_id.clone(),
            kind: kinds[kind] as i32,
            text: a.text.clone(),
            choices: a.choices.iter().map(|c| pb::Choice { id: c.id.clone(), label: c.label.clone() }).collect(),
            tool: a.tool.clone(),
            guardrail_id: a.guardrail_id.clone(),
            guardrail_label: a.guardrail_label.clone(),
            turn_id: self.side.sessions.get(&a.session).map(|s| s.turn_id.clone()).unwrap_or_default(),
            raised_at: now(),
            expires_at: if a.expires_in > 0 { now() + a.expires_in } else { 0 },
        };
        let bytes = req.encode_to_vec();
        let signature = self.side.sign(signing::ATTENTION, &bytes);
        self.side.open.insert(request_id.clone(), (req, bytes.clone()));
        self.event(up::Body::Attention(pb::Attention {
            change: Some(pb::attention::Change::Raised(pb::SignedRequest { request: bytes, signature })),
        }))
        .await?;
        Ok(request_id)
    }

    /// An item is over (`resolution`: approved, denied, answered, at_mac,
    /// timed_out, withdrawn).
    pub async fn clear(&mut self, session: &str, request_id: &str, resolution: &str) -> Result<(), Error> {
        let all = [
            pb::AttentionResolution::Approved,
            pb::AttentionResolution::Denied,
            pb::AttentionResolution::Answered,
            pb::AttentionResolution::AtMac,
            pb::AttentionResolution::TimedOut,
            pb::AttentionResolution::Withdrawn,
        ];
        let r = named(resolution, "ATTENTION_RESOLUTION_", &all, |e| e.as_str_name()).map(|i| all[i] as i32).unwrap_or(0);
        self.side.open.remove(request_id);
        self.event(up::Body::Attention(pb::Attention {
            change: Some(pb::attention::Change::Cleared(pb::AttentionCleared {
                session_id: session.into(),
                request_id: request_id.into(),
                resolution: r,
                at: now(),
            })),
        }))
        .await
    }

    async fn result(&mut self, subject: pb::result::Subject, outcome: pb::Outcome, reason: &str, detail: &str) -> Result<(), Error> {
        self.event(up::Body::Result(pb::Result { subject: Some(subject), outcome: outcome as i32, reason: reason.into(), detail: detail.into(), at: now() }))
            .await
    }

    /// One fabric tool call on `session`'s behalf.
    pub async fn call(&mut self, session: &str, tool: pb::call_request::Tool) -> Result<pb::CallReply, Error> {
        let call_id = self.side.id();
        self.call_as(session, &call_id, tool).await
    }

    /// A call with a given id (a retry sends the same one).
    pub async fn call_as(&mut self, session: &str, call_id: &str, tool: pb::call_request::Tool) -> Result<pb::CallReply, Error> {
        let mut req = tonic::Request::new(pb::CallRequest { call_id: call_id.into(), session_id: session.into(), tool: Some(tool) });
        let value = format!("Bearer {}", self.credential).parse().map_err(|_| Error::Transport("the credential is not a header value".into()))?;
        req.metadata_mut().insert("authorization", value);
        Ok(self.client.call(req).await.map_err(Error::Status)?.into_inner())
    }

    /// `send`, from a script.
    pub async fn send_mail(&mut self, m: &ScriptSend) -> Result<pb::SendReply, Error> {
        let kinds = [pb::MessageKind::Note, pb::MessageKind::Ask, pb::MessageKind::Reply];
        let kind = named(&m.kind, "MESSAGE_KIND_", &kinds, |e| e.as_str_name()).map(|i| kinds[i] as i32).unwrap_or(0);
        let tool = pb::call_request::Tool::Send(pb::SendRequest { to: m.to.clone(), kind, body: m.body.clone(), doc: m.doc.clone(), reply_to: m.reply_to.clone() });
        match self.call(&m.from, tool).await?.reply {
            Some(pb::call_reply::Reply::Send(r)) => Ok(r),
            other => Err(Error::Protocol(format!("send answered {other:?}"))),
        }
    }

    /// A pairing code a phone can scan (as text): good for 10 minutes.
    pub fn pairing_code(&mut self, hub_host: &str) -> String {
        let pairing_id = self.side.id();
        let secret: [u8; 32] = Sha256::digest(format!("fake pairing secret {} {pairing_id}", self.side.seed)).into();
        self.side.pairings.insert(pairing_id.clone(), secret);
        format!(
            "taisce-pair:?v=1&r={}&p={pairing_id}&k={}&s={}&h={hub_host}",
            self.side.runtime_id,
            b64url(&self.side.public_key()),
            b64url(&secret)
        )
    }

    /// The next Down event, if one comes within `wait`.
    pub async fn next_down(&mut self, wait: Duration) -> Option<Down> {
        tokio::time::timeout(wait, self.downs.recv()).await.ok().flatten()
    }

    /// Handle one Down event as a runtime does, then acknowledge it.
    pub async fn handle(&mut self, d: Down) -> Result<Handled, Error> {
        let seq = d.seq;
        let handled = match d.body {
            Some(down::Body::Order(signed)) => self.order(signed).await?,
            Some(down::Body::Settings(signed)) => self.settings(signed).await?,
            Some(down::Body::Pairing(offer)) => self.offer(offer).await?,
            Some(down::Body::Nudge(n)) => self.nudge(n).await?,
            _ => Handled::Other,
        };
        // handled durably (its result is out): only now acknowledged
        self.down_seq = self.down_seq.max(seq);
        self.send(Up { seq: 0, body: Some(up::Body::Ack(Ack { seq })) }).await?;
        Ok(handled)
    }

    /// Handle whatever comes down for `wait`, saying it on stderr, with
    /// heartbeats in between.
    pub async fn serve_for(&mut self, wait: Duration) -> Result<(), Error> {
        let until = tokio::time::Instant::now() + wait;
        let beat = Duration::from_secs(u64::from(self.welcome.heartbeat_secs.max(1)));
        while tokio::time::Instant::now() < until {
            let left = until.saturating_duration_since(tokio::time::Instant::now()).min(beat);
            match self.next_down(left).await {
                Some(d) => {
                    let h = self.handle(d).await?;
                    eprintln!("fake: {h:?}");
                }
                None => self.heartbeat().await?,
            }
        }
        Ok(())
    }

    async fn order(&mut self, signed: pb::Signed) -> Result<Handled, Error> {
        let loose = pb::Order::decode(&signed.payload[..]).unwrap_or_default();
        let order_id = loose.order_id.clone();
        let (outcome, reason) = self.judge(&signed, &loose).await?;
        self.side.log.push(format!("order {order_id}: {outcome:?} {reason}"));
        if !order_id.is_empty() {
            self.result(pb::result::Subject::OrderId(order_id.clone()), outcome, &reason, "").await?;
        }
        Ok(Handled::Order { order_id, outcome, reason })
    }

    /// The contract's checks, in its order; then what the order does.
    async fn judge(&mut self, signed: &pb::Signed, loose: &pb::Order) -> Result<(pb::Outcome, String), Error> {
        use pb::Outcome::{Done, Refused};
        let refuse = |r: &str| Ok((Refused, r.to_string()));
        let Some(device) = self.side.devices.iter().position(|d| d.device_id == signed.device_id) else { return refuse("unknown_device") };
        let key = self.side.devices[device].public_key.clone();
        if signing::verify(signing::ORDER, &key, &signed.payload, &signed.signature).is_err() {
            return refuse("bad_signature");
        }
        let Ok(order) = signing::decode_canonical::<pb::Order>(&signed.payload) else { return refuse("unsupported") };
        if order.runtime_id != self.side.runtime_id {
            return refuse("wrong_runtime");
        }
        if order.counter <= self.side.devices[device].counter {
            return refuse("replayed");
        }
        if order.expires_at <= now() {
            return refuse("expired");
        }
        // past those six the counter is spent, whatever happens next
        self.side.devices[device].counter = order.counter;
        let releasing = matches!(order.what, Some(pb::order::What::Hold(pb::Hold { on: false })));
        if self.side.held && !releasing {
            self.status().await?;
            return refuse("held");
        }
        let _ = loose;
        match order.what {
            Some(pb::order::What::Hold(h)) => {
                self.side.held = h.on;
                self.side.held_since = now();
                self.status().await?;
                Ok((Done, String::new()))
            }
            Some(pb::order::What::Command(c)) => {
                let out = self.command(&order.order_id, c).await?;
                self.status().await?;
                Ok(out)
            }
            None => refuse("unsupported"),
        }
    }

    async fn command(&mut self, order_id: &str, c: pb::Command) -> Result<(pb::Outcome, String), Error> {
        use pb::Outcome::{Done, Refused};
        let refuse = |r: &str| Ok((Refused, r.to_string()));
        let Some(session) = self.side.sessions.get(&c.session_id).cloned() else { return refuse("no_session") };
        let verb = pb::Verb::try_from(c.verb).unwrap_or(pb::Verb::Unspecified);
        let request = |side: &super::Side, kind: pb::AttentionKind| {
            side.open
                .get(&c.request_id)
                .filter(|(r, bytes)| r.kind == kind as i32 && r.session_id == c.session_id && signing::request_hash(bytes).as_slice() == c.request_hash.as_slice())
                .is_some()
        };
        match verb {
            pb::Verb::Send | pb::Verb::Answer if c.text.starts_with('!') || c.text.starts_with('/') => refuse("unsafe_text"),
            pb::Verb::Send => {
                if session.state == "asleep" {
                    return refuse("asleep");
                }
                if c.after_turn != session.turn_id && !c.queued {
                    return refuse("wrong_turn");
                }
                // "typed": played back as the turn it started
                let reply = format!("(fake runtime) done: {}", c.text);
                self.turn(&ScriptTurn { session: session.id.clone(), prompt: c.text, reply, source: "order".into(), order_id: order_id.into(), ..Default::default() })
                    .await?;
                Ok((Done, String::new()))
            }
            pb::Verb::Approve | pb::Verb::Deny => {
                if !request(&self.side, pb::AttentionKind::Approval) {
                    return refuse("stale_request");
                }
                let r = if verb == pb::Verb::Approve { "approved" } else { "denied" };
                self.clear(&c.session_id, &c.request_id, r).await?;
                Ok((Done, String::new()))
            }
            pb::Verb::Answer => {
                if !request(&self.side, pb::AttentionKind::Question) {
                    return refuse("wrong_turn");
                }
                self.clear(&c.session_id, &c.request_id, "answered").await?;
                let reply = format!("(fake runtime) going with: {}", c.text);
                self.turn(&ScriptTurn { session: session.id.clone(), prompt: c.text, reply, source: "answer".into(), order_id: order_id.into(), ..Default::default() })
                    .await?;
                Ok((Done, String::new()))
            }
            pb::Verb::Seen => {
                if session.state == "finished" {
                    let mut s = session.clone();
                    s.state = "idle".into();
                    s.since = Some(now());
                    self.state(&s).await?;
                }
                Ok((Done, String::new()))
            }
            pb::Verb::SetFabric => {
                let mut s = session.clone();
                s.fabric_id = c.text;
                self.state(&s).await?;
                Ok((Done, String::new()))
            }
            pb::Verb::Interrupt
            | pb::Verb::Resume
            | pb::Verb::Cancel
            | pb::Verb::Reorder
            | pb::Verb::ReleaseQueue
            | pb::Verb::Wake
            | pb::Verb::Compact
            | pb::Verb::Clear => Ok((Done, String::new())),
            pb::Verb::Spawn | pb::Verb::Unspecified => refuse("unsupported"),
        }
    }

    async fn settings(&mut self, signed: pb::Signed) -> Result<Handled, Error> {
        let Some(d) = self.side.devices.iter().find(|d| d.device_id == signed.device_id) else {
            return Ok(Handled::Settings { version: 0, accepted: false });
        };
        let settings = match signing::open::<pb::Settings>(signing::SETTINGS, &d.public_key, &signed.payload, &signed.signature) {
            Ok(s) => s,
            Err(_) => return Ok(Handled::Settings { version: 0, accepted: false }),
        };
        let version = settings.version;
        if version <= self.side.settings_version {
            self.result(pb::result::Subject::SettingsVersion(version), pb::Outcome::Refused, "stale_settings", "").await?;
            return Ok(Handled::Settings { version, accepted: false });
        }
        self.side.settings_version = version;
        self.side.settings = Some(settings);
        self.side.log.push(format!("settings {version} in force"));
        self.status().await?;
        Ok(Handled::Settings { version, accepted: true })
    }

    async fn offer(&mut self, offer: pb::PairingOffer) -> Result<Handled, Error> {
        let id = offer.pairing_id.clone();
        let reply = |outcome: pb::PairingOutcome, device: &str| Handled::Pairing { pairing_id: id.clone(), outcome, device_id: device.into() };
        let Some(secret) = self.side.pairings.remove(&offer.pairing_id) else {
            self.event(up::Body::Pairing(pb::Pairing { pairing_id: id.clone(), outcome: pb::PairingOutcome::Expired as i32, ..Default::default() })).await?;
            return Ok(reply(pb::PairingOutcome::Expired, ""));
        };
        let opened = (|| {
            let hk = hkdf::Hkdf::<Sha256>::new(Some(offer.pairing_id.as_bytes()), &secret);
            let mut k = [0u8; 32];
            hk.expand(b"taisce/pairing/v1", &mut k).ok()?;
            let (nonce, ct) = (offer.sealed.get(..12)?, offer.sealed.get(12..)?);
            let cipher = Aes256Gcm::new_from_slice(&k).ok()?;
            let plain = cipher.decrypt(&Nonce::try_from(nonce).ok()?, Payload { msg: ct, aad: self.side.runtime_id.as_bytes() }).ok()?;
            let seal = pb::PairingSeal::decode(&plain[..]).ok()?;
            let claim = pb::PairingClaim::decode(&seal.claim[..]).ok()?;
            signing::verify(signing::PAIRING, &claim.public_key, &seal.claim, &seal.signature).ok()?;
            (claim.runtime_id == self.side.runtime_id && claim.pairing_id == offer.pairing_id).then_some(claim)
        })();
        let Some(claim) = opened else {
            self.event(up::Body::Pairing(pb::Pairing { pairing_id: id.clone(), outcome: pb::PairingOutcome::Invalid as i32, ..Default::default() })).await?;
            return Ok(reply(pb::PairingOutcome::Invalid, ""));
        };
        // "the fingerprint matches", as the person at the Mac would say
        let device = self.side.pair(&claim.public_key, &claim.device_name);
        eprintln!("fake: paired {} ({})", claim.device_name, signing::fingerprint(&device));
        self.side.log.push(format!("paired {device}"));
        self.event(up::Body::Pairing(pb::Pairing {
            pairing_id: id.clone(),
            outcome: pb::PairingOutcome::Paired as i32,
            device_id: device.clone(),
            detail: String::new(),
        }))
        .await?;
        self.status().await?;
        Ok(reply(pb::PairingOutcome::Paired, &device))
    }

    async fn nudge(&mut self, n: pb::Nudge) -> Result<Handled, Error> {
        let inbox = self.call(&n.session_id, pb::call_request::Tool::Inbox(pb::InboxRequest { limit: 50 })).await?;
        let read = match inbox.reply {
            Some(pb::call_reply::Reply::Inbox(i)) => i.messages,
            _ => vec![],
        };
        let ids: Vec<String> = read.iter().map(|m| m.message_id.clone()).collect();
        if !ids.is_empty() {
            self.call(&n.session_id, pb::call_request::Tool::InboxAck(pb::InboxAckRequest { message_ids: ids })).await?;
        }
        for m in &read {
            self.side.log.push(format!("mail for {} from {}: {}", n.session_id, m.from_address, m.body));
        }
        self.result(pb::result::Subject::MessageId(n.message_id.clone()), pb::Outcome::Done, "", "").await?;
        Ok(Handled::Nudge { message_id: n.message_id, read })
    }
}
