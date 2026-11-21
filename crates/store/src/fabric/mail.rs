//! Messages between a person's agents (fabric.sql, ADR 0008 §7): one row
//! per receiver, and one conversation row per pair of sessions that the
//! pair limit counts. Every row names its person and is read only in their
//! `Scope::User`; there is no way to address a row of another person's
//! fleet, because every lookup filters on the scope's user.

use super::{Fleet, uuid_at};
use crate::sqlite::SqliteStore;
use crate::{Result, StoreError};
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

/// A message waits this long unread before it bounces.
pub const MESSAGE_TEXT_TTL: i64 = 86400;
/// Unread messages one session may hold.
pub const MAX_UNREAD: usize = 100;

/// One end of a message or a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct Party {
    pub runtime_id: Uuid,
    pub session_id: Uuid,
    pub address: String,
}

/// One message to one receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricMessage {
    pub id: String,
    pub user_id: Uuid,
    pub conversation_id: Option<String>,
    pub from: Party,
    pub to: Party,
    /// What the sender wrote in `to`.
    pub to_written: String,
    /// `note`, `ask`, `reply` or `bounce`.
    pub kind: String,
    /// None once read, bounced or expired.
    pub body: Option<String>,
    pub doc: String,
    pub reply_to: String,
    pub sent_at: i64,
    pub read_at: Option<i64>,
    /// `unread`, `read`, `held` or `bounced`.
    pub state: String,
    pub bounce_reason: Option<String>,
    pub answered: bool,
    pub down_instance: Option<String>,
    pub down_seq: Option<u64>,
    pub received_at: Option<i64>,
    pub nudge_outcome: Option<String>,
    pub nudge_reason: Option<String>,
    pub updated_at: i64,
}

const MESSAGE_COLS: &str = "id, user_id, conversation_id, from_runtime, from_session, from_address, to_runtime, to_session, \
     to_address, to_written, kind, body, doc, reply_to, sent_at, read_at, state, bounce_reason, answered, down_instance, \
     down_seq, received_at, nudge_outcome, nudge_reason, updated_at";

fn message_row(r: &Row) -> rusqlite::Result<FabricMessage> {
    let seq: Option<i64> = r.get(20)?;
    Ok(FabricMessage {
        id: r.get(0)?,
        user_id: uuid_at(r, 1)?,
        conversation_id: r.get(2)?,
        from: Party { runtime_id: uuid_at(r, 3)?, session_id: uuid_at(r, 4)?, address: r.get(5)? },
        to: Party { runtime_id: uuid_at(r, 6)?, session_id: uuid_at(r, 7)?, address: r.get(8)? },
        to_written: r.get(9)?,
        kind: r.get(10)?,
        body: r.get(11)?,
        doc: r.get(12)?,
        reply_to: r.get(13)?,
        sent_at: r.get(14)?,
        read_at: r.get(15)?,
        state: r.get(16)?,
        bounce_reason: r.get(17)?,
        answered: r.get(18)?,
        down_instance: r.get(19)?,
        down_seq: seq.map(|s| s.max(0) as u64),
        received_at: r.get(21)?,
        nudge_outcome: r.get(22)?,
        nudge_reason: r.get(23)?,
        updated_at: r.get(24)?,
    })
}

/// Two sessions that have messaged each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricConversation {
    pub id: String,
    pub user_id: Uuid,
    pub a: Party,
    pub b: Party,
    /// Messages since the person last ordered either session.
    pub count: u32,
    pub total: u32,
    pub last_at: i64,
    pub paused: bool,
    pub updated_at: i64,
}

const CONVERSATION_COLS: &str =
    "id, user_id, a_runtime, a_session, a_address, b_runtime, b_session, b_address, count, total, last_at, paused, updated_at";

fn conversation_row(r: &Row) -> rusqlite::Result<FabricConversation> {
    Ok(FabricConversation {
        id: r.get(0)?,
        user_id: uuid_at(r, 1)?,
        a: Party { runtime_id: uuid_at(r, 2)?, session_id: uuid_at(r, 3)?, address: r.get(4)? },
        b: Party { runtime_id: uuid_at(r, 5)?, session_id: uuid_at(r, 6)?, address: r.get(7)? },
        count: r.get::<_, i64>(8)?.max(0) as u32,
        total: r.get::<_, i64>(9)?.max(0) as u32,
        last_at: r.get(10)?,
        paused: r.get(11)?,
        updated_at: r.get(12)?,
    })
}

impl SqliteStore {
    fn mail_user(&self) -> Result<Option<String>> {
        Ok(match self.fleet()? {
            Fleet::Of(u) => Some(u.to_string()),
            Fleet::All | Fleet::Nobody => None,
        })
    }

    /// The scope's conversation between two sessions, made if new. Both
    /// sessions' runtimes must be the scope's.
    pub fn fabric_conversation_for(&mut self, x: &Party, y: &Party, now: i64) -> Result<FabricConversation> {
        let owner = self.fabric_runtime(x.runtime_id)?.user_id;
        if self.fabric_runtime(y.runtime_id)?.user_id != owner {
            return Err(StoreError::NotFound(format!("runtime {}", y.runtime_id)));
        }
        let (a, b) = if (x.runtime_id, x.session_id) <= (y.runtime_id, y.session_id) { (x, y) } else { (y, x) };
        self.conn.execute(
            "INSERT INTO fabric_conversations (id, user_id, a_runtime, a_session, a_address, b_runtime, b_session, b_address,
                 last_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
             ON CONFLICT (user_id, a_runtime, a_session, b_runtime, b_session) DO UPDATE SET
                 a_address = excluded.a_address, b_address = excluded.b_address",
            params![
                Uuid::now_v7().to_string(),
                owner.to_string(),
                a.runtime_id.to_string(),
                a.session_id.to_string(),
                a.address,
                b.runtime_id.to_string(),
                b.session_id.to_string(),
                b.address,
                now
            ],
        )?;
        Ok(self.conn.query_row(
            &format!(
                "SELECT {CONVERSATION_COLS} FROM fabric_conversations
                 WHERE user_id = ?1 AND a_runtime = ?2 AND a_session = ?3 AND b_runtime = ?4 AND b_session = ?5"
            ),
            params![owner.to_string(), a.runtime_id.to_string(), a.session_id.to_string(), b.runtime_id.to_string(), b.session_id.to_string()],
            conversation_row,
        )?)
    }

    /// One of the scope's conversations.
    pub fn fabric_conversation(&self, id: &str) -> Result<FabricConversation> {
        let Some(user) = self.mail_user()? else { return Err(StoreError::NotFound(format!("conversation {id}"))) };
        self.conn
            .query_row(&format!("SELECT {CONVERSATION_COLS} FROM fabric_conversations WHERE id = ?1 AND user_id = ?2"), params![id, user], conversation_row)
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("conversation {id}")))
    }

    /// The scope's conversations, most recent first.
    pub fn fabric_conversations(&self, limit: usize) -> Result<Vec<FabricConversation>> {
        let Some(user) = self.mail_user()? else { return Ok(vec![]) };
        let mut st = self.conn.prepare(&format!(
            "SELECT {CONVERSATION_COLS} FROM fabric_conversations WHERE user_id = ?1 ORDER BY last_at DESC, id LIMIT ?2"
        ))?;
        let rows = st.query_map(params![user, limit as i64], conversation_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Set one of the scope's conversations' count, total, pause and time.
    pub fn fabric_conversation_set(&mut self, id: &str, count: u32, total: u32, paused: bool, last_at: i64, now: i64) -> Result<FabricConversation> {
        let c = self.fabric_conversation(id)?;
        self.conn.execute(
            "UPDATE fabric_conversations SET count = ?2, total = ?3, paused = ?4, last_at = ?5, updated_at = ?6 WHERE id = ?1",
            params![c.id, count, total, paused, last_at, now],
        )?;
        self.fabric_conversation(id)
    }

    /// The person ordered a session: every conversation it is in starts
    /// counting again (a paused one stays paused until continued or
    /// stopped). Says which changed.
    pub fn fabric_conversations_reset(&mut self, runtime: Uuid, session: Uuid, now: i64) -> Result<Vec<FabricConversation>> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let mut st = self.conn.prepare(&format!(
            "UPDATE fabric_conversations SET count = 0, updated_at = ?4
             WHERE user_id = ?1 AND count > 0 AND paused = 0
                 AND ((a_runtime = ?2 AND a_session = ?3) OR (b_runtime = ?2 AND b_session = ?3))
             RETURNING {CONVERSATION_COLS}"
        ))?;
        let rows = st.query_map(params![owner.to_string(), runtime.to_string(), session.to_string(), now], conversation_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Store a message. Both ends' runtimes must be the scope's.
    pub fn fabric_message_insert(&mut self, m: &FabricMessage) -> Result<FabricMessage> {
        let owner = self.fabric_runtime(m.to.runtime_id)?.user_id;
        if self.fabric_runtime(m.from.runtime_id)?.user_id != owner {
            return Err(StoreError::NotFound(format!("runtime {}", m.from.runtime_id)));
        }
        self.conn.execute(
            &format!(
                "INSERT INTO fabric_messages ({MESSAGE_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)"
            ),
            params![
                m.id,
                owner.to_string(),
                m.conversation_id,
                m.from.runtime_id.to_string(),
                m.from.session_id.to_string(),
                m.from.address,
                m.to.runtime_id.to_string(),
                m.to.session_id.to_string(),
                m.to.address,
                m.to_written,
                m.kind,
                m.body,
                m.doc,
                m.reply_to,
                m.sent_at,
                m.read_at,
                m.state,
                m.bounce_reason,
                m.answered,
                m.down_instance,
                m.down_seq.map(|s| i64::try_from(s).unwrap_or(i64::MAX)),
                m.received_at,
                m.nudge_outcome,
                m.nudge_reason,
                m.updated_at
            ],
        )?;
        self.fabric_message(&m.id)
    }

    /// One of the scope's messages.
    pub fn fabric_message(&self, id: &str) -> Result<FabricMessage> {
        let Some(user) = self.mail_user()? else { return Err(StoreError::NotFound(format!("message {id}"))) };
        self.conn
            .query_row(&format!("SELECT {MESSAGE_COLS} FROM fabric_messages WHERE id = ?1 AND user_id = ?2"), params![id, user], message_row)
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("message {id}")))
    }

    /// One conversation's messages, newest first.
    pub fn fabric_conversation_messages(&self, conversation: &str, limit: usize) -> Result<Vec<FabricMessage>> {
        let c = self.fabric_conversation(conversation)?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM fabric_messages WHERE conversation_id = ?1 AND user_id = ?2 ORDER BY sent_at DESC, id DESC LIMIT ?3"
        ))?;
        let rows = st.query_map(params![c.id, c.user_id.to_string(), limit as i64], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The unread mail of one session of one of the scope's runtimes,
    /// oldest first, and how many more there are.
    pub fn fabric_inbox(&self, runtime: Uuid, session: Uuid, limit: usize) -> Result<(Vec<FabricMessage>, usize)> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let all: i64 = self.conn.query_row(
            "SELECT count(*) FROM fabric_messages WHERE user_id = ?1 AND to_runtime = ?2 AND to_session = ?3 AND state = 'unread'",
            params![owner.to_string(), runtime.to_string(), session.to_string()],
            |r| r.get(0),
        )?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM fabric_messages WHERE user_id = ?1 AND to_runtime = ?2 AND to_session = ?3
                 AND state = 'unread' ORDER BY sent_at, id LIMIT ?4"
        ))?;
        let rows = st.query_map(params![owner.to_string(), runtime.to_string(), session.to_string(), limit as i64], message_row)?;
        let got: Vec<FabricMessage> = rows.collect::<rusqlite::Result<_>>()?;
        let more = (all.max(0) as usize).saturating_sub(got.len());
        Ok((got, more))
    }

    /// These messages reached their session: mark them read and delete
    /// their text. Only the session's own unread mail is touched; returns
    /// the rows marked.
    pub fn fabric_inbox_ack(&mut self, runtime: Uuid, session: Uuid, ids: &[String], now: i64) -> Result<Vec<FabricMessage>> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let mut out = vec![];
        for id in ids {
            let got = self
                .conn
                .query_row(
                    &format!(
                        "UPDATE fabric_messages SET state = 'read', read_at = ?5, body = NULL, updated_at = ?5
                         WHERE id = ?1 AND user_id = ?2 AND to_runtime = ?3 AND to_session = ?4 AND state = 'unread'
                         RETURNING {MESSAGE_COLS}"
                    ),
                    params![id, owner.to_string(), runtime.to_string(), session.to_string(), now],
                    message_row,
                )
                .optional()?;
            out.extend(got);
        }
        Ok(out)
    }

    /// Unread messages one session of one of the scope's runtimes holds.
    pub fn fabric_unread_count(&self, runtime: Uuid, session: Uuid) -> Result<usize> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM fabric_messages WHERE user_id = ?1 AND to_runtime = ?2 AND to_session = ?3 AND state = 'unread'",
            params![owner.to_string(), runtime.to_string(), session.to_string()],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as usize)
    }

    /// Move one of the scope's messages: `state`, with a bounce reason when
    /// it bounced. Leaving `unread` deletes the text. With `only_from`,
    /// only from one of those states.
    pub fn fabric_message_set(&mut self, id: &str, state: &str, bounce_reason: Option<&str>, only_from: &[&str], now: i64) -> Result<Option<FabricMessage>> {
        let m = self.fabric_message(id)?;
        if !only_from.is_empty() && !only_from.contains(&m.state.as_str()) {
            return Ok(None);
        }
        let keep_body = state == "unread" || state == "held";
        self.conn.execute(
            "UPDATE fabric_messages SET state = ?2, bounce_reason = COALESCE(?3, bounce_reason), updated_at = ?4,
                 body = CASE WHEN ?5 THEN body ELSE NULL END,
                 received_at = CASE WHEN ?2 = 'unread' AND state = 'held' THEN NULL ELSE received_at END
             WHERE id = ?1",
            params![m.id, state, bounce_reason, now, keep_body],
        )?;
        self.fabric_message(id).map(Some)
    }

    /// An ask was answered.
    pub fn fabric_message_answered(&mut self, id: &str, now: i64) -> Result<()> {
        let m = self.fabric_message(id)?;
        self.conn.execute("UPDATE fabric_messages SET answered = 1, updated_at = ?2 WHERE id = ?1", params![m.id, now])?;
        Ok(())
    }

    /// What the runtime said of a message's nudge.
    pub fn fabric_message_nudged(&mut self, runtime: Uuid, id: &str, outcome: &str, reason: Option<&str>, now: i64) -> Result<Option<FabricMessage>> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        Ok(self
            .conn
            .query_row(
                &format!(
                    "UPDATE fabric_messages SET nudge_outcome = ?4, nudge_reason = ?5, updated_at = ?6
                     WHERE id = ?1 AND user_id = ?2 AND to_runtime = ?3 RETURNING {MESSAGE_COLS}"
                ),
                params![id, owner.to_string(), runtime.to_string(), outcome, reason, now],
                message_row,
            )
            .optional()?)
    }

    /// The held messages of one of the scope's conversations, oldest first.
    pub fn fabric_conversation_held(&self, conversation: &str) -> Result<Vec<FabricMessage>> {
        let c = self.fabric_conversation(conversation)?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM fabric_messages WHERE conversation_id = ?1 AND user_id = ?2 AND state = 'held' ORDER BY sent_at, id"
        ))?;
        let rows = st.query_map(params![c.id, c.user_id.to_string()], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Unread messages to one of the scope's runtimes whose nudge it has
    /// not acknowledged, oldest first.
    pub fn fabric_nudges_undelivered(&self, runtime: Uuid) -> Result<Vec<FabricMessage>> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM fabric_messages WHERE user_id = ?1 AND to_runtime = ?2 AND state = 'unread'
                 AND received_at IS NULL ORDER BY sent_at, id"
        ))?;
        let rows = st.query_map(params![owner.to_string(), runtime.to_string()], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every unread or held message of one of the scope's runtimes' session
    /// (it left the fleet: they bounce).
    pub fn fabric_messages_to(&self, runtime: Uuid, session: Uuid) -> Result<Vec<FabricMessage>> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM fabric_messages WHERE user_id = ?1 AND to_runtime = ?2 AND to_session = ?3
                 AND state IN ('unread', 'held') ORDER BY sent_at, id"
        ))?;
        let rows = st.query_map(params![owner.to_string(), runtime.to_string(), session.to_string()], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Maintenance (System only): every unread or held message sent before
    /// `cutoff`, to bounce. The caller bounces each in its person's scope.
    pub fn fabric_messages_stale(&self, cutoff: i64) -> Result<Vec<FabricMessage>> {
        if !matches!(self.fleet()?, Fleet::All) {
            return Err(StoreError::InvalidOp("finding stale messages is maintenance (System scope)".into()));
        }
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM fabric_messages WHERE state IN ('unread', 'held') AND sent_at < ?1 ORDER BY sent_at, id"
        ))?;
        let rows = st.query_map([cutoff], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
