//! Agents' fabric tool calls (`rpc Call`, ADR 0008 §3, §7): `who`, `send`,
//! `inbox`, `inbox_ack`, and what follows from them: routing that never
//! guesses, the pair limit and its pause, bounces, and the conversations a
//! person's devices read. Everything resolves inside the calling runtime's
//! owner's fleet and is stored in their `Scope::User`: no address, fabric
//! name or message id can reach another person's session.

use super::hub::{Hub, UserFleet};
use super::record::{Reject, cleared, hub_item, reject};
use super::view::{AlertKind, AttentionView, Change, ConversationView, MessageView, SessionAlert, attention_rank};
use crate::store_ext::with_store;
use serde_json::Value;
use taisce_runtime_proto::v1 as pb;
use taisce_store::fabric::{FabricConversation, FabricMessage, FabricRuntime, FabricSession, MAX_UNREAD, Party, SessionStatus};
use taisce_store::{Scope, StoreError};
use tonic::Status;
use uuid::Uuid;

/// A retried call within this long gets the first answer.
pub const CALL_MEMORY: i64 = 600;
/// A message body, in characters.
pub const MAX_BODY_CHARS: usize = 8000;
/// Candidates a refused `send` lists.
const MAX_CANDIDATES: usize = 20;

fn status_of(s: SessionStatus) -> i32 {
    (match s {
        SessionStatus::Working => pb::SessionStatus::Working,
        SessionStatus::NeedsYou => pb::SessionStatus::NeedsYou,
        SessionStatus::Finished => pb::SessionStatus::Finished,
        SessionStatus::Stalled => pb::SessionStatus::Stalled,
        SessionStatus::WaitingOnPeer => pb::SessionStatus::WaitingOnPeer,
        SessionStatus::Idle => pb::SessionStatus::Idle,
        SessionStatus::Asleep => pb::SessionStatus::Asleep,
        SessionStatus::Exited => pb::SessionStatus::Exited,
        SessionStatus::Unknown => pb::SessionStatus::Unspecified,
    }) as i32
}

fn kind_name(k: i32) -> Option<&'static str> {
    match pb::MessageKind::try_from(k).ok()? {
        pb::MessageKind::Note => Some("note"),
        pb::MessageKind::Ask => Some("ask"),
        pb::MessageKind::Reply => Some("reply"),
        pb::MessageKind::Bounce => Some("bounce"),
        pb::MessageKind::Unspecified => None,
    }
}

fn kind_of(name: &str) -> i32 {
    (match name {
        "note" => pb::MessageKind::Note,
        "ask" => pb::MessageKind::Ask,
        "reply" => pb::MessageKind::Reply,
        "bounce" => pb::MessageKind::Bounce,
        _ => pb::MessageKind::Unspecified,
    }) as i32
}

fn party(s: &FabricSession) -> Party {
    Party { runtime_id: s.runtime_id, session_id: s.id, address: s.address.clone() }
}

/// The fabric a session works in: its named fabric's id, or its project's.
fn fabric_key(s: &FabricSession) -> String {
    if s.fabric_id.is_empty() { format!("project:{}", s.project) } else { s.fabric_id.clone() }
}

/// A session as `who` and `send` describe it. A Text off project shows its
/// address, fabric and state alone.
fn peer(f: &UserFleet, s: &FabricSession, me: bool) -> pb::Peer {
    let offline = f.runtimes.get(&s.runtime_id).is_none_or(|r| r.conn.is_none());
    let mut p = pb::Peer {
        address: s.address.clone(),
        session_id: s.id.to_string(),
        fabric: s.fabric.clone(),
        project: s.project.clone(),
        branch: s.branch.clone(),
        state: status_of(s.state),
        note: s.note.clone(),
        offline,
        me,
        last_line: s.last_line.clone(),
    };
    if f.text_off(&s.project) {
        p.project.clear();
        p.branch.clear();
        p.note.clear();
        p.last_line.clear();
    }
    p
}

fn sessions(f: &UserFleet) -> impl Iterator<Item = &FabricSession> {
    f.runtimes.values().flat_map(|r| r.sessions.values().map(|s| &s.row))
}

/// The live sessions a sender may have meant by `to`: the same
/// `project/worktree` (any `:n`), then the same project.
fn candidates(f: &UserFleet, to: &str, caller: (Uuid, Uuid)) -> Vec<pb::Peer> {
    let base = to.split(':').next().unwrap_or(to);
    let project = to.split('/').next().unwrap_or(to);
    let live: Vec<&FabricSession> = sessions(f).filter(|s| s.state != SessionStatus::Exited && (s.runtime_id, s.id) != caller).collect();
    let mut out: Vec<&FabricSession> = live.iter().copied().filter(|s| s.address.split(':').next() == Some(base)).collect();
    let mut more: Vec<&FabricSession> = live
        .iter()
        .copied()
        .filter(|s| (s.project == project || s.address.split('/').next() == Some(project)) && !out.iter().any(|o| o.id == s.id && o.runtime_id == s.runtime_id))
        .collect();
    out.sort_by(|a, b| a.address.cmp(&b.address));
    more.sort_by(|a, b| a.address.cmp(&b.address));
    out.extend(more);
    out.into_iter().take(MAX_CANDIDATES).map(|s| peer(f, s, false)).collect()
}

/// The settings' fabrics, from their JSON.
fn fabrics(settings: &Option<Value>) -> Vec<Value> {
    settings.as_ref().and_then(|s| s["fabrics"].as_array().cloned()).unwrap_or_default()
}

fn pair_limit(settings: &Option<Value>) -> u32 {
    settings.as_ref().and_then(|s| s["limits"]["pair_messages"].as_u64()).map(|n| n as u32).filter(|n| *n > 0).unwrap_or(12)
}

/// One message to send, resolved.
struct Outgoing {
    to: Party,
    to_written: String,
    kind: &'static str,
    body: String,
    doc: String,
    reply_to: String,
    /// The receiver's state now, for the alert an ask to a sleeper raises.
    receiver_state: SessionStatus,
}

impl Hub {
    /// One fabric tool call from `runtime`, on its session's behalf.
    pub async fn call(&self, runtime: &FabricRuntime, req: pb::CallRequest, now: i64) -> Result<pb::CallReply, Status> {
        if req.call_id.is_empty() || req.call_id.len() > 64 {
            return Err(Status::invalid_argument("call_id: 1-64 bytes"));
        }
        let session = Uuid::parse_str(&req.session_id).map_err(|_| Status::invalid_argument("session_id: a UUID"))?;
        let key = (runtime.id, req.call_id.clone());
        {
            let mut calls = self.0.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            calls.retain(|_, (at, _)| now - *at < CALL_MEMORY);
            if let Some((_, reply)) = calls.get(&key) {
                return Ok(reply.clone());
            }
        }
        let user = runtime.user_id;
        self.sync(user).await.map_err(|_| Status::unavailable("the hub could not read its store; retry with the same call_id"))?;
        {
            let fleets = self.fleets();
            let rt = fleets.get(&user).and_then(|f| f.runtimes.get(&runtime.id)).ok_or_else(|| Status::unauthenticated("this runtime was removed"))?;
            if rt.conn.is_none() {
                return Err(Status::unavailable("open the Attach stream first; retry with the same call_id"));
            }
            if !rt.sessions.contains_key(&session) {
                return Err(Status::not_found("no such session on this runtime"));
            }
        }
        let reply = match req.tool {
            Some(pb::call_request::Tool::Who(w)) => pb::call_reply::Reply::Who(self.who_call(user, runtime.id, session, w.all).await?),
            Some(pb::call_request::Tool::Send(s)) => pb::call_reply::Reply::Send(self.send_call(user, runtime.id, session, s, now).await?),
            Some(pb::call_request::Tool::Inbox(i)) => pb::call_reply::Reply::Inbox(self.inbox_call(user, runtime.id, session, i.limit).await?),
            Some(pb::call_request::Tool::InboxAck(a)) => {
                pb::call_reply::Reply::InboxAck(self.inbox_ack_call(user, runtime.id, session, a.message_ids, now).await?)
            }
            None => return Err(Status::invalid_argument("tool: who, send, inbox or inbox_ack")),
        };
        let reply = pb::CallReply { reply: Some(reply) };
        self.0.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, (now, reply.clone()));
        Ok(reply)
    }

    async fn settings_json(&self, user: Uuid) -> Result<Option<Value>, Status> {
        let held = with_store(&self.0.store, Scope::User(user), |s| s.fabric_settings()).await.map_err(unavailable)?;
        Ok(held.and_then(|s| serde_json::from_str(&s.json).ok()))
    }

    async fn who_call(&self, user: Uuid, runtime: Uuid, session: Uuid, all: bool) -> Result<pb::WhoReply, Status> {
        let settings = self.settings_json(user).await?;
        let fleets = self.fleets();
        let f = fleets.get(&user).ok_or_else(|| Status::unavailable("retry"))?;
        let me = f.session(runtime, session).ok_or_else(|| Status::not_found("no such session on this runtime"))?;
        let key = fabric_key(me);
        let fabric = fabrics(&settings)
            .into_iter()
            .find(|x| x["id"].as_str() == Some(me.fabric_id.as_str()) && !me.fabric_id.is_empty())
            .map(|x| pb::Fabric {
                id: x["id"].as_str().unwrap_or_default().into(),
                name: x["name"].as_str().unwrap_or_default().into(),
                purpose: x["purpose"].as_str().unwrap_or_default().into(),
                doc_id: x["doc_id"].as_str().unwrap_or_default().into(),
                doc_title: x["doc_title"].as_str().unwrap_or_default().into(),
            })
            .unwrap_or_else(|| pb::Fabric { name: if me.fabric.is_empty() { me.project.clone() } else { me.fabric.clone() }, ..Default::default() });
        let mut peers: Vec<&FabricSession> =
            sessions(f).filter(|s| s.state != SessionStatus::Exited && (all || fabric_key(s) == key)).collect();
        peers.sort_by(|a, b| {
            let mine = |s: &FabricSession| !(s.runtime_id == runtime && s.id == session);
            mine(a).cmp(&mine(b)).then(attention_rank(a.state).cmp(&attention_rank(b.state))).then_with(|| a.address.cmp(&b.address))
        });
        Ok(pb::WhoReply { fabric: Some(fabric), peers: peers.into_iter().map(|s| peer(f, s, s.runtime_id == runtime && s.id == session)).collect() })
    }

    async fn send_call(&self, user: Uuid, runtime: Uuid, session: Uuid, req: pb::SendRequest, now: i64) -> Result<pb::SendReply, Status> {
        let kind = match kind_name(req.kind) {
            Some(k @ ("note" | "ask" | "reply")) => k,
            _ => return Err(Status::invalid_argument("kind: note, ask or reply")),
        };
        if req.body.trim().is_empty() || req.body.chars().count() > MAX_BODY_CHARS {
            return Err(Status::invalid_argument(format!("body: 1 to {MAX_BODY_CHARS} characters")));
        }
        if req.to.len() > 200 || req.doc.len() > 200 || req.reply_to.len() > 64 {
            return Err(Status::invalid_argument("to and doc: at most 200 bytes; reply_to: at most 64"));
        }
        let settings = self.settings_json(user).await?;
        let refused = |delivery: pb::Delivery, detail: String, candidates: Vec<pb::Peer>| pb::SendReply {
            sent: vec![pb::Sent { message_id: String::new(), recipient: None, delivery: delivery as i32, detail }],
            candidates,
        };
        let me = {
            let fleets = self.fleets();
            let f = fleets.get(&user).ok_or_else(|| Status::unavailable("retry"))?;
            party(f.session(runtime, session).ok_or_else(|| Status::not_found("no such session on this runtime"))?)
        };
        // 1. a reply (or a follow-up) goes to the message's sender, by id
        let mut outgoing = vec![];
        if !req.reply_to.is_empty() {
            let id = req.reply_to.clone();
            let original = with_store(&self.0.store, Scope::User(user), move |s| match s.fabric_message(&id) {
                Ok(m) => Ok(Some(m)),
                Err(StoreError::NotFound(_)) => Ok(None),
                Err(e) => Err(e),
            })
            .await
            .map_err(unavailable)?;
            let Some(original) = original.filter(|m| m.to.runtime_id == runtime && m.to.session_id == session) else {
                return Ok(refused(pb::Delivery::NoSuchMessage, "reply_to names no message sent to you".into(), vec![]));
            };
            if kind == "reply" && original.kind != "ask" {
                return Ok(refused(pb::Delivery::NoSuchMessage, "a reply answers an ask; that message is not one".into(), vec![]));
            }
            let fleets = self.fleets();
            let f = fleets.get(&user).ok_or_else(|| Status::unavailable("retry"))?;
            let Some(to) = f.session(original.from.runtime_id, original.from.session_id).filter(|s| s.state != SessionStatus::Exited) else {
                return Ok(refused(pb::Delivery::ReceiverExited, format!("{} has exited", original.from.address), vec![]));
            };
            outgoing.push(Outgoing {
                to: party(to),
                to_written: String::new(),
                kind,
                body: req.body.clone(),
                doc: req.doc.clone(),
                reply_to: original.id.clone(),
                receiver_state: to.state,
            });
        } else if kind == "reply" {
            return Ok(refused(pb::Delivery::NoSuchMessage, "a reply names the ask it answers (reply_to)".into(), vec![]));
        } else if req.to.contains('/') {
            // 2. an address: exactly, among the live sessions, never a guess
            let fleets = self.fleets();
            let f = fleets.get(&user).ok_or_else(|| Status::unavailable("retry"))?;
            let named: Vec<&FabricSession> = sessions(f).filter(|s| s.address == req.to).collect();
            let live: Vec<&FabricSession> = named.iter().copied().filter(|s| s.state != SessionStatus::Exited).collect();
            match live.as_slice() {
                [one] => outgoing.push(Outgoing {
                    to: party(one),
                    to_written: req.to.clone(),
                    kind,
                    body: req.body.clone(),
                    doc: req.doc.clone(),
                    reply_to: String::new(),
                    receiver_state: one.state,
                }),
                [] if !named.is_empty() => {
                    return Ok(refused(
                        pb::Delivery::ReceiverExited,
                        format!("the session at {} has exited: see candidates", req.to),
                        candidates(f, &req.to, (runtime, session)),
                    ));
                }
                [] => {
                    return Ok(refused(
                        pb::Delivery::NoSuchAddress,
                        format!("no live session has the address {}: see candidates, or call who", req.to),
                        candidates(f, &req.to, (runtime, session)),
                    ));
                }
                _ => {
                    let list: Vec<pb::Peer> = live.iter().map(|s| peer(f, s, false)).collect();
                    return Ok(refused(pb::Delivery::Ambiguous, format!("{} sessions have the address {}: nothing was sent", list.len(), req.to), list));
                }
            }
        } else {
            // 3. a fabric's name: a note to each member
            let fleets = self.fleets();
            let f = fleets.get(&user).ok_or_else(|| Status::unavailable("retry"))?;
            let named = fabrics(&settings).into_iter().find(|x| x["name"].as_str() == Some(req.to.as_str()));
            let key = match &named {
                Some(x) => x["id"].as_str().unwrap_or_default().to_string(),
                None => format!("project:{}", req.to),
            };
            let members: Vec<&FabricSession> =
                sessions(f).filter(|s| s.state != SessionStatus::Exited && fabric_key(s) == key && !(s.runtime_id == runtime && s.id == session)).collect();
            let known = named.is_some() || sessions(f).any(|s| s.project == req.to && s.fabric_id.is_empty());
            if !known {
                return Ok(refused(
                    pb::Delivery::NoSuchAddress,
                    format!("{:?} is no address (they have a /) and no fabric: see candidates, or call who", req.to),
                    candidates(f, &req.to, (runtime, session)),
                ));
            }
            if kind != "note" {
                return Ok(refused(pb::Delivery::NotANote, "only a note goes to a whole fabric: ask one session by its address".into(), vec![]));
            }
            for m in members {
                outgoing.push(Outgoing {
                    to: party(m),
                    to_written: req.to.clone(),
                    kind,
                    body: req.body.clone(),
                    doc: req.doc.clone(),
                    reply_to: String::new(),
                    receiver_state: m.state,
                });
            }
        }
        let limit = pair_limit(&settings);
        let mut sent = vec![];
        for o in outgoing {
            sent.push(self.deliver(user, &me, o, limit, now).await?);
        }
        Ok(pb::SendReply { sent, candidates: vec![] })
    }

    /// Store one message (or hold it, at the pair limit) and say where it landed.
    async fn deliver(&self, user: Uuid, from: &Party, o: Outgoing, limit: u32, now: i64) -> Result<pb::Sent, Status> {
        let recipient = {
            let fleets = self.fleets();
            let f = fleets.get(&user).ok_or_else(|| Status::unavailable("retry"))?;
            f.session(o.to.runtime_id, o.to.session_id).map(|s| peer(f, s, false))
        };
        let (from_c, to_c, id) = (from.clone(), o.to.clone(), Uuid::now_v7().to_string());
        let (kind, reply_to) = (o.kind, o.reply_to.clone());
        type Stored = Option<(FabricMessage, FabricConversation, bool, Option<FabricMessage>)>;
        let stored: Stored = with_store(&self.0.store, Scope::User(user), move |s| -> Result<Stored, StoreError> {
            if s.fabric_unread_count(to_c.runtime_id, to_c.session_id)? >= MAX_UNREAD {
                return Ok(None);
            }
            let c = s.fabric_conversation_for(&from_c, &to_c, now)?;
            let pausing = !c.paused && c.count + 1 > limit;
            let held = c.paused || pausing;
            let m = s.fabric_message_insert(&FabricMessage {
                id: id.clone(),
                user_id: user,
                conversation_id: Some(c.id.clone()),
                from: from_c.clone(),
                to: to_c.clone(),
                to_written: o.to_written.clone(),
                kind: kind.into(),
                body: Some(o.body.clone()),
                doc: o.doc.clone(),
                reply_to: reply_to.clone(),
                sent_at: now,
                read_at: None,
                state: if held { "held".into() } else { "unread".into() },
                bounce_reason: None,
                answered: false,
                down_instance: None,
                down_seq: None,
                received_at: None,
                nudge_outcome: None,
                nudge_reason: None,
                updated_at: now,
            })?;
            let c = s.fabric_conversation_set(&c.id, if held { c.count } else { c.count + 1 }, c.total + 1, held, now, now)?;
            let answered = if kind == "reply" && !reply_to.is_empty() {
                s.fabric_message_answered(&reply_to, now)?;
                Some(s.fabric_message(&reply_to)?)
            } else {
                None
            };
            Ok(Some((m, c, pausing, answered)))
        })
        .await
        .map_err(unavailable)?;
        let Some((m, c, pausing, answered)) = stored else {
            return Ok(pb::Sent {
                message_id: String::new(),
                recipient,
                delivery: pb::Delivery::InboxFull as i32,
                detail: format!("{} has {MAX_UNREAD} unread messages: nothing was sent", o.to.address),
            });
        };
        let mut changes = vec![Change::Message(MessageView::from(&m)), Change::Conversation(ConversationView::from(&c))];
        changes.extend(answered.iter().map(|a| Change::Message(MessageView::from(a))));
        let mut alerts = vec![];
        if pausing {
            let item = hub_item(user, from, "pair_paused", &format!("pair:{}:{now}", c.id), now, |a| {
                a.conversation_id = Some(c.id.clone());
                a.from_address = Some(o.to.address.clone());
                a.text = format!("{} and {} have exchanged {} messages without you. Continue?", from.address, o.to.address, c.count);
            });
            let rt = from.runtime_id;
            if let Ok(Some((row, _))) = with_store(&self.0.store, Scope::User(user), move |s| s.fabric_attention_raise(rt, &item)).await {
                changes.push(Change::Attention(AttentionView::from(&row)));
                let mut a = SessionAlert::new(user, from.runtime_id, from.session_id, &from.address, AlertKind::PairPaused);
                a.request_id = Some(row.id.clone());
                a.preview = Some(format!("with {}", o.to.address));
                alerts.push(a);
            }
        }
        if m.state == "unread" && kind == "ask" && o.receiver_state == SessionStatus::Asleep {
            let item = hub_item(user, &o.to, "ask_waiting", &format!("ask:{}", m.id), now, |a| {
                a.message_id = Some(m.id.clone());
                a.from_address = Some(from.address.clone());
                a.text = format!("{} asked {}, which is asleep: only you can resume it.", from.address, o.to.address);
            });
            let rt = o.to.runtime_id;
            if let Ok(Some((row, _))) = with_store(&self.0.store, Scope::User(user), move |s| s.fabric_attention_raise(rt, &item)).await {
                changes.push(Change::Attention(AttentionView::from(&row)));
                let mut a = SessionAlert::new(user, o.to.runtime_id, o.to.session_id, &o.to.address, AlertKind::AskWaiting);
                a.request_id = Some(row.id.clone());
                a.preview = Some(format!("from {}", from.address));
                alerts.push(a);
            }
        }
        let alerts = {
            let mut fleets = self.fleets();
            match fleets.get_mut(&user) {
                Some(fleet) => {
                    if let Some(rt) = fleet.runtimes.get(&o.to.runtime_id) {
                        rt.ring();
                    }
                    Self::publish(fleet, changes);
                    fleet.finish_alerts(alerts, now)
                }
                None => vec![],
            }
        };
        self.alert_all(alerts);
        let (delivery, detail) = if m.state == "held" {
            (pb::Delivery::Paused, format!("paused: {} messages between you and {} without your person; they have been asked", c.total, o.to.address))
        } else {
            let state = recipient.as_ref().map(|p| p.state).unwrap_or_default();
            let offline = recipient.as_ref().is_some_and(|p| p.offline);
            let how = if offline {
                "its Mac is offline: it gets it when it is back"
            } else if state == pb::SessionStatus::Working as i32 {
                "it is working: it is told when its turn ends"
            } else if state == pb::SessionStatus::Asleep as i32 {
                "it is asleep: your person has been told"
            } else {
                "it is told now"
            };
            (pb::Delivery::Queued, format!("sent to {} ({how})", o.to.address))
        };
        Ok(pb::Sent { message_id: m.id, recipient, delivery: delivery as i32, detail })
    }

    async fn inbox_call(&self, user: Uuid, runtime: Uuid, session: Uuid, limit: u32) -> Result<pb::InboxReply, Status> {
        let limit = match limit {
            0 => 20,
            n => (n as usize).min(50),
        };
        let (mail, more) = with_store(&self.0.store, Scope::User(user), move |s| s.fabric_inbox(runtime, session, limit)).await.map_err(unavailable)?;
        Ok(pb::InboxReply {
            messages: mail
                .into_iter()
                .map(|m| pb::Mail {
                    message_id: m.id,
                    from_address: m.from.address,
                    from_session_id: m.from.session_id.to_string(),
                    kind: kind_of(&m.kind),
                    body: m.body.unwrap_or_default(),
                    doc: m.doc,
                    reply_to: m.reply_to,
                    sent_at: m.sent_at,
                    to_session_id: m.to.session_id.to_string(),
                    to: m.to_written,
                })
                .collect(),
            more: more as u32,
        })
    }

    async fn inbox_ack_call(&self, user: Uuid, runtime: Uuid, session: Uuid, ids: Vec<String>, now: i64) -> Result<pb::InboxAckReply, Status> {
        if ids.len() > 50 || ids.iter().any(|i| i.len() > 64) {
            return Err(Status::invalid_argument("message_ids: at most 50"));
        }
        let read = with_store(&self.0.store, Scope::User(user), move |s| -> Result<_, StoreError> {
            let read = s.fabric_inbox_ack(runtime, session, &ids, now)?;
            // an offer about mail now read is over
            let mut over = vec![];
            for m in &read {
                for id in [format!("wake:{}", m.id), format!("ask:{}", m.id)] {
                    over.extend(s.fabric_attention_clear(m.to.runtime_id, &id, "withdrawn", now, false)?);
                }
            }
            Ok((read, over))
        })
        .await
        .map_err(unavailable)?;
        let (read, over) = read;
        let mut changes: Vec<Change> = read.iter().map(|m| Change::Message(MessageView::from(m))).collect();
        changes.extend(over.iter().map(cleared));
        self.publish_to(user, changes);
        Ok(pb::InboxAckReply { acknowledged: read.len() as u32 })
    }

    /// A message cannot be delivered: it bounces to its sender, whose inbox
    /// gets a `bounce` naming it. A bounce never bounces.
    pub(super) async fn bounce(&self, user: Uuid, id: &str, why: &str, now: i64) {
        let (id, why_s) = (id.to_string(), why.to_string());
        let done = with_store(&self.0.store, Scope::User(user), move |s| -> Result<_, StoreError> {
            let Some(m) = s.fabric_message_set(&id, "bounced", Some(&why_s), &["unread", "held"], now)? else { return Ok(None) };
            if m.kind == "bounce" {
                return Ok(Some((m, None)));
            }
            // the sender must still be in the fleet for its runtime to take it
            let back = s.fabric_message_insert(&FabricMessage {
                id: Uuid::now_v7().to_string(),
                user_id: user,
                conversation_id: None,
                from: m.to.clone(),
                to: m.from.clone(),
                to_written: String::new(),
                kind: "bounce".into(),
                body: Some(format!("Your {} to {} was not delivered: {why_s}.", m.kind, m.to.address)),
                doc: String::new(),
                reply_to: m.id.clone(),
                sent_at: now,
                read_at: None,
                state: "unread".into(),
                bounce_reason: None,
                answered: false,
                down_instance: None,
                down_seq: None,
                received_at: None,
                nudge_outcome: None,
                nudge_reason: None,
                updated_at: now,
            });
            Ok(Some((m, back.ok())))
        })
        .await;
        match done {
            Ok(Some((m, back))) => {
                let mut changes = vec![Change::Message(MessageView::from(&m))];
                let mut fleets = self.fleets();
                if let Some(fleet) = fleets.get_mut(&user) {
                    if let Some(b) = &back {
                        changes.push(Change::Message(MessageView::from(b)));
                        if let Some(rt) = fleet.runtimes.get(&b.to.runtime_id) {
                            rt.ring();
                        }
                    }
                    Self::publish(fleet, changes);
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("fabric: bouncing message {why} failed: {e}"),
        }
    }

    // ---- what the person's devices do with conversations ----

    pub async fn conversations(&self, user: Uuid) -> Result<Vec<ConversationView>, StoreError> {
        let got = with_store(&self.0.store, Scope::User(user), |s| s.fabric_conversations(200)).await?;
        Ok(got.iter().map(ConversationView::from).collect())
    }

    pub async fn conversation(&self, user: Uuid, id: &str, limit: usize) -> Result<(ConversationView, Vec<MessageView>), StoreError> {
        let id = id.to_string();
        let (c, msgs) = with_store(&self.0.store, Scope::User(user), move |s| Ok::<_, StoreError>((s.fabric_conversation(&id)?, s.fabric_conversation_messages(&id, limit)?))).await?;
        Ok((ConversationView::from(&c), msgs.iter().map(MessageView::from).collect()))
    }

    /// The person says a paused pair may go on (`go`) or stop: held
    /// messages are delivered, or bounce; either way the count restarts.
    pub async fn resolve_pair(&self, user: Uuid, id: &str, go: bool, now: i64) -> Result<ConversationView, Reject> {
        let cid = id.to_string();
        let got = with_store(&self.0.store, Scope::User(user), move |s| -> Result<_, StoreError> {
            let c = s.fabric_conversation(&cid)?;
            let held = s.fabric_conversation_held(&cid)?;
            let c = s.fabric_conversation_set(&cid, 0, c.total, false, c.last_at, now)?;
            let mut released = vec![];
            if go {
                for m in &held {
                    released.extend(s.fabric_message_set(&m.id, "unread", None, &["held"], now)?);
                }
            }
            let items: Vec<_> = s.fabric_attention_open()?.into_iter().filter(|a| a.conversation_id.as_deref() == Some(cid.as_str())).collect();
            let mut over = vec![];
            for a in items {
                over.extend(s.fabric_attention_clear(a.runtime_id, &a.id, if go { "continued" } else { "stopped" }, now, false)?);
            }
            Ok((c, held, released, over))
        })
        .await
        .map_err(|e| match e {
            StoreError::NotFound(_) => reject(404, "not_found", "no such conversation"),
            e => reject(500, "store", e.to_string()),
        })?;
        let (c, held, released, over) = got;
        if !go {
            for m in &held {
                self.bounce(user, &m.id, "your person stopped this exchange", now).await;
            }
        }
        let mut changes = vec![Change::Conversation(ConversationView::from(&c))];
        changes.extend(released.iter().map(|m| Change::Message(MessageView::from(m))));
        changes.extend(over.iter().map(cleared));
        {
            let mut fleets = self.fleets();
            if let Some(fleet) = fleets.get_mut(&user) {
                for m in &released {
                    if let Some(rt) = fleet.runtimes.get(&m.to.runtime_id) {
                        rt.ring();
                    }
                }
                Self::publish(fleet, changes);
            }
        }
        Ok(ConversationView::from(&c))
    }

    /// Maintenance: mail unread for a day bounces; orders that never
    /// reached their runtime in time expire (their people are told);
    /// what is past retention goes.
    pub async fn expire_work(&self, now: i64) {
        let stale = with_store(&self.0.store, Scope::System, move |s| s.fabric_messages_stale(now - taisce_store::fabric::MESSAGE_TEXT_TTL)).await;
        match stale {
            Ok(stale) => {
                for m in stale {
                    self.bounce(m.user_id, &m.id, "nobody read it for a day", now).await;
                }
            }
            Err(e) => tracing::warn!("fabric: finding stale mail failed: {e}"),
        }
        let expired = match with_store(&self.0.store, Scope::System, move |s| s.fabric_work_expire(now)).await {
            Ok(x) => x,
            Err(e) => return tracing::warn!("fabric: expiring work failed: {e}"),
        };
        let (orders, pairings) = expired;
        let mut alerts = vec![];
        {
            let mut fleets = self.fleets();
            for o in &orders {
                if let Some(fleet) = fleets.get_mut(&o.user_id) {
                    Self::publish(fleet, vec![Change::Order(super::view::OrderView::from(o))]);
                    let session = o.session_id.unwrap_or_default();
                    let address = fleet.session(o.runtime_id, session).map(|s| s.address.clone()).unwrap_or_default();
                    let mut a = SessionAlert::new(o.user_id, o.runtime_id, session, &address, AlertKind::OrderRefused);
                    a.order_id = Some(o.id.clone());
                    a.preview = Some("expired before its Mac was back".into());
                    alerts.extend(fleet.finish_alerts(vec![a], now));
                }
            }
            for p in &pairings {
                if let Some(fleet) = fleets.get_mut(&p.user_id) {
                    Self::publish(fleet, vec![Change::Pairing(super::view::PairingView::from(p))]);
                }
            }
        }
        self.alert_all(alerts);
    }
}

fn unavailable(e: StoreError) -> Status {
    tracing::warn!("fabric: store error on a call: {e}");
    Status::unavailable("the hub could not reach its store; retry with the same call_id")
}
