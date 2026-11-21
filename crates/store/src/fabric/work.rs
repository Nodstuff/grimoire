//! ADR 0008's record (fabric.sql): turns, attention items, signed orders,
//! the person's signed settings, their project switches and pairing offers,
//! and the bookkeeping of what went down a runtime's stream. The Scope rule
//! is the parent module's: `User(u)` sees `u`'s rows only, anyone else's
//! are `NotFound`; `System` sees everything (expiry); `Local` and `Within`
//! see nothing; `Public` is denied. A write naming a runtime first resolves
//! it in the caller's scope, and every row is keyed to that runtime and its
//! owner, whatever the caller passed.

use super::{Fleet, uuid_at};
use crate::sqlite::SqliteStore;
use crate::{Result, StoreError};
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

/// Turns, orders, attention items and message envelopes are history: this
/// long after their last change.
pub const WORK_RETENTION: i64 = 7 * 86400;
/// A pairing offer's row is kept this long (it is live for 10 minutes).
pub const PAIRING_ROW_RETENTION: i64 = 86400;
/// Open attention items one runtime may hold; the oldest is cleared beyond.
pub const MAX_OPEN_ATTENTION: usize = 64;

fn not_found(what: &str, id: impl std::fmt::Display) -> StoreError {
    StoreError::NotFound(format!("{what} {id}"))
}

fn opt_uuid(r: &Row, col: usize) -> rusqlite::Result<Option<Uuid>> {
    let text: Option<String> = r.get(col)?;
    text.map(|t| Uuid::parse_str(&t).map_err(|e| rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e))))
        .transpose()
}

fn u64_at(r: &Row, col: usize) -> rusqlite::Result<u64> {
    Ok(r.get::<_, i64>(col)?.max(0) as u64)
}

fn i64_of(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

// ---- turns ----

/// One finished turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricTurn {
    pub runtime_id: Uuid,
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub turn_id: String,
    pub address: String,
    pub project: String,
    pub prompt: String,
    pub reply: String,
    /// The project had Text off: prompt and reply were not stored.
    pub text_off: bool,
    pub tool_count: u32,
    /// JSON: `{files_changed, lines_added, lines_removed, commits, pull_request_url, pull_request_number}`.
    pub outcome: String,
    pub started: i64,
    pub ended: i64,
    pub truncated: bool,
    pub context_used_pct: Option<u8>,
    pub end_kind: String,
    pub source: String,
    pub order_id: Option<String>,
    pub created_at: i64,
}

const TURN_COLS: &str = "runtime_id, session_id, user_id, turn_id, address, project, prompt, reply, text_off, tool_count, \
     outcome, started, ended, truncated, context_used_pct, end_kind, source, order_id, created_at";

fn turn_row(r: &Row) -> rusqlite::Result<FabricTurn> {
    let pct: Option<i64> = r.get(14)?;
    Ok(FabricTurn {
        runtime_id: uuid_at(r, 0)?,
        session_id: uuid_at(r, 1)?,
        user_id: uuid_at(r, 2)?,
        turn_id: r.get(3)?,
        address: r.get(4)?,
        project: r.get(5)?,
        prompt: r.get(6)?,
        reply: r.get(7)?,
        text_off: r.get(8)?,
        tool_count: u64_at(r, 9)? as u32,
        outcome: r.get(10)?,
        started: r.get(11)?,
        ended: r.get(12)?,
        truncated: r.get(13)?,
        context_used_pct: pct.map(|p| p.clamp(0, 100) as u8),
        end_kind: r.get(15)?,
        source: r.get(16)?,
        order_id: r.get(17)?,
        created_at: r.get(18)?,
    })
}

// ---- attention ----

/// An attention item, raised by a runtime or by the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricAttention {
    pub runtime_id: Uuid,
    pub id: String,
    pub user_id: Uuid,
    pub session_id: Uuid,
    pub address: String,
    pub project: String,
    /// `runtime` or `hub`.
    pub source: String,
    pub kind: String,
    pub text: String,
    /// JSON `[{id, label}]`.
    pub choices: String,
    pub tool: String,
    pub guardrail_id: String,
    pub guardrail_label: String,
    pub turn_id: String,
    pub raised_at: i64,
    pub expires_at: Option<i64>,
    pub request: Option<Vec<u8>>,
    pub signature: Option<Vec<u8>>,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
    pub from_address: Option<String>,
    pub cleared_at: Option<i64>,
    pub resolution: Option<String>,
    pub updated_at: i64,
}

const ATTENTION_COLS: &str = "runtime_id, id, user_id, session_id, address, project, source, kind, text, choices, tool, \
     guardrail_id, guardrail_label, turn_id, raised_at, expires_at, request, signature, conversation_id, message_id, \
     from_address, cleared_at, resolution, updated_at";

fn attention_row(r: &Row) -> rusqlite::Result<FabricAttention> {
    Ok(FabricAttention {
        runtime_id: uuid_at(r, 0)?,
        id: r.get(1)?,
        user_id: uuid_at(r, 2)?,
        session_id: uuid_at(r, 3)?,
        address: r.get(4)?,
        project: r.get(5)?,
        source: r.get(6)?,
        kind: r.get(7)?,
        text: r.get(8)?,
        choices: r.get(9)?,
        tool: r.get(10)?,
        guardrail_id: r.get(11)?,
        guardrail_label: r.get(12)?,
        turn_id: r.get(13)?,
        raised_at: r.get(14)?,
        expires_at: r.get(15)?,
        request: r.get(16)?,
        signature: r.get(17)?,
        conversation_id: r.get(18)?,
        message_id: r.get(19)?,
        from_address: r.get(20)?,
        cleared_at: r.get(21)?,
        resolution: r.get(22)?,
        updated_at: r.get(23)?,
    })
}

// ---- orders ----

/// A signed order and where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricOrder {
    pub runtime_id: Uuid,
    pub id: String,
    pub user_id: Uuid,
    pub session_id: Option<Uuid>,
    pub device_id: String,
    pub counter: u64,
    /// `command` or `hold`.
    pub kind: String,
    pub verb: Option<String>,
    pub hold: Option<bool>,
    pub text: Option<String>,
    pub after_turn: String,
    pub queued: bool,
    pub request_id: String,
    pub choice_id: String,
    /// JSON array of order ids.
    pub order_ids: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub payload: Option<Vec<u8>>,
    pub signature: Option<Vec<u8>>,
    pub state: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub down_instance: Option<String>,
    pub down_seq: Option<u64>,
    pub received_at: Option<i64>,
    pub project: String,
}

const ORDER_COLS: &str = "runtime_id, id, user_id, session_id, device_id, counter, kind, verb, hold, text, after_turn, \
     queued, request_id, choice_id, order_ids, issued_at, expires_at, created_at, updated_at, payload, signature, state, \
     reason, detail, down_instance, down_seq, received_at, project";

fn order_row(r: &Row) -> rusqlite::Result<FabricOrder> {
    let seq: Option<i64> = r.get(25)?;
    Ok(FabricOrder {
        runtime_id: uuid_at(r, 0)?,
        id: r.get(1)?,
        user_id: uuid_at(r, 2)?,
        session_id: opt_uuid(r, 3)?,
        device_id: r.get(4)?,
        counter: u64_at(r, 5)?,
        kind: r.get(6)?,
        verb: r.get(7)?,
        hold: r.get(8)?,
        text: r.get(9)?,
        after_turn: r.get(10)?,
        queued: r.get(11)?,
        request_id: r.get(12)?,
        choice_id: r.get(13)?,
        order_ids: r.get(14)?,
        issued_at: r.get(15)?,
        expires_at: r.get(16)?,
        created_at: r.get(17)?,
        updated_at: r.get(18)?,
        payload: r.get(19)?,
        signature: r.get(20)?,
        state: r.get(21)?,
        reason: r.get(22)?,
        detail: r.get(23)?,
        down_instance: r.get(24)?,
        down_seq: seq.map(|s| s.max(0) as u64),
        received_at: r.get(26)?,
        project: r.get(27)?,
    })
}

/// States an order is still on its way in: the hub holds it, or it is on a
/// stream the runtime has not acknowledged.
pub const ORDER_UNDELIVERED: [&str; 2] = ["pending", "sent"];

// ---- settings, projects, pairings ----

/// The person's newest signed settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricSettings {
    pub user_id: Uuid,
    pub version: u64,
    pub issued_at: i64,
    pub payload: Vec<u8>,
    pub device_id: String,
    pub signature: Vec<u8>,
    /// The decoded settings, as JSON.
    pub json: String,
    pub updated_at: i64,
}

/// A person's switches for one project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FabricProject {
    pub name: String,
    pub text_off: bool,
    pub private_notifications: bool,
    pub updated_at: i64,
}

/// A pairing offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricPairing {
    pub runtime_id: Uuid,
    pub id: String,
    pub user_id: Uuid,
    pub sealed: Option<Vec<u8>>,
    pub state: String,
    pub device_id: Option<String>,
    pub detail: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub updated_at: i64,
    pub down_instance: Option<String>,
    pub down_seq: Option<u64>,
    pub received_at: Option<i64>,
}

const PAIRING_COLS: &str =
    "runtime_id, id, user_id, sealed, state, device_id, detail, created_at, expires_at, updated_at, down_instance, down_seq, received_at";

fn pairing_row(r: &Row) -> rusqlite::Result<FabricPairing> {
    let seq: Option<i64> = r.get(11)?;
    Ok(FabricPairing {
        runtime_id: uuid_at(r, 0)?,
        id: r.get(1)?,
        user_id: uuid_at(r, 2)?,
        sealed: r.get(3)?,
        state: r.get(4)?,
        device_id: r.get(5)?,
        detail: r.get(6)?,
        created_at: r.get(7)?,
        expires_at: r.get(8)?,
        updated_at: r.get(9)?,
        down_instance: r.get(10)?,
        down_seq: seq.map(|s| s.max(0) as u64),
        received_at: r.get(12)?,
    })
}

/// What a row on a runtime's stream is: the tables a Down event comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownKind {
    Order,
    Pairing,
    Nudge,
}

impl DownKind {
    fn table(self) -> &'static str {
        match self {
            DownKind::Order => "fabric_orders",
            DownKind::Pairing => "fabric_pairings",
            DownKind::Nudge => "fabric_messages",
        }
    }

    /// The column naming the runtime the row goes down to.
    fn runtime_col(self) -> &'static str {
        match self {
            DownKind::Order | DownKind::Pairing => "runtime_id",
            DownKind::Nudge => "to_runtime",
        }
    }
}

impl SqliteStore {
    /// The scope's own user id, or NotFound for a scope that sees no fleet
    /// (and for System, which has no person to write as).
    fn person(&self, what: &str) -> Result<Uuid> {
        match self.fleet()? {
            Fleet::Of(u) => Ok(u),
            Fleet::All | Fleet::Nobody => Err(StoreError::NotFound(what.to_string())),
        }
    }

    // ---- turns ----

    /// Store a turn of one of the scope's runtimes, keyed to it and its
    /// owner. A turn already held (a replay) is left as it is: false.
    pub fn fabric_turn_put(&mut self, runtime: Uuid, t: &FabricTurn) -> Result<bool> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let n = self.conn.execute(
            &format!(
                "INSERT INTO fabric_turns ({TURN_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                 ON CONFLICT (runtime_id, session_id, turn_id) DO NOTHING"
            ),
            params![
                runtime.to_string(),
                t.session_id.to_string(),
                owner.to_string(),
                t.turn_id,
                t.address,
                t.project,
                t.prompt,
                t.reply,
                t.text_off,
                t.tool_count,
                t.outcome,
                t.started,
                t.ended,
                t.truncated,
                t.context_used_pct,
                t.end_kind,
                t.source,
                t.order_id,
                t.created_at
            ],
        )?;
        Ok(n == 1)
    }

    /// One session's turns, newest first, ended before `before` when given.
    /// A runtime that is not the scope's is NotFound.
    pub fn fabric_turns(&self, runtime: Uuid, session: Uuid, before: Option<i64>, limit: usize) -> Result<Vec<FabricTurn>> {
        self.fabric_runtime(runtime)?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {TURN_COLS} FROM fabric_turns WHERE runtime_id = ?1 AND session_id = ?2 AND (?3 IS NULL OR ended < ?3)
             ORDER BY ended DESC, created_at DESC LIMIT ?4"
        ))?;
        let rows = st.query_map(params![runtime.to_string(), session.to_string(), before, limit as i64], turn_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every turn of the scope's fleet ended after `since`, oldest first.
    pub fn fabric_turns_since(&self, since: i64, limit: usize) -> Result<Vec<FabricTurn>> {
        let Some(user) = self.fleet()?.filter() else { return Ok(vec![]) };
        let mut st = self.conn.prepare(&format!(
            "SELECT {TURN_COLS} FROM fabric_turns WHERE (?1 IS NULL OR user_id = ?1) AND ended > ?2
             ORDER BY ended, created_at LIMIT ?3"
        ))?;
        let rows = st.query_map(params![user, since, limit as i64], turn_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- attention ----

    /// Raise an item on one of the scope's runtimes. One already held under
    /// that id (a replay) is left as it is: None. Beyond
    /// [`MAX_OPEN_ATTENTION`] open, the oldest are cleared (`withdrawn`) and
    /// returned with the new row.
    pub fn fabric_attention_raise(&mut self, runtime: Uuid, a: &FabricAttention) -> Result<Option<(FabricAttention, Vec<FabricAttention>)>> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            &format!(
                "INSERT INTO fabric_attention ({ATTENTION_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
                 ON CONFLICT (runtime_id, id) DO NOTHING"
            ),
            params![
                runtime.to_string(),
                a.id,
                owner.to_string(),
                a.session_id.to_string(),
                a.address,
                a.project,
                a.source,
                a.kind,
                a.text,
                a.choices,
                a.tool,
                a.guardrail_id,
                a.guardrail_label,
                a.turn_id,
                a.raised_at,
                a.expires_at,
                a.request,
                a.signature,
                a.conversation_id,
                a.message_id,
                a.from_address,
                a.cleared_at,
                a.resolution,
                a.updated_at
            ],
        )?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = {
            let mut st = tx.prepare(&format!(
                "UPDATE fabric_attention SET cleared_at = ?2, resolution = 'withdrawn', updated_at = ?2
                 WHERE runtime_id = ?1 AND cleared_at IS NULL AND id IN (
                     SELECT id FROM fabric_attention WHERE runtime_id = ?1 AND cleared_at IS NULL
                     ORDER BY raised_at DESC, rowid DESC LIMIT -1 OFFSET ?3)
                 RETURNING {ATTENTION_COLS}"
            ))?;
            let rows = st.query_map(params![runtime.to_string(), a.updated_at, MAX_OPEN_ATTENTION as i64], attention_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let row = tx.query_row(
            &format!("SELECT {ATTENTION_COLS} FROM fabric_attention WHERE runtime_id = ?1 AND id = ?2"),
            params![runtime.to_string(), a.id],
            attention_row,
        )?;
        tx.commit()?;
        Ok(Some((row, trimmed)))
    }

    /// Clear an open item of one of the scope's runtimes. None when it is
    /// not open (cleared already, or never raised).
    pub fn fabric_attention_clear(&mut self, runtime: Uuid, id: &str, resolution: &str, at: i64) -> Result<Option<FabricAttention>> {
        self.fabric_runtime(runtime)?;
        Ok(self
            .conn
            .query_row(
                &format!(
                    "UPDATE fabric_attention SET cleared_at = ?3, resolution = ?4, updated_at = ?3
                     WHERE runtime_id = ?1 AND id = ?2 AND cleared_at IS NULL RETURNING {ATTENTION_COLS}"
                ),
                params![runtime.to_string(), id, at, resolution],
                attention_row,
            )
            .optional()?)
    }

    /// Clear every open item of one of the scope's runtimes, raised by the
    /// runtime, that `keep` does not name (a snapshot's `open_requests`).
    pub fn fabric_attention_clear_others(&mut self, runtime: Uuid, keep: &[String], at: i64) -> Result<Vec<FabricAttention>> {
        self.fabric_runtime(runtime)?;
        let open: Vec<String> = {
            let mut st =
                self.conn.prepare("SELECT id FROM fabric_attention WHERE runtime_id = ?1 AND source = 'runtime' AND cleared_at IS NULL")?;
            st.query_map([runtime.to_string()], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let mut cleared = vec![];
        for id in open.into_iter().filter(|id| !keep.contains(id)) {
            if let Some(a) = self.fabric_attention_clear(runtime, &id, "withdrawn", at)? {
                cleared.push(a);
            }
        }
        Ok(cleared)
    }

    /// The scope's open items, newest first.
    pub fn fabric_attention_open(&self) -> Result<Vec<FabricAttention>> {
        let Some(user) = self.fleet()?.filter() else { return Ok(vec![]) };
        let mut st = self.conn.prepare(&format!(
            "SELECT {ATTENTION_COLS} FROM fabric_attention WHERE (?1 IS NULL OR user_id = ?1) AND cleared_at IS NULL
             ORDER BY raised_at DESC, rowid DESC"
        ))?;
        let rows = st.query_map(params![user], attention_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One of the scope's items by id, open or not; anyone else's is NotFound.
    pub fn fabric_attention_get(&self, id: &str) -> Result<FabricAttention> {
        let Some(user) = self.fleet()?.filter() else { return Err(not_found("attention item", id)) };
        self.conn
            .query_row(
                &format!(
                    "SELECT {ATTENTION_COLS} FROM fabric_attention WHERE id = ?1 AND (?2 IS NULL OR user_id = ?2)
                     ORDER BY cleared_at IS NULL DESC, raised_at DESC LIMIT 1"
                ),
                params![id, user],
                attention_row,
            )
            .optional()?
            .ok_or_else(|| not_found("attention item", id))
    }

    // ---- orders ----

    /// Store a new order for one of the scope's runtimes. Its id must be
    /// new there (`InvalidOp` otherwise: the caller looks first).
    pub fn fabric_order_insert(&mut self, o: &FabricOrder) -> Result<FabricOrder> {
        let owner = self.fabric_runtime(o.runtime_id)?.user_id;
        let n = self.conn.execute(
            &format!(
                "INSERT INTO fabric_orders ({ORDER_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)
                 ON CONFLICT (runtime_id, id) DO NOTHING"
            ),
            params![
                o.runtime_id.to_string(),
                o.id,
                owner.to_string(),
                o.session_id.map(|s| s.to_string()),
                o.device_id,
                i64_of(o.counter),
                o.kind,
                o.verb,
                o.hold,
                o.text,
                o.after_turn,
                o.queued,
                o.request_id,
                o.choice_id,
                o.order_ids,
                o.issued_at,
                o.expires_at,
                o.created_at,
                o.updated_at,
                o.payload,
                o.signature,
                o.state,
                o.reason,
                o.detail,
                o.down_instance,
                o.down_seq.map(i64_of),
                o.received_at,
                o.project
            ],
        )?;
        if n == 0 {
            return Err(StoreError::InvalidOp(format!("order {} exists", o.id)));
        }
        self.fabric_order(o.runtime_id, &o.id)
    }

    /// One order of one of the scope's runtimes.
    pub fn fabric_order(&self, runtime: Uuid, id: &str) -> Result<FabricOrder> {
        let Some(user) = self.fleet()?.filter() else { return Err(not_found("order", id)) };
        self.conn
            .query_row(
                &format!("SELECT {ORDER_COLS} FROM fabric_orders WHERE runtime_id = ?1 AND id = ?2 AND (?3 IS NULL OR user_id = ?3)"),
                params![runtime.to_string(), id, user],
                order_row,
            )
            .optional()?
            .ok_or_else(|| not_found("order", id))
    }

    /// One of the scope's orders by id alone (the newest, should two
    /// runtimes hold the same id).
    pub fn fabric_order_find(&self, id: &str) -> Result<FabricOrder> {
        let Some(user) = self.fleet()?.filter() else { return Err(not_found("order", id)) };
        self.conn
            .query_row(
                &format!(
                    "SELECT {ORDER_COLS} FROM fabric_orders WHERE id = ?1 AND (?2 IS NULL OR user_id = ?2)
                     ORDER BY created_at DESC LIMIT 1"
                ),
                params![id, user],
                order_row,
            )
            .optional()?
            .ok_or_else(|| not_found("order", id))
    }

    /// The scope's orders, newest first, narrowed to a runtime and a
    /// session when given, created before `before` when given.
    pub fn fabric_orders(&self, runtime: Option<Uuid>, session: Option<Uuid>, before: Option<i64>, limit: usize) -> Result<Vec<FabricOrder>> {
        let Some(user) = self.fleet()?.filter() else { return Ok(vec![]) };
        let mut st = self.conn.prepare(&format!(
            "SELECT {ORDER_COLS} FROM fabric_orders WHERE (?1 IS NULL OR user_id = ?1) AND (?2 IS NULL OR runtime_id = ?2)
                 AND (?3 IS NULL OR session_id = ?3) AND (?4 IS NULL OR created_at < ?4)
             ORDER BY created_at DESC, rowid DESC LIMIT ?5"
        ))?;
        let rows = st.query_map(
            params![user, runtime.map(|r| r.to_string()), session.map(|s| s.to_string()), before, limit as i64],
            order_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The highest counter held from `device` for one of the scope's runtimes.
    pub fn fabric_order_max_counter(&self, runtime: Uuid, device: &str) -> Result<u64> {
        self.fabric_runtime(runtime)?;
        let max: Option<i64> = self.conn.query_row(
            "SELECT max(counter) FROM fabric_orders WHERE runtime_id = ?1 AND device_id = ?2",
            params![runtime.to_string(), device],
            |r| r.get(0),
        )?;
        Ok(max.unwrap_or(0).max(0) as u64)
    }

    /// Move an order of one of the scope's runtimes to `state`. With
    /// `only_from`, only from one of those states (None when it was in
    /// another). `drop_text` forgets the text and the signed bytes (Text
    /// off, once the runtime has them or they are dead).
    pub fn fabric_order_set(
        &mut self,
        runtime: Uuid,
        id: &str,
        state: &str,
        reason: Option<&str>,
        detail: Option<&str>,
        only_from: Option<&[&str]>,
        drop_text: bool,
        now: i64,
    ) -> Result<Option<FabricOrder>> {
        let o = self.fabric_order(runtime, id)?;
        if only_from.is_some_and(|from| !from.contains(&o.state.as_str())) {
            return Ok(None);
        }
        self.conn.execute(
            "UPDATE fabric_orders SET state = ?3, reason = ?4, detail = ?5, updated_at = ?6,
                 text = CASE WHEN ?7 THEN NULL ELSE text END,
                 payload = CASE WHEN ?7 THEN NULL ELSE payload END,
                 signature = CASE WHEN ?7 THEN NULL ELSE signature END
             WHERE runtime_id = ?1 AND id = ?2",
            params![runtime.to_string(), id, state, reason, detail, now, drop_text],
        )?;
        self.fabric_order(runtime, id).map(Some)
    }

    /// One of the scope's runtimes' orders still on their way, oldest first.
    pub fn fabric_orders_undelivered(&self, runtime: Uuid) -> Result<Vec<FabricOrder>> {
        self.fabric_runtime(runtime)?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {ORDER_COLS} FROM fabric_orders WHERE runtime_id = ?1 AND state IN ('pending', 'sent') AND received_at IS NULL
             ORDER BY created_at, rowid"
        ))?;
        let rows = st.query_map([runtime.to_string()], order_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- what went down a stream ----

    /// A row went down `runtime`'s stream as event `seq` of `instance`.
    pub fn fabric_down_sent(&mut self, runtime: Uuid, kind: DownKind, id: &str, instance: &str, seq: u64, now: i64) -> Result<()> {
        self.fabric_runtime(runtime)?;
        let state = match kind {
            DownKind::Order => ", state = CASE WHEN state = 'pending' THEN 'sent' ELSE state END",
            DownKind::Pairing => ", state = CASE WHEN state = 'pending' THEN 'sent' ELSE state END",
            DownKind::Nudge => "",
        };
        self.conn.execute(
            &format!(
                "UPDATE {} SET down_instance = ?3, down_seq = ?4, updated_at = ?5{state} WHERE {} = ?1 AND id = ?2",
                kind.table(),
                kind.runtime_col()
            ),
            params![runtime.to_string(), id, instance, i64_of(seq), now],
        )?;
        Ok(())
    }

    /// The runtime has every Down event of `instance` up to `upto`: mark
    /// what went down as received. Says which rows, in sending order.
    pub fn fabric_down_received(&mut self, runtime: Uuid, instance: &str, upto: u64, now: i64) -> Result<Vec<(DownKind, String)>> {
        self.fabric_runtime(runtime)?;
        let mut got = vec![];
        for kind in [DownKind::Order, DownKind::Pairing, DownKind::Nudge] {
            let state = match kind {
                DownKind::Order => ", state = CASE WHEN state = 'sent' THEN 'received' ELSE state END",
                DownKind::Pairing | DownKind::Nudge => "",
            };
            let mut st = self.conn.prepare(&format!(
                "UPDATE {} SET received_at = ?4, updated_at = ?4{state}
                 WHERE {} = ?1 AND down_instance = ?2 AND down_seq <= ?3 AND received_at IS NULL RETURNING id, down_seq",
                kind.table(),
                kind.runtime_col()
            ))?;
            let rows = st.query_map(params![runtime.to_string(), instance, i64_of(upto), now], |r| {
                Ok((r.get::<_, i64>(1)?, kind, r.get::<_, String>(0)?))
            })?;
            got.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        got.sort_by_key(|(seq, _, _)| *seq);
        Ok(got.into_iter().map(|(_, k, id)| (k, id)).collect())
    }

    /// The highest Down number of `instance` any row of one of the scope's
    /// runtimes carries (0 for none).
    pub fn fabric_down_max_seq(&self, runtime: Uuid, instance: &str) -> Result<u64> {
        self.fabric_runtime(runtime)?;
        let mut max = 0u64;
        for kind in [DownKind::Order, DownKind::Pairing, DownKind::Nudge] {
            let m: Option<i64> = self.conn.query_row(
                &format!("SELECT max(down_seq) FROM {} WHERE {} = ?1 AND down_instance = ?2", kind.table(), kind.runtime_col()),
                params![runtime.to_string(), instance],
                |r| r.get(0),
            )?;
            max = max.max(m.unwrap_or(0).max(0) as u64);
        }
        Ok(max)
    }

    // ---- settings ----

    /// The scope's newest signed settings, if any.
    pub fn fabric_settings(&self) -> Result<Option<FabricSettings>> {
        let Ok(user) = self.person("settings") else { return Ok(None) };
        Ok(self
            .conn
            .query_row(
                "SELECT user_id, version, issued_at, payload, device_id, signature, json, updated_at FROM fabric_settings WHERE user_id = ?1",
                [user.to_string()],
                |r| {
                    Ok(FabricSettings {
                        user_id: uuid_at(r, 0)?,
                        version: u64_at(r, 1)?,
                        issued_at: r.get(2)?,
                        payload: r.get(3)?,
                        device_id: r.get(4)?,
                        signature: r.get(5)?,
                        json: r.get(6)?,
                        updated_at: r.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    /// Replace the scope's settings with `s`, whose version must be exactly
    /// one above the version held (0 when none): `InvalidOp("stale …")`
    /// otherwise, checked in the same transaction as the write.
    pub fn fabric_settings_put(&mut self, s: &FabricSettings) -> Result<()> {
        let user = self.person("settings")?;
        let tx = self.conn.transaction()?;
        let held: Option<i64> =
            tx.query_row("SELECT version FROM fabric_settings WHERE user_id = ?1", [user.to_string()], |r| r.get(0)).optional()?;
        let held = held.unwrap_or(0).max(0) as u64;
        if s.version != held + 1 {
            return Err(StoreError::InvalidOp(format!("stale settings: version {held} is held, so the next is {}", held + 1)));
        }
        tx.execute(
            "INSERT INTO fabric_settings (user_id, version, issued_at, payload, device_id, signature, json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (user_id) DO UPDATE SET version = excluded.version, issued_at = excluded.issued_at,
                 payload = excluded.payload, device_id = excluded.device_id, signature = excluded.signature,
                 json = excluded.json, updated_at = excluded.updated_at",
            params![user.to_string(), i64_of(s.version), s.issued_at, s.payload, s.device_id, s.signature, s.json, s.updated_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ---- projects ----

    /// The scope's project switches.
    pub fn fabric_projects(&self) -> Result<Vec<FabricProject>> {
        let Ok(user) = self.person("projects") else { return Ok(vec![]) };
        let mut st = self.conn.prepare(
            "SELECT name, text_off, private_notifications, updated_at FROM fabric_projects WHERE user_id = ?1 ORDER BY name",
        )?;
        let rows = st.query_map([user.to_string()], |r| {
            Ok(FabricProject { name: r.get(0)?, text_off: r.get(1)?, private_notifications: r.get(2)?, updated_at: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Set the scope's switches for `name` (None leaves one as it is).
    /// Turning Text off deletes the project's stored text at once: turns'
    /// prompts and replies, attention text and signed requests of cleared
    /// items, sessions' last lines and notes, and the text of orders the
    /// runtime already has.
    pub fn fabric_project_put(&mut self, name: &str, text_off: Option<bool>, private: Option<bool>, now: i64) -> Result<FabricProject> {
        let user = self.person("projects")?;
        let u = user.to_string();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO fabric_projects (user_id, name, text_off, private_notifications, updated_at)
             VALUES (?1, ?2, COALESCE(?3, 0), COALESCE(?4, 0), ?5)
             ON CONFLICT (user_id, name) DO UPDATE SET text_off = COALESCE(?3, text_off),
                 private_notifications = COALESCE(?4, private_notifications), updated_at = ?5",
            params![u, name, text_off, private, now],
        )?;
        if text_off == Some(true) {
            tx.execute("UPDATE fabric_turns SET prompt = '', reply = '', text_off = 1 WHERE user_id = ?1 AND project = ?2", params![u, name])?;
            tx.execute(
                "UPDATE fabric_attention SET text = '', request = NULL, signature = NULL
                 WHERE user_id = ?1 AND project = ?2 AND cleared_at IS NOT NULL",
                params![u, name],
            )?;
            tx.execute("UPDATE fabric_sessions SET last_line = '', note = '' WHERE user_id = ?1 AND project = ?2", params![u, name])?;
            tx.execute(
                "UPDATE fabric_orders SET text = NULL, payload = NULL, signature = NULL
                 WHERE user_id = ?1 AND project = ?2 AND state NOT IN ('pending', 'sent')",
                params![u, name],
            )?;
        }
        let p = tx.query_row(
            "SELECT name, text_off, private_notifications, updated_at FROM fabric_projects WHERE user_id = ?1 AND name = ?2",
            params![u, name],
            |r| Ok(FabricProject { name: r.get(0)?, text_off: r.get(1)?, private_notifications: r.get(2)?, updated_at: r.get(3)? }),
        )?;
        tx.commit()?;
        Ok(p)
    }

    // ---- pairings ----

    /// A new pairing offer for one of the scope's runtimes. An id already
    /// used there is `InvalidOp`.
    pub fn fabric_pairing_create(&mut self, runtime: Uuid, id: &str, sealed: &[u8], now: i64, expires_at: i64) -> Result<FabricPairing> {
        let owner = self.fabric_runtime(runtime)?.user_id;
        let n = self.conn.execute(
            "INSERT INTO fabric_pairings (runtime_id, id, user_id, sealed, state, created_at, expires_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?5) ON CONFLICT (runtime_id, id) DO NOTHING",
            params![runtime.to_string(), id, owner.to_string(), sealed, now, expires_at],
        )?;
        if n == 0 {
            return Err(StoreError::InvalidOp(format!("pairing {id} exists")));
        }
        self.fabric_pairing(runtime, id)
    }

    /// One pairing offer of one of the scope's runtimes.
    pub fn fabric_pairing(&self, runtime: Uuid, id: &str) -> Result<FabricPairing> {
        self.fabric_runtime(runtime)?;
        self.conn
            .query_row(
                &format!("SELECT {PAIRING_COLS} FROM fabric_pairings WHERE runtime_id = ?1 AND id = ?2"),
                params![runtime.to_string(), id],
                pairing_row,
            )
            .optional()?
            .ok_or_else(|| not_found("pairing", id))
    }

    /// One of the scope's pairing offers by id alone.
    pub fn fabric_pairing_find(&self, id: &str) -> Result<FabricPairing> {
        let Some(user) = self.fleet()?.filter() else { return Err(not_found("pairing", id)) };
        self.conn
            .query_row(
                &format!(
                    "SELECT {PAIRING_COLS} FROM fabric_pairings WHERE id = ?1 AND (?2 IS NULL OR user_id = ?2) ORDER BY created_at DESC LIMIT 1"
                ),
                params![id, user],
                pairing_row,
            )
            .optional()?
            .ok_or_else(|| not_found("pairing", id))
    }

    /// Record what a runtime said of a pairing offer (or that it expired).
    /// The sealed bytes go: they have done their job.
    pub fn fabric_pairing_set(&mut self, runtime: Uuid, id: &str, state: &str, device_id: Option<&str>, detail: Option<&str>, now: i64) -> Result<Option<FabricPairing>> {
        self.fabric_runtime(runtime)?;
        Ok(self
            .conn
            .query_row(
                &format!(
                    "UPDATE fabric_pairings SET state = ?3, device_id = ?4, detail = ?5, updated_at = ?6, sealed = NULL
                     WHERE runtime_id = ?1 AND id = ?2 RETURNING {PAIRING_COLS}"
                ),
                params![runtime.to_string(), id, state, device_id, detail, now],
                pairing_row,
            )
            .optional()?)
    }

    /// One of the scope's runtimes' live offers not yet received, oldest first.
    pub fn fabric_pairings_undelivered(&self, runtime: Uuid, now: i64) -> Result<Vec<FabricPairing>> {
        self.fabric_runtime(runtime)?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {PAIRING_COLS} FROM fabric_pairings WHERE runtime_id = ?1 AND state IN ('pending', 'sent')
                 AND received_at IS NULL AND expires_at > ?2 ORDER BY created_at, rowid"
        ))?;
        let rows = st.query_map(params![runtime.to_string(), now], pairing_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- expiry ----

    /// Maintenance (System only): mark offers past their time `expired`
    /// and orders that never reached their runtime in time `expired`
    /// (returned, so their people can be told), then delete what is past
    /// retention.
    pub fn fabric_work_expire(&mut self, now: i64) -> Result<(Vec<FabricOrder>, Vec<FabricPairing>)> {
        if !matches!(self.fleet()?, Fleet::All) {
            return Err(StoreError::InvalidOp("expiring fabric rows is maintenance (System scope)".into()));
        }
        let cutoff = now - WORK_RETENTION;
        let tx = self.conn.transaction()?;
        let orders = {
            let mut st = tx.prepare(&format!(
                "UPDATE fabric_orders SET state = 'expired', reason = 'expired', updated_at = ?1,
                     text = CASE WHEN EXISTS (SELECT 1 FROM fabric_projects p WHERE p.user_id = fabric_orders.user_id
                         AND p.name = fabric_orders.project AND p.text_off) THEN NULL ELSE text END,
                     payload = NULL, signature = NULL
                 WHERE state IN ('pending', 'sent') AND received_at IS NULL AND expires_at <= ?1 RETURNING {ORDER_COLS}"
            ))?;
            let rows = st.query_map([now], order_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let pairings = {
            let mut st = tx.prepare(&format!(
                "UPDATE fabric_pairings SET state = 'expired', sealed = NULL, updated_at = ?1
                 WHERE state IN ('pending', 'sent') AND expires_at <= ?1 RETURNING {PAIRING_COLS}"
            ))?;
            let rows = st.query_map([now], pairing_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.execute("DELETE FROM fabric_turns WHERE ended < ?1", [cutoff])?;
        tx.execute("DELETE FROM fabric_orders WHERE updated_at < ?1", [cutoff])?;
        tx.execute("DELETE FROM fabric_attention WHERE updated_at < ?1", [cutoff])?;
        tx.execute("DELETE FROM fabric_pairings WHERE created_at < ?1", [now - PAIRING_ROW_RETENTION])?;
        tx.execute("DELETE FROM fabric_messages WHERE sent_at < ?1", [cutoff])?;
        tx.execute("DELETE FROM fabric_conversations WHERE last_at < ?1", [cutoff])?;
        tx.commit()?;
        Ok((orders, pairings))
    }
}
