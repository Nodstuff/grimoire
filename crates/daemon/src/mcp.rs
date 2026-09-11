//! MCP tools over streamable HTTP (tickets 3.1–3.6, 3.7, #52/#53; AX 2).
//!
//! Every write goes through the propose gate — the MCP surface has no
//! direct-write path by design — and is attributed to an *acting principal*.
//! MCP 2026-07-28 has no sessions (SEP-2567) and rmcp serves it with a fresh
//! `KsMcp` per request, so nothing survives between calls; the principal is
//! resolved per call from, in precedence order: the tool's `as` argument, the
//! `X-Grimoire-Principal` header, `?as=<name>` on the `/mcp` URL,
//! `?cwd=<path>` (→ `claude:<basename>`), else the shared `claude`.
//!
//! How the per-request HTTP values reach a tool: rmcp's streamable-HTTP
//! service consumes the body and injects the remaining `http::request::Parts`
//! into the `RequestContext.extensions` of every request it dispatches, so a
//! tool takes `ctx: RequestContext<RoleServer>` and `RequestHint::from_ctx`
//! reads the header and query string from there — no tower layer or
//! task-local needed (see `RequestHint`).
//!
//! Sixteen tools (asserted by `exactly_sixteen_tools`); write tools answer
//! with a one-line verdict (`render_outcome`) unless `verbose`.

use crate::store_ext::with_store;
use grimoire_store::locate::{self, short_ref};
use grimoire_store::{Block, BlockNode, BlockStore, OpInput, OpKind, ProposeOutcome, ReviewDecision, SqliteStore};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

// ─── dedupe cache ───

/// Idempotency cache: (principal, key) → serialized outcome. Bounded,
/// in-memory, entries expire after `DEDUPE_TTL`. The HTTP API keys it by a
/// client `request_id`; the MCP tools key it by a hash of (tool, doc,
/// canonical payload) — `dedupe_key` — so a retried call within the window
/// returns the original outcome instead of double-applying. Keyed by
/// principal so one agent's retry can never replay another's outcome.
/// Values carry an insertion sequence so eviction drops the OLDEST half
/// instead of clearing — a retry storm never wipes an in-window entry.
pub type DedupeCache = Arc<Mutex<std::collections::HashMap<(Uuid, Uuid), (u64, Instant, Value)>>>;

pub const DEDUPE_CAPACITY: usize = 512;
/// How long a repeated identical write is treated as a retry.
pub const DEDUPE_TTL: Duration = Duration::from_secs(120);
static DEDUPE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn dedupe_get(cache: &DedupeCache, principal: Uuid, id: Uuid) -> Option<Value> {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(principal, id))
        .filter(|(_, at, _)| at.elapsed() < DEDUPE_TTL)
        .map(|(_, _, v)| v.clone())
}

pub fn dedupe_put(cache: &DedupeCache, principal: Uuid, id: Uuid, v: Value) {
    let mut c = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    c.retain(|_, (_, at, _)| at.elapsed() < DEDUPE_TTL);
    if c.len() >= DEDUPE_CAPACITY {
        // evict the oldest half: the newest entries are the ones a retry
        // in flight can still ask for
        let mut seqs: Vec<u64> = c.values().map(|(seq, _, _)| *seq).collect();
        seqs.sort_unstable();
        let cutoff = seqs[seqs.len() / 2];
        c.retain(|_, (seq, _, _)| *seq >= cutoff);
    }
    let seq = DEDUPE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    c.insert((principal, id), (seq, Instant::now(), v));
}

pub fn new_dedupe() -> DedupeCache {
    Arc::new(Mutex::new(std::collections::HashMap::new()))
}

/// The dedupe key of an MCP write: sha256 over (tool, doc, canonical payload)
/// folded into a UUID. serde_json sorts object keys, so equal payloads hash
/// equal regardless of argument order on the wire.
pub fn dedupe_key(tool: &str, doc: Option<Uuid>, payload: &Value) -> Uuid {
    use sha2::Digest;
    let canonical = serde_json::to_string(&json!({"tool": tool, "doc": doc, "payload": payload})).unwrap_or_default();
    let digest = sha2::Sha256::digest(canonical.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

// ─── principals ───

/// Agent principals auto-created since boot (find-or-create by name is an
/// unauthenticated surface: a misbehaving client must not fill the table).
static AUTO_CREATED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub const MAX_AUTO_PRINCIPALS_PER_BOOT: usize = 256;

/// Test-only: swap the per-boot counter, returning what it was. The counter is
/// process-global, so a test that moves it holds `AUTO_CREATED_TEST_LOCK` for
/// the whole window and puts the old value back — otherwise it would refuse
/// creations for every other test in the binary.
#[cfg(test)]
pub static AUTO_CREATED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(test)]
pub fn swap_auto_created_for_test(n: usize) -> usize {
    AUTO_CREATED.swap(n, std::sync::atomic::Ordering::Relaxed)
}

/// Name/UUID string → agent principal id, shared across requests (one per
/// daemon, handed to every `KsMcp` the factory builds — the same pattern as
/// `DedupeCache`). Only a shortcut past the `list_principals` scan:
/// `agent_principal_by_name` stays the source of truth and every entry was
/// resolved through it (or verified via `get_principal`) first.
pub type NameCache = Arc<Mutex<std::collections::HashMap<String, Uuid>>>;

pub fn new_name_cache() -> NameCache {
    Arc::new(Mutex::new(std::collections::HashMap::new()))
}

/// A principal name: 1–64 printable chars (no control characters), trimmed.
pub fn valid_principal_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err("name must be 1-64 printable chars".into());
    }
    Ok(name)
}

/// Find-or-create the Agent principal named `name` (the `as` rule, shared
/// with the HTTP `X-Grimoire-Principal` header). Creation is capped per
/// boot; existing agent names always resolve. A Human or Remote principal
/// carrying the name is refused: an agent never acts as the human.
pub fn agent_principal_by_name(store: &mut SqliteStore, name: &str) -> Result<Uuid, String> {
    let name = valid_principal_name(name)?;
    let existing = store
        .list_principals()
        .ok()
        .and_then(|ps| ps.into_iter().find(|pr| pr.display_name == name));
    if let Some(pr) = existing {
        return match pr.kind {
            grimoire_store::PrincipalKind::Agent => Ok(pr.id),
            kind => Err(format!(
                "{name:?} is the {} principal, not an agent: agents cannot act as it",
                kind.as_str()
            )),
        };
    }
    if AUTO_CREATED.load(std::sync::atomic::Ordering::Relaxed) >= MAX_AUTO_PRINCIPALS_PER_BOOT {
        return Err(format!(
            "too many new agent principals since the daemon started ({MAX_AUTO_PRINCIPALS_PER_BOOT}); \
             reuse an existing name, or restart the daemon"
        ));
    }
    let pr = store
        .create_principal(grimoire_store::PrincipalKind::Agent, name, None)
        .map_err(|e| e.to_string())?;
    AUTO_CREATED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(pr.id)
}

/// Header carrying the acting principal on `/mcp` (same as the HTTP API).
pub const PRINCIPAL_HEADER: &str = "x-grimoire-principal";

/// The per-request principal hints from the HTTP layer, in precedence order
/// below the tool's own `as` argument: header > `?as=` > `?cwd=`. Read from
/// the `http::request::Parts` rmcp injects into the request context.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RequestHint {
    pub header: Option<String>,
    pub query_as: Option<String>,
    pub cwd: Option<String>,
}

impl RequestHint {
    pub fn from_parts(parts: &axum::http::request::Parts) -> Self {
        let header = parts
            .headers
            .get(PRINCIPAL_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let mut query_as = None;
        let mut cwd = None;
        for (k, v) in parts.uri.query().map(query_pairs).unwrap_or_default() {
            match k.as_str() {
                "as" if !v.trim().is_empty() => query_as = Some(v.trim().to_string()),
                "cwd" if !v.trim().is_empty() => cwd = Some(v.trim().to_string()),
                _ => {}
            }
        }
        Self { header, query_as, cwd }
    }

    pub fn from_ctx(ctx: &RequestContext<RoleServer>) -> Self {
        ctx.extensions
            .get::<axum::http::request::Parts>()
            .map(Self::from_parts)
            .unwrap_or_default()
    }

    /// The name this request acts as when the tool has no `as`: header, else
    /// `?as=`, else `claude:<basename of ?cwd=>` (`principal_for_cwd`), else
    /// None (the shared default).
    pub fn default_name(&self) -> Option<String> {
        self.header
            .clone()
            .or_else(|| self.query_as.clone())
            .or_else(|| self.cwd.as_deref().and_then(principal_for_cwd))
    }
}

/// `?cwd=/Users/me/src/qompass` → `claude:qompass`. The basename is trimmed
/// of path separators and whitespace and must pass `valid_principal_name`
/// once prefixed; anything else (empty, `/`, control chars) yields None so
/// the call falls back to the shared `claude` rather than erroring.
pub fn principal_for_cwd(cwd: &str) -> Option<String> {
    let trimmed = cwd.trim().trim_end_matches(['/', '\\']);
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if base.is_empty() || base == "." || base == ".." {
        return None;
    }
    let name = format!("claude:{base}");
    valid_principal_name(&name).ok().map(str::to_string)
}

/// `a=1&b=%2Fx` → [("a","1"),("b","/x")]; percent-decoded, `+` → space.
fn query_pairs(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Who a call acts as. Precedence: `as_` (the tool argument) > `hint` (header
/// > `?as=` > `?cwd=`) > `default` (the shared `claude`). The handle is
/// either an agent principal's UUID — which must exist and be an Agent — or a
/// name, found-or-created. Either form is remembered in `names` so the next
/// request skips the store scan.
pub fn acting_principal(
    store: &mut SqliteStore,
    names: &NameCache,
    as_: Option<&str>,
    hint: &RequestHint,
    default: Uuid,
) -> Result<Uuid, String> {
    let raw = match as_ {
        Some(a) => a.to_string(),
        None => match hint.default_name() {
            Some(n) => n,
            None => return Ok(default),
        },
    };
    let key = raw.trim();
    if let Some(id) = cached_principal(names, key) {
        return Ok(id);
    }
    let id = match Uuid::parse_str(key) {
        Ok(id) => match store.get_principal(id) {
            Ok(pr) if pr.kind == grimoire_store::PrincipalKind::Agent => id,
            Ok(pr) => {
                return Err(format!(
                    "as: {id} is the {} principal {:?}, not an agent: agents cannot act as it",
                    pr.kind.as_str(),
                    pr.display_name
                ));
            }
            Err(_) => return Err(format!("as: no principal with id {id}; pass a name to create one")),
        },
        Err(_) => agent_principal_by_name(store, key).map_err(|m| format!("as: {m}"))?,
    };
    names
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key.to_string(), id);
    Ok(id)
}

fn cached_principal(names: &NameCache, key: &str) -> Option<Uuid> {
    names
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(key)
        .copied()
}

// ─── the server ───

#[derive(Clone)]
pub struct KsMcp {
    store: Arc<Mutex<SqliteStore>>,
    dedupe: DedupeCache,
    /// The freeze: content writes against a live doc are refused (P2.3).
    hot: crate::hot::HotState,
    /// The shared default principal (`claude`).
    agent: Uuid,
    /// Shared `as`-string → principal cache (see `NameCache`).
    names: NameCache,
    /// Block embeddings for the dense leg of `search`/`related`; None when
    /// the model did not load (those tools degrade to keywords and say so).
    embedder: Option<Arc<crate::embed::Embedder>>,
    // referenced only through the #[tool_handler] macro's generated code
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl KsMcp {
    /// The principal this call acts as (`acting_principal`). Skips the store
    /// when no handle is given or it is already cached.
    async fn acting(&self, as_: Option<&str>, hint: &RequestHint) -> Result<Uuid, String> {
        let raw = match as_ {
            Some(a) => a.trim().to_string(),
            None => match hint.default_name() {
                Some(n) => n,
                None => return Ok(self.agent),
            },
        };
        if let Some(id) = cached_principal(&self.names, &raw) {
            return Ok(id);
        }
        let (names, default) = (self.names.clone(), self.agent);
        with_store(&self.store, move |store| {
            acting_principal(store, &names, Some(&raw), &RequestHint::default(), default)
        })
        .await
    }

    pub fn with_embedder(mut self, embedder: Option<Arc<crate::embed::Embedder>>) -> Self {
        self.embedder = embedder;
        self
    }
}

fn ok_json<T: Serialize>(v: &T) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("serialize error: {e}")),
    )]))
}

fn ok_text(s: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(s.into())]))
}

fn err(msg: String) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(msg)]))
}

fn parse_uuid(s: &str, what: &str) -> std::result::Result<Uuid, String> {
    Uuid::parse_str(s.trim()).map_err(|_| format!("{what} is not a valid UUID: {s}"))
}

fn parse_opt_uuid(s: Option<&str>, what: &str) -> std::result::Result<Option<Uuid>, String> {
    s.map(|s| parse_uuid(s, what)).transpose()
}

/// Resolve `^abc123` or a full UUID to a live block anywhere in the store.
pub fn resolve_block(store: &SqliteStore, s: &str) -> Result<Block, String> {
    match locate::parse_block_ref(s) {
        Some(locate::BlockRef::Id(id)) => store.read_block(id).map_err(|e| e.to_string()),
        Some(locate::BlockRef::Suffix(suffix)) => {
            let mut hits = store.blocks_by_id_suffix(&suffix).map_err(|e| e.to_string())?;
            match hits.len() {
                0 => Err(format!("no block ends with ^{suffix}")),
                1 => Ok(hits.remove(0)),
                n => {
                    let docs: Vec<String> = hits
                        .iter()
                        .map(|b| store.get_doc(b.doc_id).map(|d| d.title).unwrap_or_else(|_| b.doc_id.to_string()))
                        .collect();
                    Err(format!(
                        "^{suffix} is ambiguous: {n} blocks match (docs: {}) — use the full block id",
                        docs.join(", ")
                    ))
                }
            }
        }
        None => Err(format!("{s:?} is neither a ^abc123 ref nor a block UUID")),
    }
}

// ─── verdict rendering ───

fn kind_label(k: &OpKind) -> &'static str {
    match k {
        OpKind::Insert { .. } => "insert",
        OpKind::Replace { .. } => "replace",
        OpKind::Delete { .. } => "delete",
        OpKind::Move { .. } => "move",
        OpKind::RenameDoc { .. } => "rename",
        OpKind::MoveDoc { .. } => "move doc",
        OpKind::SetStatus { .. } => "status",
        OpKind::DeleteDoc { .. } => "delete doc",
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}")
}

/// The one-line verdict every write answers with, plus a line per non-green
/// op and a `new:` line listing inserted blocks' short refs:
///
/// `ok · 1 replace · epoch 12→13`
/// `ok · 2 insert · 3 green · 1 yellow (flagged) · epoch 20→21`
/// `parked · 1 red — proposed text preserved · epoch 20 (unchanged)`
///
/// `ops` is the proposed op list in the same order as `out.verdicts`.
pub fn render_outcome(ops: &[OpInput], out: &ProposeOutcome, before: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    let applied = out.verdicts.iter().filter(|v| v.applied).count();
    let greens = out.verdicts.iter().filter(|v| v.verdict == grimoire_store::Verdict::Green).count();
    let yellows = out.verdicts.iter().filter(|v| v.verdict == grimoire_store::Verdict::Yellow).count();
    let reds = out.verdicts.iter().filter(|v| v.verdict == grimoire_store::Verdict::Red).count();
    parts.push(if applied == 0 && reds > 0 { "parked" } else { "ok" }.to_string());

    // op kinds, in first-seen order
    let mut kinds: Vec<(&'static str, usize)> = Vec::new();
    for op in ops {
        let label = kind_label(&op.kind);
        match kinds.iter_mut().find(|(l, _)| *l == label) {
            Some((_, n)) => *n += 1,
            None => kinds.push((label, 1)),
        }
    }
    for (label, n) in &kinds {
        parts.push(plural(*n, label));
    }
    if yellows + reds > 0 {
        if greens > 0 {
            parts.push(plural(greens, "green"));
        }
        if yellows > 0 {
            parts.push(format!("{} (flagged)", plural(yellows, "yellow")));
        }
        if reds > 0 {
            parts.push(format!("{} — proposed text preserved", plural(reds, "red")));
        }
    }
    if out.epoch != before {
        parts.push(format!("epoch {before}→{}", out.epoch));
    } else {
        parts.push(format!("epoch {before} (unchanged)"));
    }
    let mut text = parts.join(" · ");

    for v in out.verdicts.iter().filter(|v| v.verdict != grimoire_store::Verdict::Green) {
        let target = v.block_id.map(short_ref).unwrap_or_else(|| "doc".into());
        let note = v.note.trim();
        text.push('\n');
        text.push_str(&format!("{} {target} {note}", v.verdict.as_str()).trim_end());
    }
    let new: Vec<String> = ops
        .iter()
        .zip(out.verdicts.iter())
        .filter(|(op, v)| matches!(op.kind, OpKind::Insert { .. }) && v.applied)
        .filter_map(|(_, v)| v.block_id.map(short_ref))
        .collect();
    if !new.is_empty() {
        text.push_str("\nnew: ");
        text.push_str(&new.join(" "));
    }
    text
}

/// `ok · no changes · epoch N`.
fn no_changes(epoch: i64) -> Result<CallToolResult, McpError> {
    ok_text(format!("ok · no changes · epoch {epoch}"))
}

/// A write's answer: the one-line verdict, or the full JSON with `verbose`.
fn answer(ops: &[OpInput], out: &ProposeOutcome, before: i64, verbose: bool) -> Result<CallToolResult, McpError> {
    if verbose {
        ok_json(out)
    } else {
        ok_text(render_outcome(ops, out, before))
    }
}

/// What the dedupe cache stores for a write: both renderings, so a replay
/// answers in whichever shape the retry asks for.
fn stored(text: &str, full: &Value) -> Value {
    json!({"text": text, "full": full})
}

fn replay(prev: &Value, verbose: bool) -> Result<CallToolResult, McpError> {
    if verbose {
        ok_json(&prev["full"])
    } else {
        ok_text(prev["text"].as_str().unwrap_or_default())
    }
}

/// The verdict of a write or a store error, as the tool's answer; records the
/// outcome under `key` for retries.
fn finish(
    dedupe: &DedupeCache,
    principal: Uuid,
    key: Uuid,
    ops: &[OpInput],
    res: grimoire_store::Result<ProposeOutcome>,
    before: i64,
    verbose: bool,
) -> Result<CallToolResult, McpError> {
    match res {
        Ok(out) => {
            let text = render_outcome(ops, &out, before);
            dedupe_put(dedupe, principal, key, stored(&text, &serde_json::to_value(&out).unwrap_or_default()));
            answer(ops, &out, before, verbose)
        }
        Err(e) => err(e.to_string()),
    }
}

// ─── params ───

#[derive(Deserialize, JsonSchema)]
pub struct ReadDocParams {
    /// Doc UUID.
    pub doc_id: String,
    /// Omit for the markdown (default). "outline": block ids, types, first
    /// lines as JSON (token-cheap shape of a big doc).
    pub mode: Option<String>,
    /// true: a `^abc123` short-ref line above every block (quote them in
    /// propose / add_comment; propose_markdown strips them).
    pub refs: Option<bool>,
    /// Only this section: a heading path ('grimoire › Plans', '## x / ### y')
    /// or a block ref.
    pub section: Option<String>,
    /// Only this block: `^abc123` or a block UUID.
    pub block: Option<String>,
    /// true: append a `## comments` section listing the doc's comment threads.
    pub comments: Option<bool>,
    /// Outline only: include provenance fields on blocks and the doc.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct EditDocParams {
    /// Doc UUID.
    pub doc_id: String,
    /// Exact text to replace (as read_doc shows it). Whitespace differences
    /// are tolerated as a fallback. Must match exactly once unless replace_all.
    pub old: String,
    /// Replacement text (may be empty to delete, or span several blocks).
    pub new: String,
    /// Replace every occurrence (default false: >1 match is an error listing them).
    pub replace_all: Option<bool>,
    #[doc = "Who this call acts as: 'claude:<project>-<task>' or an agent principal UUID. Beats the X-Grimoire-Principal header and ?as=/?cwd= on the /mcp URL; without any, writes land on the shared 'claude'."]
    #[serde(rename = "as")]
    pub as_: Option<String>,
    /// true: the full JSON verdicts instead of the one-line summary.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AppendParams {
    /// Doc UUID.
    pub doc_id: String,
    /// The markdown to add (one or many blocks).
    pub markdown: String,
    /// Where: a heading path ('grimoire › Plans', '## grimoire / ### Plans')
    /// or a block ref (^abc123 / UUID). Omit for the end of the doc.
    pub to: Option<String>,
    /// "end" (default: after the section's last block) or "start" (right after its heading).
    pub at: Option<String>,
    /// true: create the headings of `to` that do not exist yet (levels from
    /// the path's `#`s if given, else parent level + 1; a new top-level
    /// section takes the level of the doc's existing top-level headings, or
    /// `#` when it has none). An ambiguous path is still an error.
    pub create_missing: Option<bool>,
    #[doc = "Who this call acts as: 'claude:<project>-<task>' or an agent principal UUID. Beats the X-Grimoire-Principal header and ?as=/?cwd= on the /mcp URL; without any, writes land on the shared 'claude'."]
    #[serde(rename = "as")]
    pub as_: Option<String>,
    /// true: the full JSON verdicts instead of the one-line summary.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct FindDocParams {
    /// Title or path fragment, e.g. "roadmap", "daily 2026-09-08", "revew gate" (typos ok).
    pub query: String,
    /// Restrict to this doc's subtree (UUID).
    pub parent_doc_id: Option<String>,
    /// Max matches (default 8).
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocOpParams {
    /// "rename" | "move" | "status" | "delete" | "merge".
    pub op: String,
    /// The doc acted on (for merge: the doc whose content moves and is then trashed).
    pub doc_id: String,
    /// rename: the new title.
    pub title: Option<String>,
    /// move: destination parent (UUID); omit for the root level.
    pub new_parent_id: Option<String>,
    /// move: land right after this sibling (UUID, a child of the new parent); omit to append last.
    pub after_doc_id: Option<String>,
    /// status: "draft" | "in-review" | "decided" | "superseded"; omit or "null" to clear.
    pub status: Option<String>,
    /// merge: the doc that receives doc_id's content.
    pub into_doc_id: Option<String>,
    #[doc = "Who this call acts as: 'claude:<project>-<task>' or an agent principal UUID. Beats the X-Grimoire-Principal header and ?as=/?cwd= on the /mcp URL; without any, writes land on the shared 'claude'."]
    #[serde(rename = "as")]
    pub as_: Option<String>,
    /// true: the full JSON verdicts instead of the one-line summary.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeParams {
    /// Doc UUID.
    pub doc_id: String,
    /// The doc epoch your read was based on (from read_doc's first line).
    pub base_epoch: i64,
    /// Ops array. Each op: {"kind": {"op": "insert"|"replace"|"delete"|"move", ...},
    /// "source_refs": ["..."]}. insert: parent_id (block UUID or null),
    /// order_key ("" = last sibling, "after:<block-uuid>" = after that block),
    /// block_type, content, optional block_id. replace: target, content.
    /// delete: target. move: target, new_parent, new_order_key.
    pub ops: Value,
    #[doc = "Who this call acts as: 'claude:<project>-<task>' or an agent principal UUID. Beats the X-Grimoire-Principal header and ?as=/?cwd= on the /mcp URL; without any, writes land on the shared 'claude'."]
    #[serde(rename = "as")]
    pub as_: Option<String>,
    /// true: the full JSON verdicts instead of the one-line summary.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeMarkdownParams {
    /// Doc UUID.
    pub doc_id: String,
    /// The doc epoch your read was based on (from read_doc's first line).
    pub base_epoch: i64,
    /// The doc's complete new markdown (frontmatter included; `^abc123` ref lines may stay).
    pub markdown: String,
    #[doc = "Who this call acts as: 'claude:<project>-<task>' or an agent principal UUID. Beats the X-Grimoire-Principal header and ?as=/?cwd= on the /mcp URL; without any, writes land on the shared 'claude'."]
    #[serde(rename = "as")]
    pub as_: Option<String>,
    /// true: the full JSON verdicts instead of the one-line summary.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DiffSinceParams {
    /// Doc UUID.
    pub doc_id: String,
    /// Return ops applied after this epoch.
    pub since_epoch: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposalsParams {
    /// "mine" (default): what happened to your proposals (as: your name).
    /// "pending": open review annotations awaiting a human, oldest first.
    pub kind: Option<String>,
    /// pending: restrict to one doc (UUID).
    pub doc_id: Option<String>,
    /// Max entries (default 20 for mine, 50 for pending).
    pub limit: Option<u32>,
    /// Include full prior blocks and op bookkeeping (default false).
    pub verbose: Option<bool>,
    #[doc = "Who this call acts as: 'claude:<project>-<task>' or an agent principal UUID. Beats the X-Grimoire-Principal header and ?as=/?cwd= on the /mcp URL; without any, writes land on the shared 'claude'."]
    #[serde(rename = "as")]
    pub as_: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ResolveParams {
    /// Annotation UUID from proposals(kind: "pending").
    pub annotation_id: String,
    /// "accept" or "decline".
    pub decision: String,
    #[doc = "Who this call acts as: 'claude:<project>-<task>' or an agent principal UUID. Beats the X-Grimoire-Principal header and ?as=/?cwd= on the /mcp URL; without any, writes land on the shared 'claude'."]
    #[serde(rename = "as")]
    pub as_: Option<String>,
    /// true: the full JSON receipt instead of the one-line summary.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateDocParams {
    pub title: String,
    /// Parent doc UUID for tree placement; omit for root.
    pub parent_doc_id: Option<String>,
    /// Initial content: the whole doc as markdown, written in the same call.
    pub markdown: Option<String>,
    /// "error" (default): fail if a live doc with this exact title already
    /// exists under the parent. "reuse": return that doc instead (markdown
    /// ignored) — an atomic find-or-create.
    pub if_exists: Option<String>,
    #[doc = "Who this call acts as: 'claude:<project>-<task>' or an agent principal UUID. Beats the X-Grimoire-Principal header and ?as=/?cwd= on the /mcp URL; without any, writes land on the shared 'claude'."]
    #[serde(rename = "as")]
    pub as_: Option<String>,
    /// true: JSON {id, title, parent_id, epoch, reused} instead of the one-line summary.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddCommentParams {
    /// The content block the comment anchors to: `^abc123` or a block UUID.
    pub block_id: String,
    pub text: String,
    /// Comment to reply to (ref or UUID, same thread); omit for a new thread.
    pub reply_to: Option<String>,
    #[doc = "Who this call acts as: 'claude:<project>-<task>' or an agent principal UUID. Beats the X-Grimoire-Principal header and ?as=/?cwd= on the /mcp URL; without any, writes land on the shared 'claude'."]
    #[serde(rename = "as")]
    pub as_: Option<String>,
    /// true: the comment block as JSON instead of the one-line summary.
    pub verbose: Option<bool>,
}

#[derive(Serialize)]
struct FlatBlock {
    id: Uuid,
    parent_id: Option<Uuid>,
    depth: usize,
    block_type: &'static str,
    content: String,
}

fn flatten(nodes: &[BlockNode], depth: usize, out: &mut Vec<FlatBlock>) {
    for n in nodes {
        out.push(FlatBlock {
            id: n.block.id,
            parent_id: n.block.parent_id,
            depth,
            block_type: n.block.block_type.as_str(),
            content: locate::first_line(&n.block.content, 100),
        });
        flatten(&n.children, depth + 1, out);
    }
}

/// `doc <uuid> · epoch <N> · <Folder › Sub › Title>`.
fn doc_header(store: &SqliteStore, doc: &grimoire_store::Doc) -> String {
    let path = store
        .list_docs()
        .ok()
        .map(|docs| crate::nav::breadcrumbs(&docs))
        .and_then(|c| c.get(&doc.id).cloned())
        .unwrap_or_else(|| doc.title.clone());
    format!("doc {} · epoch {} · {path}", doc.id, doc.current_epoch)
}

/// Blocks as markdown, optionally each under its `^ref` line.
fn render_blocks(blocks: &[&Block], refs: bool) -> String {
    let parts: Vec<String> = blocks
        .iter()
        .map(|b| {
            if refs {
                format!("{}\n{}", short_ref(b.id), b.content)
            } else {
                b.content.clone()
            }
        })
        .collect();
    let mut md = parts.join("\n\n");
    if !md.is_empty() {
        md.push('\n');
    }
    md
}

/// The `## comments` section: one line per comment, threads indented,
/// anchored by the target block's ref and first line.
fn render_comments(store: &SqliteStore, roots: &[BlockNode]) -> String {
    fn walk<'a>(nodes: &'a [BlockNode], out: &mut Vec<&'a Block>) {
        for n in nodes {
            if n.block.block_type == grimoire_store::BlockType::Comment {
                out.push(&n.block);
            }
            walk(&n.children, out);
        }
    }
    let mut comments = Vec::new();
    walk(roots, &mut comments);
    let mut out = String::from("## comments\n");
    if comments.is_empty() {
        out.push_str("(none)\n");
        return out;
    }
    let names: std::collections::HashMap<Uuid, String> = store
        .list_principals()
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.id, p.display_name))
        .collect();
    let who = |id: Uuid| names.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let content = locate::content_blocks(roots);
    let anchor_line = |id: Uuid| {
        content
            .iter()
            .find(|b| b.id == id)
            .map(|b| locate::first_line(&b.content, 60))
            .unwrap_or_default()
    };
    // depth of a comment in its thread = number of comment ancestors
    let depth_of = |c: &Block| {
        let mut d = 0;
        let mut cur = c.parent_id;
        while let Some(p) = cur {
            match comments.iter().find(|x| x.id == p) {
                Some(px) => {
                    d += 1;
                    cur = px.parent_id;
                }
                None => break,
            }
        }
        d
    };
    let mut last_anchor: Option<Uuid> = None;
    for c in &comments {
        if c.refers_to != last_anchor && depth_of(c) == 0 {
            if let Some(a) = c.refers_to {
                out.push_str(&format!("on {} “{}”\n", short_ref(a), anchor_line(a)));
            }
            last_anchor = c.refers_to;
        }
        let indent = "  ".repeat(depth_of(c) + 1);
        out.push_str(&format!(
            "{indent}- {} {}: {}\n",
            short_ref(c.id),
            who(c.created_by),
            locate::first_line(&c.content, 200)
        ));
    }
    out
}

/// Where `append` lands, in content-export byte offsets.
struct Landing {
    offset: usize,
    /// Headings to create before the markdown (create_missing), already
    /// formatted (`### Plans`).
    new_headings: Vec<String>,
}

/// Resolve `to`/`at`/`create_missing` against the tree. `md` is the content
/// export the offset indexes into.
fn resolve_landing(
    store: &SqliteStore,
    doc_id: Uuid,
    roots: &[BlockNode],
    md: &str,
    to: Option<&str>,
    at_start: bool,
    create_missing: bool,
) -> Result<Landing, String> {
    let Some(to) = to.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Landing { offset: md.len(), new_headings: Vec::new() });
    };
    // a block ref anchors directly
    if locate::parse_block_ref(to).is_some() {
        let b = resolve_block(store, to)?;
        if b.doc_id != doc_id {
            return Err(format!("{to} is in another doc ({})", b.doc_id));
        }
        let (start, end) = locate::section_insert_offsets(roots, b.id)
            .ok_or_else(|| format!("{to} is not a content block of this doc"))?;
        return Ok(Landing { offset: if at_start { start } else { end }, new_headings: Vec::new() });
    }
    match locate::resolve_heading_path(roots, to) {
        Ok(h) => {
            let (start, end) = locate::section_insert_offsets(roots, h.id).ok_or("section vanished")?;
            Ok(Landing { offset: if at_start { start } else { end }, new_headings: Vec::new() })
        }
        Err(e @ locate::LocateError::Ambiguous { .. }) => Err(e.to_string()),
        Err(e @ locate::LocateError::NotFound { .. }) => {
            if !create_missing {
                return Err(format!("{e}; pass create_missing: true to create it"));
            }
            let segments = locate::split_path(to);
            if segments.is_empty() {
                return Err(format!("{to:?} is not a heading path"));
            }
            // the longest prefix that resolves uniquely is the parent
            let mut parent: Option<locate::HeadingEntry> = None;
            let mut k = segments.len() - 1;
            while k > 0 {
                match locate::resolve_heading_path(roots, &segments[..k].join(" › ")) {
                    Ok(h) => {
                        parent = Some(h);
                        break;
                    }
                    Err(e @ locate::LocateError::Ambiguous { .. }) => return Err(e.to_string()),
                    Err(_) => k -= 1,
                }
            }
            let offset = match &parent {
                Some(h) => locate::section_insert_offsets(roots, h.id).ok_or("section vanished")?.1,
                None => md.len(),
            };
            let mut level = match &parent {
                Some(h) => h.level + 1,
                None => locate::root_heading_level(roots),
            };
            let mut new_headings = Vec::new();
            for seg in &segments[k..] {
                let hashes = seg.chars().take_while(|c| *c == '#').count() as u8;
                if hashes > 0 {
                    level = hashes;
                }
                let level_now = level.clamp(1, 6);
                let text = seg.trim_start_matches('#').trim();
                new_headings.push(format!("{} {text}", "#".repeat(level_now as usize)));
                level = level_now + 1;
            }
            Ok(Landing { offset, new_headings })
        }
    }
}

// ─── the tools ───

#[tool_router]
impl KsMcp {
    pub fn new(
        store: Arc<Mutex<SqliteStore>>,
        agent: Uuid,
        dedupe: DedupeCache,
        names: NameCache,
        hot: crate::hot::HotState,
    ) -> Self {
        Self {
            store,
            dedupe,
            hot,
            agent,
            names,
            embedder: None,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Find docs by name: fuzzy match over titles and breadcrumb paths ('Folder › Sub › Title'), typo-tolerant, ranked exact > prefix > substring > path words > fuzzy. Returns {id, title, path, parent_id, epoch, status}. Start here; then read_doc or edit_doc with the id."
    )]
    async fn find_doc(&self, Parameters(p): Parameters<FindDocParams>) -> Result<CallToolResult, McpError> {
        let parent = match parse_opt_uuid(p.parent_doc_id.as_deref(), "parent_doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let limit = p.limit.unwrap_or(8).clamp(1, 50) as usize;
        with_store(&self.store, move |store| match store.list_docs() {
            Ok(docs) => ok_json(&crate::nav::find_docs(&docs, &p.query, parent, limit)),
            Err(e) => err(e.to_string()),
        })
        .await
    }

    #[tool(
        description = "The map: the doc tree to `depth` (default 2) with ids and block counts, then the most-linked docs with their first paragraph and the tags in scope, within a token budget. Call once to orient in the corpus or a subtree (root_doc_id); says when truncated."
    )]
    async fn orient(&self, Parameters(p): Parameters<crate::retrieval::OrientParams>) -> Result<CallToolResult, McpError> {
        let root = match parse_opt_uuid(p.root_doc_id.as_deref(), "root_doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let max_tokens = p.max_tokens.unwrap_or(1500).clamp(100, 20_000) as usize;
        let depth = p.depth.unwrap_or(2).clamp(1, 12) as usize;
        with_store(&self.store, move |store| match crate::retrieval::orient(store, root, max_tokens, depth) {
            Ok(text) => ok_text(text),
            Err(m) => err(m),
        })
        .await
    }

    #[tool(
        description = "Read a doc as markdown: first line 'doc <id> · epoch <N> · <path>', then the content (comments and canvases excluded). refs: true puts a ^abc123 line above each block; section: a heading path or ref reads just that section; block: just one block; comments: true appends the comment threads; mode 'outline' gives the JSON shape instead."
    )]
    async fn read_doc(&self, Parameters(p): Parameters<ReadDocParams>) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let mode = p.mode.clone().unwrap_or_else(|| "markdown".into());
        if !matches!(mode.as_str(), "outline" | "markdown") {
            return err(format!("mode must be outline or omitted (markdown), got {mode}"));
        }
        let refs = p.refs.unwrap_or(false);
        with_store(&self.store, move |store| {
            let tree = match store.read_doc(doc_id) {
                Ok(t) => t,
                Err(e) => return err(e.to_string()),
            };
            if mode == "outline" {
                let doc = json!({
                    "id": tree.doc.id,
                    "title": tree.doc.title,
                    "parent_id": tree.doc.parent_id,
                    "status": tree.doc.status,
                    "review_policy": tree.doc.review_policy,
                });
                let mut blocks = Vec::new();
                flatten(&tree.roots, 0, &mut blocks);
                return ok_json(&json!({
                    "doc": if p.verbose.unwrap_or(false) { json!(tree.doc) } else { doc },
                    "epoch": tree.doc.current_epoch,
                    "mode": "outline",
                    "blocks": blocks,
                }));
            }
            let mut out = doc_header(store, &tree.doc);
            out.push('\n');
            if let Some(b) = p.block.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                let block = match resolve_block(store, b) {
                    Ok(b) => b,
                    Err(m) => return err(m),
                };
                if block.doc_id != doc_id {
                    return err(format!("{b} is in another doc ({})", block.doc_id));
                }
                out.push_str(&format!("block {} · {}\n\n", short_ref(block.id), block.block_type.as_str()));
                out.push_str(&block.content);
                out.push('\n');
            } else if let Some(sec) = p.section.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                let anchor = if locate::parse_block_ref(sec).is_some() {
                    match resolve_block(store, sec) {
                        Ok(b) if b.doc_id == doc_id => (b.id, short_ref(b.id)),
                        Ok(b) => return err(format!("{sec} is in another doc ({})", b.doc_id)),
                        Err(m) => return err(m),
                    }
                } else {
                    match locate::resolve_heading_path(&tree.roots, sec) {
                        Ok(h) => (h.id, h.display()),
                        Err(e) => return err(e.to_string()),
                    }
                };
                let span = locate::section_span(&tree.roots, anchor.0);
                out.push_str(&format!("section {}\n\n", anchor.1));
                out.push_str(&render_blocks(&span, refs));
            } else {
                out.push('\n');
                out.push_str(&render_blocks(&locate::content_blocks(&tree.roots), refs));
            }
            if p.comments.unwrap_or(false) {
                out.push('\n');
                out.push_str(&render_comments(store, &tree.roots));
            }
            ok_text(out)
        })
        .await
    }

    #[tool(
        description = "Edit like the Edit tool: replace `old` (exact text from read_doc; whitespace-tolerant) with `new` in one doc — no epoch needed, the diff is taken against the live doc and proposed through the gate. Exactly one match unless replace_all; zero matches names the closest block, several list their ^refs so you can widen `old`. Answers with a one-line verdict."
    )]
    async fn edit_doc(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(p): Parameters<EditDocParams>,
    ) -> Result<CallToolResult, McpError> {
        self.edit_doc_impl(RequestHint::from_ctx(&ctx), p).await
    }

    #[tool(
        description = "Add markdown to a doc without an epoch: at the end (default), or into a section named by heading path ('grimoire › Plans', '## x / ### y') or block ref, at: 'end' | 'start'. create_missing: true creates absent headings (the daily-log case). One-line verdict."
    )]
    async fn append(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(p): Parameters<AppendParams>,
    ) -> Result<CallToolResult, McpError> {
        self.append_impl(RequestHint::from_ctx(&ctx), p).await
    }

    #[tool(
        description = "Full rewrite: hand over the doc's complete new markdown with the epoch from your read_doc; the server diffs it against the current blocks (unchanged blocks keep ids and comments) and proposes the minimal ops. A stale epoch is refused with the missed ops — re-read and re-send. Prefer edit_doc/append for local changes."
    )]
    async fn propose_markdown(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(p): Parameters<ProposeMarkdownParams>,
    ) -> Result<CallToolResult, McpError> {
        self.propose_markdown_impl(RequestHint::from_ctx(&ctx), p).await
    }

    #[tool(
        description = "Surgical block ops (insert/replace/delete/move by block id) at a base_epoch from read_doc; a stale base is scored per op (unchanged targets still green). Inserts may omit block_id; order_key '' appends, 'after:<uuid>' places. Use edit_doc/append unless you need explicit ops."
    )]
    async fn propose(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(p): Parameters<ProposeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.propose_impl(RequestHint::from_ctx(&ctx), p).await
    }

    #[tool(description = "Ops applied to a doc after an epoch — what you missed. Use after a stale-epoch refusal, or to see what changed.")]
    async fn diff_since(&self, Parameters(p): Parameters<DiffSinceParams>) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        with_store(&self.store, move |store| match store.ops_since(doc_id, p.since_epoch) {
            Ok(ops) => ok_json(&ops),
            Err(e) => err(e.to_string()),
        })
        .await
    }

    #[tool(
        description = "Ranked search over live blocks: exact phrase first, then all-words, then fuzzy and by-meaning. Compact hits {doc_id, path, block_id, snippet, score}; kind 'docs' groups by doc; scope_doc_id restricts to a subtree."
    )]
    async fn search(&self, Parameters(p): Parameters<crate::retrieval::SearchParams>) -> Result<CallToolResult, McpError> {
        let scope = match parse_opt_uuid(p.scope_doc_id.as_deref(), "scope_doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let kind = p.kind.clone().unwrap_or_else(|| "blocks".into());
        if kind != "blocks" && kind != "docs" {
            return err(format!("kind must be blocks|docs, got {kind}"));
        }
        let opts = crate::retrieval::SearchOpts {
            scope,
            exclude_answers: p.exclude_answers.unwrap_or(true),
            limit: p.limit.unwrap_or(10).clamp(1, 100) as usize,
        };
        let embedder = self.embedder.clone();
        with_store(&self.store, move |store| {
            let emb = embedder.as_deref();
            let out = if kind == "docs" {
                crate::retrieval::search_docs(store, emb, &p.query, opts).map(|h| json!(h))
            } else {
                crate::retrieval::search_blocks(store, emb, &p.query, opts).map(|h| json!(h))
            };
            match out {
                Ok(v) => ok_json(&v),
                Err(m) => err(m),
            }
        })
        .await
    }

    #[tool(
        description = "Exhaustive regex sweep (Rust syntax, per line of block content) grouped by doc: every match with totals and a truncated flag — raise max_groups/max_matches_per_group for the rest. Comments and frontmatter skipped unless include_hidden."
    )]
    async fn grep(&self, Parameters(p): Parameters<crate::retrieval::GrepParams>) -> Result<CallToolResult, McpError> {
        let scope = match parse_opt_uuid(p.scope_doc_id.as_deref(), "scope_doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let opts = crate::retrieval::GrepOpts {
            scope,
            case_insensitive: p.case_insensitive.unwrap_or(false),
            max_groups: p.max_groups.unwrap_or(20).clamp(1, 500) as usize,
            max_matches_per_group: p.max_matches_per_group.unwrap_or(5).clamp(1, 200) as usize,
            include_hidden: p.include_hidden.unwrap_or(false),
        };
        with_store(&self.store, move |store| match crate::retrieval::grep(store, &p.pattern, opts) {
            Ok(out) => ok_json(&out),
            Err(m) => err(m),
        })
        .await
    }

    #[tool(
        description = "From a block or doc, what else matters: docs that [[link]] to it (why=backlink), nearest blocks by meaning (why=similar), same-folder docs (why=sibling). by: 'tag' + tag lists the docs carrying a tag; tags: true lists every tag with counts."
    )]
    async fn related(&self, Parameters(p): Parameters<crate::retrieval::RelatedParams>) -> Result<CallToolResult, McpError> {
        if p.tags.unwrap_or(false) {
            return with_store(&self.store, move |store| match store.list_tags() {
                Ok(t) => ok_json(&t.into_iter().map(|(tag, n)| json!({"tag": tag, "docs": n})).collect::<Vec<_>>()),
                Err(e) => err(e.to_string()),
            })
            .await;
        }
        if p.by.as_deref() == Some("tag") {
            let Some(tag) = p.tag.clone().filter(|t| !t.trim().is_empty()) else {
                return err("by: 'tag' needs tag".into());
            };
            return with_store(&self.store, move |store| match store.docs_by_tag(tag.trim()) {
                Ok(docs) => {
                    let crumbs = store.list_docs().map(|all| crate::nav::breadcrumbs(&all)).unwrap_or_default();
                    ok_json(
                        &docs
                            .iter()
                            .map(|d| {
                                json!({"id": d.id, "title": d.title, "path": crumbs.get(&d.id).cloned().unwrap_or_else(|| d.title.clone()), "epoch": d.current_epoch})
                            })
                            .collect::<Vec<_>>(),
                    )
                }
                Err(e) => err(e.to_string()),
            })
            .await;
        }
        if let Some(by) = p.by.as_deref() {
            return err(format!("by must be 'tag' (or omitted), got {by}"));
        }
        let limit = p.limit.unwrap_or(8).clamp(1, 50) as usize;
        let embedder = self.embedder.clone();
        with_store(&self.store, move |store| {
            let anchor = match (p.block_id.as_deref(), p.doc_id.as_deref()) {
                (Some(b), _) => match resolve_block(store, b) {
                    Ok(b) => crate::retrieval::Anchor::Block(b.id),
                    Err(m) => return err(m),
                },
                (None, Some(d)) => match parse_uuid(d, "doc_id") {
                    Ok(u) => crate::retrieval::Anchor::Doc(u),
                    Err(m) => return err(m),
                },
                (None, None) => return err("pass block_id or doc_id (or by: 'tag' / tags: true)".into()),
            };
            match crate::retrieval::related(store, embedder.as_deref(), anchor, limit) {
                Ok(out) => ok_json(&out),
                Err(m) => err(m),
            }
        })
        .await
    }

    #[tool(
        description = "Create a doc, optionally with its whole markdown in the same call. if_exists 'reuse' is an atomic find-or-create by exact title under the parent (use it for daily docs); the default 'error' refuses a duplicate. Answers 'ok · created|reused \"Title\" · doc <id> · epoch N'."
    )]
    async fn create_doc(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(p): Parameters<CreateDocParams>,
    ) -> Result<CallToolResult, McpError> {
        self.create_doc_impl(RequestHint::from_ctx(&ctx), p).await
    }

    #[tool(
        description = "Tree ops through the gate: op 'rename' (title), 'move' (new_parent_id, after_doc_id), 'status' (status) land as flagged yellows a reviewer can decline; 'delete' is always red (parked until a human trashes it); 'merge' (into_doc_id) appends doc_id's content as yellows then parks a red delete. Refused on docs shared with you."
    )]
    async fn doc_op(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(p): Parameters<DocOpParams>,
    ) -> Result<CallToolResult, McpError> {
        self.doc_op_impl(RequestHint::from_ctx(&ctx), p).await
    }

    #[tool(
        description = "Comment on a block (^abc123 or UUID), or reply within a thread via reply_to. Comments are blocks with provenance; threads survive edits. Read them with read_doc(comments: true)."
    )]
    async fn add_comment(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(p): Parameters<AddCommentParams>,
    ) -> Result<CallToolResult, McpError> {
        self.add_comment_impl(RequestHint::from_ctx(&ctx), p).await
    }

    #[tool(
        description = "Resolve one review annotation (from proposals kind 'pending'): accept or decline, as this agent. You cannot resolve your own proposals (proposer ≠ approver)."
    )]
    async fn resolve(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(p): Parameters<ResolveParams>,
    ) -> Result<CallToolResult, McpError> {
        self.resolve_impl(RequestHint::from_ctx(&ctx), p).await
    }

    #[tool(
        description = "kind 'mine' (default): what happened to your proposals — each op, its verdict, whether its annotation was accepted/declined/open and by whom; learn from declines. kind 'pending': open yellows and parked reds awaiting a human, oldest first (doc_id to filter)."
    )]
    async fn proposals(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(p): Parameters<ProposalsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.proposals_impl(RequestHint::from_ctx(&ctx), p).await
    }
}

// ─── tool bodies (testable without a RequestContext) ───

impl KsMcp {
    pub(crate) async fn edit_doc_impl(&self, hint: RequestHint, p: EditDocParams) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let verbose = p.verbose.unwrap_or(false);
        let principal = match self.acting(p.as_.as_deref(), &hint).await {
            Ok(id) => id,
            Err(m) => return err(m),
        };
        let key = dedupe_key(
            "edit_doc",
            Some(doc_id),
            &json!({"old": p.old, "new": p.new, "replace_all": p.replace_all.unwrap_or(false)}),
        );
        if let Some(prev) = dedupe_get(&self.dedupe, principal, key) {
            return replay(&prev, verbose);
        }
        if let Err(m) = self.hot.assert_cold(doc_id) {
            return err(m);
        }
        let dedupe = self.dedupe.clone();
        with_store(&self.store, move |store| {
            if let Some(m) = crate::api::refuse_if_mirror(store, doc_id, "editing") {
                return err(m);
            }
            let tree = match store.read_doc(doc_id) {
                Ok(t) => t,
                Err(e) => return err(e.to_string()),
            };
            let epoch = tree.doc.current_epoch;
            if p.old == p.new {
                return no_changes(epoch);
            }
            if p.old.trim().is_empty() {
                return err("old must not be empty (use append to add text)".into());
            }
            let (export, spans) = locate::export_with_offsets(&tree.roots);
            let mut ranges: Vec<std::ops::Range<usize>> = locate::find_all(&export, &p.old)
                .into_iter()
                .map(|s| s..s + p.old.len())
                .collect();
            if ranges.is_empty() {
                ranges = locate::find_all_normalised(&export, &p.old);
            }
            if ranges.is_empty() {
                let blocks = locate::content_blocks(&tree.roots);
                let hint = locate::closest_block(&blocks, &p.old)
                    .map(|b| format!("; closest block {} “{}”", short_ref(b.id), locate::first_line(&b.content, 200)))
                    .unwrap_or_default();
                return err(format!("old not found in doc (epoch {epoch}){hint}"));
            }
            if ranges.len() > 1 && !p.replace_all.unwrap_or(false) {
                let list: Vec<String> = ranges
                    .iter()
                    .map(|r| {
                        let id = locate::block_at(&spans, r.start);
                        let first = tree_first_line(&tree.roots, id);
                        format!("{} “{}”", id.map(short_ref).unwrap_or_else(|| "?".into()), first)
                    })
                    .collect();
                return err(format!(
                    "old matches {} times — widen it (or replace_all: true): {}",
                    ranges.len(),
                    list.join(" | ")
                ));
            }
            let mut new_md = export.clone();
            for r in ranges.iter().rev() {
                new_md = locate::splice_replace(&new_md, r.clone(), &p.new);
            }
            let ops = grimoire_store::mddiff::markdown_to_ops_from(&tree.roots, &new_md, "edit_doc");
            if ops.is_empty() {
                return no_changes(epoch);
            }
            let res = store.propose(doc_id, epoch, principal, ops.clone());
            finish(&dedupe, principal, key, &ops, res, epoch, verbose)
        })
        .await
    }

    pub(crate) async fn append_impl(&self, hint: RequestHint, p: AppendParams) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let at_start = match p.at.as_deref().map(str::trim).unwrap_or("end") {
            "end" | "" => false,
            "start" => true,
            other => return err(format!("at must be end | start, got {other}")),
        };
        if p.markdown.trim().is_empty() {
            return err("markdown must not be empty".into());
        }
        let verbose = p.verbose.unwrap_or(false);
        let principal = match self.acting(p.as_.as_deref(), &hint).await {
            Ok(id) => id,
            Err(m) => return err(m),
        };
        let key = dedupe_key(
            "append",
            Some(doc_id),
            &json!({"markdown": p.markdown, "to": p.to, "at": at_start, "create_missing": p.create_missing.unwrap_or(false)}),
        );
        if let Some(prev) = dedupe_get(&self.dedupe, principal, key) {
            return replay(&prev, verbose);
        }
        if let Err(m) = self.hot.assert_cold(doc_id) {
            return err(m);
        }
        let dedupe = self.dedupe.clone();
        with_store(&self.store, move |store| {
            if let Some(m) = crate::api::refuse_if_mirror(store, doc_id, "editing") {
                return err(m);
            }
            let tree = match store.read_doc(doc_id) {
                Ok(t) => t,
                Err(e) => return err(e.to_string()),
            };
            let epoch = tree.doc.current_epoch;
            let (export, _) = locate::export_with_offsets(&tree.roots);
            let landing = match resolve_landing(
                store,
                doc_id,
                &tree.roots,
                &export,
                p.to.as_deref(),
                at_start,
                p.create_missing.unwrap_or(false),
            ) {
                Ok(l) => l,
                Err(m) => return err(m),
            };
            let mut text = landing.new_headings.join("\n\n");
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(p.markdown.trim_matches('\n'));
            let new_md = locate::splice_insert(&export, landing.offset, &text);
            let ops = grimoire_store::mddiff::markdown_to_ops_from(&tree.roots, &new_md, "append");
            if ops.is_empty() {
                return no_changes(epoch);
            }
            let res = store.propose(doc_id, epoch, principal, ops.clone());
            finish(&dedupe, principal, key, &ops, res, epoch, verbose)
        })
        .await
    }

    pub(crate) async fn propose_impl(&self, hint: RequestHint, p: ProposeParams) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let ops: Vec<OpInput> = match serde_json::from_value(p.ops.clone()) {
            Ok(o) => o,
            Err(e) => return err(format!("ops did not parse: {e}")),
        };
        let verbose = p.verbose.unwrap_or(false);
        let principal = match self.acting(p.as_.as_deref(), &hint).await {
            Ok(id) => id,
            Err(m) => return err(m),
        };
        let key = dedupe_key("propose", Some(doc_id), &json!({"base_epoch": p.base_epoch, "ops": p.ops}));
        if let Some(prev) = dedupe_get(&self.dedupe, principal, key) {
            return replay(&prev, verbose);
        }
        if let Err(m) = self.hot.assert_cold(doc_id) {
            return err(m);
        }
        let dedupe = self.dedupe.clone();
        with_store(&self.store, move |store| {
            // block ops against a stale base are SCORED per op (the gate's whole
            // point: unchanged targets still green, conflicts yellow/red) — a
            // stale base is not an error here; see propose_markdown for the
            // whole-doc path, where it is.
            let before = store.get_doc(doc_id).map(|d| d.current_epoch).unwrap_or(p.base_epoch);
            let res = store.propose(doc_id, p.base_epoch, principal, ops.clone());
            finish(&dedupe, principal, key, &ops, res, before, verbose)
        })
        .await
    }

    pub(crate) async fn propose_markdown_impl(&self, hint: RequestHint, p: ProposeMarkdownParams) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let verbose = p.verbose.unwrap_or(false);
        let principal = match self.acting(p.as_.as_deref(), &hint).await {
            Ok(id) => id,
            Err(m) => return err(m),
        };
        let key = dedupe_key("propose_markdown", Some(doc_id), &json!({"base_epoch": p.base_epoch, "markdown": p.markdown}));
        if let Some(prev) = dedupe_get(&self.dedupe, principal, key) {
            return replay(&prev, verbose);
        }
        if let Err(m) = self.hot.assert_cold(doc_id) {
            return err(m);
        }
        let dedupe = self.dedupe.clone();
        with_store(&self.store, move |store| {
            let tree = match store.read_doc(doc_id) {
                Ok(t) => t,
                Err(e) => return err(e.to_string()),
            };
            // Whole-doc semantics: the markdown was written against base_epoch,
            // but the diff can only be taken against the CURRENT blocks. If the
            // doc moved on, that diff would silently re-apply the agent's stale
            // view over others' edits (scored red, parked, invisible to the
            // caller). So a stale base is an error here, with the missed ops
            // attached — re-read, re-apply, re-send.
            if p.base_epoch != tree.doc.current_epoch {
                let missed = store.ops_since(doc_id, p.base_epoch).unwrap_or_default();
                let summary: Vec<String> = missed
                    .iter()
                    .map(|o| {
                        format!(
                            "{} {}",
                            o.kind.op_type(),
                            o.kind.target_block().map(short_ref).unwrap_or_else(|| "doc".into())
                        )
                    })
                    .collect();
                if verbose {
                    return ok_json(&json!({
                        "error": "stale_base",
                        "base_epoch": p.base_epoch,
                        "current_epoch": tree.doc.current_epoch,
                        "missed_ops": missed,
                        "recover": "re-read the doc, re-apply your edit to the fresh markdown, re-send with the current epoch",
                    }));
                }
                return err(format!(
                    "stale_base: doc is at epoch {} (you read {}); missed {} op(s): {} — read_doc again and re-send, or use edit_doc",
                    tree.doc.current_epoch,
                    p.base_epoch,
                    missed.len(),
                    summary.join(", ")
                ));
            }
            let ops = grimoire_store::mddiff::markdown_to_ops(&tree.roots, &p.markdown);
            if ops.is_empty() {
                return no_changes(tree.doc.current_epoch);
            }
            let res = store.propose(doc_id, p.base_epoch, principal, ops.clone());
            finish(&dedupe, principal, key, &ops, res, tree.doc.current_epoch, verbose)
        })
        .await
    }

    pub(crate) async fn create_doc_impl(&self, hint: RequestHint, p: CreateDocParams) -> Result<CallToolResult, McpError> {
        let parent = match parse_opt_uuid(p.parent_doc_id.as_deref(), "parent_doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let reuse = match p.if_exists.as_deref().unwrap_or("error") {
            "reuse" => true,
            "error" => false,
            other => return err(format!("if_exists must be reuse | error, got {other}")),
        };
        let title = p.title.trim().to_string();
        if title.is_empty() {
            return err("title must not be empty".into());
        }
        let verbose = p.verbose.unwrap_or(false);
        let principal = match self.acting(p.as_.as_deref(), &hint).await {
            Ok(id) => id,
            Err(m) => return err(m),
        };
        let key = dedupe_key("create_doc", parent, &json!({"title": title, "markdown": p.markdown, "reuse": reuse}));
        if let Some(prev) = dedupe_get(&self.dedupe, principal, key) {
            return replay(&prev, verbose);
        }
        let dedupe = self.dedupe.clone();
        with_store(&self.store, move |store| {
            // one lock for the whole find-or-create: two sessions racing on
            // the same daily title cannot both create
            let existing = match store.list_docs() {
                Ok(docs) => docs.into_iter().find(|d| d.parent_id == parent && d.title == title),
                Err(e) => return err(e.to_string()),
            };
            let render = |d: &grimoire_store::Doc, reused: bool| {
                let full = json!({"id": d.id, "title": d.title, "parent_id": d.parent_id, "epoch": d.current_epoch, "reused": reused});
                let text = format!(
                    "ok · {} “{}” · doc {} · epoch {}",
                    if reused { "reused" } else { "created" },
                    d.title,
                    d.id,
                    d.current_epoch
                );
                (text, full)
            };
            if let Some(d) = existing {
                if reuse {
                    let (text, full) = render(&d, true);
                    return if verbose { ok_json(&full) } else { ok_text(text) };
                }
                return err(format!(
                    "a doc titled {title:?} already exists here ({}); pass if_exists: \"reuse\" to use it",
                    d.id
                ));
            }
            let ops = p
                .markdown
                .as_deref()
                .filter(|m| !m.trim().is_empty())
                .map(|m| grimoire_store::import::to_ops(grimoire_store::import::segment(m)))
                .unwrap_or_default();
            match store.create_doc_with_ops(&title, parent, principal, ops) {
                Ok((d, _)) => {
                    let (text, full) = render(&d, false);
                    dedupe_put(&dedupe, principal, key, stored(&text, &full));
                    if verbose { ok_json(&full) } else { ok_text(text) }
                }
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    pub(crate) async fn doc_op_impl(&self, hint: RequestHint, p: DocOpParams) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let op = p.op.trim().to_lowercase();
        if !matches!(op.as_str(), "rename" | "move" | "status" | "delete" | "merge") {
            return err(format!("op must be rename | move | status | delete | merge, got {}", p.op));
        }
        let verbose = p.verbose.unwrap_or(false);
        let principal = match self.acting(p.as_.as_deref(), &hint).await {
            Ok(id) => id,
            Err(m) => return err(m),
        };
        let payload = json!({
            "op": op, "title": p.title, "new_parent_id": p.new_parent_id, "after_doc_id": p.after_doc_id,
            "status": p.status, "into_doc_id": p.into_doc_id,
        });
        let key = dedupe_key("doc_op", Some(doc_id), &payload);
        if let Some(prev) = dedupe_get(&self.dedupe, principal, key) {
            return replay(&prev, verbose);
        }
        let hot = self.hot.clone();
        let dedupe = self.dedupe.clone();
        with_store(&self.store, move |store| {
            let is_hot = |d: Uuid| hot.is_hot(d);
            let before = store.get_doc(doc_id).map(|d| d.current_epoch).unwrap_or(0);
            let single = |ops_kind: &str, res: Result<ProposeOutcome, String>| match res {
                Ok(out) => {
                    // doc ops are one ledger row; label it by the op name
                    let fake = vec![OpInput {
                        kind: match ops_kind {
                            "rename" => OpKind::RenameDoc { title: String::new(), from_title: String::new() },
                            "move" => OpKind::MoveDoc {
                                new_parent: None,
                                sort_key: None,
                                new_parent_title: None,
                                from_parent: None,
                                from_sort_key: None,
                                from_parent_title: None,
                            },
                            "status" => OpKind::SetStatus { status: None, from_status: None },
                            _ => OpKind::DeleteDoc { title: String::new(), doc_count: 0 },
                        },
                        source_refs: vec![],
                    }];
                    let text = render_outcome(&fake, &out, before);
                    dedupe_put(&dedupe, principal, key, stored(&text, &serde_json::to_value(&out).unwrap_or_default()));
                    if verbose { ok_json(&out) } else { ok_text(text) }
                }
                Err(m) => err(m),
            };
            match op.as_str() {
                "rename" => {
                    let Some(title) = p.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
                        return err("rename needs title".into());
                    };
                    single("rename", crate::docops::rename(store, doc_id, title, principal))
                }
                "move" => {
                    let new_parent = match parse_opt_uuid(p.new_parent_id.as_deref(), "new_parent_id") {
                        Ok(u) => u,
                        Err(m) => return err(m),
                    };
                    let after = match parse_opt_uuid(p.after_doc_id.as_deref(), "after_doc_id") {
                        Ok(u) => u,
                        Err(m) => return err(m),
                    };
                    single("move", crate::docops::move_doc(store, doc_id, new_parent, after, principal))
                }
                "status" => {
                    let status = match crate::docops::parse_status(p.status.as_deref()) {
                        Ok(s) => s,
                        Err(m) => return err(m),
                    };
                    single("status", crate::docops::set_status(store, doc_id, status, principal))
                }
                "delete" => single("delete", crate::docops::delete(store, &is_hot, doc_id, principal)),
                _ => {
                    let into = match p.into_doc_id.as_deref().map(|s| parse_uuid(s, "into_doc_id")) {
                        Some(Ok(u)) => u,
                        Some(Err(m)) => return err(m),
                        None => return err("merge needs into_doc_id".into()),
                    };
                    match crate::docops::merge(store, &is_hot, doc_id, into, principal) {
                        Ok(out) => {
                            let appended = out["into"]["verdicts"].as_array().map(Vec::len).unwrap_or(0);
                            let text = format!(
                                "ok · merge · {appended} appended (yellow, flagged) · delete parked red — {}",
                                out["note"].as_str().unwrap_or("")
                            );
                            dedupe_put(&dedupe, principal, key, stored(&text, &out));
                            if verbose { ok_json(&out) } else { ok_text(text) }
                        }
                        Err(m) => err(m),
                    }
                }
            }
        })
        .await
    }

    pub(crate) async fn add_comment_impl(&self, hint: RequestHint, p: AddCommentParams) -> Result<CallToolResult, McpError> {
        let verbose = p.verbose.unwrap_or(false);
        let principal = match self.acting(p.as_.as_deref(), &hint).await {
            Ok(id) => id,
            Err(m) => return err(m),
        };
        let key = dedupe_key("add_comment", None, &json!({"block_id": p.block_id, "text": p.text, "reply_to": p.reply_to}));
        if let Some(prev) = dedupe_get(&self.dedupe, principal, key) {
            return replay(&prev, verbose);
        }
        let dedupe = self.dedupe.clone();
        with_store(&self.store, move |store| {
            let target = match resolve_block(store, &p.block_id) {
                Ok(b) => b,
                Err(m) => return err(m),
            };
            let reply_to = match p.reply_to.as_deref().map(|r| resolve_block(store, r)) {
                Some(Ok(b)) => Some(b.id),
                Some(Err(m)) => return err(m),
                None => None,
            };
            match store.add_comment(target.id, principal, &p.text, reply_to) {
                Ok(c) => {
                    let text = format!("ok · comment {} on {} · epoch {}", short_ref(c.id), short_ref(target.id), c.epoch);
                    dedupe_put(&dedupe, principal, key, stored(&text, &json!(c)));
                    if verbose { ok_json(&c) } else { ok_text(text) }
                }
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    pub(crate) async fn resolve_impl(&self, hint: RequestHint, p: ResolveParams) -> Result<CallToolResult, McpError> {
        let id = match parse_uuid(&p.annotation_id, "annotation_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let decision = match p.decision.trim() {
            "accept" => ReviewDecision::Accept,
            "decline" => ReviewDecision::Decline,
            other => return err(format!("decision must be accept|decline, got {other}")),
        };
        let verbose = p.verbose.unwrap_or(false);
        let principal = match self.acting(p.as_.as_deref(), &hint).await {
            Ok(id) => id,
            Err(m) => return err(m),
        };
        let hot = self.hot.clone();
        with_store(&self.store, move |store| {
            if let Some(doc) = crate::hot::annotation_doc(store, id)
                && let Err(m) = hot.assert_cold(doc)
            {
                return err(m);
            }
            match store.resolve(id, principal, decision) {
                Ok(receipt) => {
                    if verbose {
                        return ok_json(&json!({ "resolved": true, "receipt": receipt }));
                    }
                    let word = if decision == ReviewDecision::Accept { "accepted" } else { "declined" };
                    match receipt {
                        Some(r) => ok_text(format!("ok · {word} · epoch {}", r.epoch)),
                        None => ok_text(format!("ok · {word}")),
                    }
                }
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    pub(crate) async fn proposals_impl(&self, hint: RequestHint, p: ProposalsParams) -> Result<CallToolResult, McpError> {
        let kind = p.kind.clone().unwrap_or_else(|| "mine".into());
        let verbose = p.verbose.unwrap_or(false);
        match kind.as_str() {
            "mine" => {
                let principal = match self.acting(p.as_.as_deref(), &hint).await {
                    Ok(id) => id,
                    Err(m) => return err(m),
                };
                let limit = p.limit.unwrap_or(20).max(1) as usize;
                with_store(&self.store, move |store| match store.proposal_outcomes(principal, limit) {
                    Ok(rows) => ok_json(&json!({
                        "principal": principal,
                        "proposals": rows
                            .into_iter()
                            .map(|(op, status, resolver)| {
                                let mut op = json!(op);
                                if !verbose {
                                    // the outcome is the point: what was proposed, the
                                    // verdict, who resolved it. The pre-image's
                                    // provenance fields and the op's own bookkeeping
                                    // are noise here (verbose: true keeps them).
                                    compact_prior(&mut op);
                                    if let Some(o) = op.as_object_mut() {
                                        o.remove("principal");
                                        o.remove("base_epoch");
                                        if let Some(k) = o.get_mut("kind").and_then(Value::as_object_mut) {
                                            k.remove("refers_to");
                                        }
                                    }
                                }
                                json!({"op": op, "review_status": status, "resolved_by": resolver})
                            })
                            .collect::<Vec<_>>(),
                    })),
                    Err(e) => err(e.to_string()),
                })
                .await
            }
            "pending" => {
                let doc_id = match parse_opt_uuid(p.doc_id.as_deref(), "doc_id") {
                    Ok(u) => u,
                    Err(m) => return err(m),
                };
                let limit = p.limit.unwrap_or(50).max(1) as usize;
                with_store(&self.store, move |store| match store.review_queue(doc_id) {
                    Ok(q) => {
                        let total = q.len();
                        let mut items: Vec<Value> = q.iter().take(limit).map(|e| json!(e)).collect();
                        if !verbose {
                            // the queue is read to decide, not to reconstruct: the
                            // pre-image's provenance fields are noise on every entry
                            for item in &mut items {
                                if let Some(op) = item.get_mut("op") {
                                    compact_prior(op);
                                }
                            }
                        }
                        ok_json(&json!({"total": total, "shown": items.len(), "items": items}))
                    }
                    Err(e) => err(e.to_string()),
                })
                .await
            }
            other => err(format!("kind must be mine | pending, got {other}")),
        }
    }
}

/// Trim an op's `prior` block to {id, block_type, content}.
fn compact_prior(op: &mut Value) {
    if let Some(prior) = op.get_mut("prior")
        && prior.is_object()
    {
        *prior = json!({
            "id": prior.get("id").cloned().unwrap_or(Value::Null),
            "block_type": prior.get("block_type").cloned().unwrap_or(Value::Null),
            "content": prior.get("content").cloned().unwrap_or(Value::Null),
        });
    }
}

fn tree_first_line(roots: &[BlockNode], id: Option<Uuid>) -> String {
    id.and_then(|id| locate::find_node(roots, id))
        .map(|n| locate::first_line(&n.block.content, 80))
        .unwrap_or_default()
}

#[tool_handler]
impl ServerHandler for KsMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Grimoire: docs as block trees behind a review gate. Every write answers with \
             one line: green = applied, yellow = applied + flagged for a human, red = \
             parked until a human accepts.\n\
             Who you are: pass as: 'claude:<project>-<task>' on writes, or configure it \
             once — /mcp?as=<name>, /mcp?cwd=<dir> (→ claude:<dirname>) or the \
             X-Grimoire-Principal header. No sessions: nothing else survives a call.\n\
             The loop: find_doc(query) → optionally read_doc(doc_id, section: 'grimoire › \
             Plans') → edit_doc(doc_id, old, new) or append(doc_id, markdown, to: \
             'heading path', create_missing: true for a daily log) → read the verdict. \
             No epochs needed on those two. Refs: ^abc123 = a block (read_doc refs: true \
             shows them; add_comment and section/to accept them). Heading paths use › or /.\n\
             Full rewrite: propose_markdown(doc_id, epoch from read_doc, markdown). \
             Surgical block ops: propose. Tree: doc_op(op: rename|move|status|delete|merge) \
             — yellows, delete always red. create_doc(if_exists: 'reuse') is find-or-create.\n\
             proposals(kind: 'mine') shows what happened to yours; 'pending' what awaits a \
             human. Human-only, never here: review policy, shares, trust, gardeners.",
        )
    }
}

/// Largest `/mcp` request body, matching `propose_markdown` on the HTTP API:
/// a whole doc's markdown can be megabytes. rmcp's own default is 4 MB, which
/// silently truncated a big `propose_markdown` long before the axum layer.
pub const MAX_MCP_BODY: usize = 16 * 1024 * 1024;

/// `embedder`: the block embedder for the dense legs of `search`/`related`
/// (None = keyword-only, the tools say so).
pub fn router(
    store: Arc<Mutex<SqliteStore>>,
    agent: Uuid,
    hot: crate::hot::HotState,
    dedupe: DedupeCache,
    embedder: Option<Arc<crate::embed::Embedder>>,
) -> axum::Router {
    // one `as` cache for every request: rmcp builds a fresh KsMcp per request
    // on stateless protocol versions, so anything cross-call lives out here
    let names = new_name_cache();
    let service = StreamableHttpService::new(
        move || {
            Ok(KsMcp::new(store.clone(), agent, dedupe.clone(), names.clone(), hot.clone())
                .with_embedder(embedder.clone()))
        },
        LocalSessionManager::default().into(),
        rmcp::transport::streamable_http_server::tower::StreamableHttpServerConfig::default()
            .with_max_request_body_bytes(MAX_MCP_BODY),
    );
    // rmcp reads the body itself, so neither axum's DefaultBodyLimit nor a
    // tower-http layer gets a say: rmcp's own cap is the one that applies and
    // it enforces it while streaming, answering 413. A tower-http layer here
    // only turned that 413 into a 500 on a body with no Content-Length.
    // The per-request principal hints (header, ?as=, ?cwd=) need no layer
    // either: rmcp hands the request's `Parts` to every tool via the request
    // context (`RequestHint::from_ctx`).
    axum::Router::new().nest_service("/mcp", service)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::import::import_markdown;

    pub const TOOLS: [&str; 16] = [
        "find_doc",
        "orient",
        "read_doc",
        "edit_doc",
        "append",
        "propose_markdown",
        "propose",
        "diff_since",
        "search",
        "grep",
        "related",
        "create_doc",
        "doc_op",
        "add_comment",
        "resolve",
        "proposals",
    ];

    #[test]
    fn exactly_sixteen_tools_and_no_request_id() {
        let tools = KsMcp::tool_router().list_all();
        let mut names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        names.sort();
        let mut want: Vec<String> = TOOLS.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(names, want);
        assert_eq!(tools.len(), 16);
        for t in &tools {
            let schema = serde_json::to_string(&t.input_schema).unwrap();
            assert!(!schema.contains("request_id"), "{}: {schema}", t.name);
            let desc = t.description.as_deref().unwrap_or("");
            assert!(desc.split(". ").count() <= 3, "≤3 sentences — {}: {desc}", t.name);
        }
    }

    #[test]
    fn dedupe_evicts_the_oldest_half_not_everything() {
        let cache = new_dedupe();
        let p = Uuid::now_v7();
        let ids: Vec<Uuid> = (0..DEDUPE_CAPACITY).map(|_| Uuid::now_v7()).collect();
        for (i, id) in ids.iter().enumerate() {
            dedupe_put(&cache, p, *id, json!(i));
        }
        let extra = Uuid::now_v7();
        dedupe_put(&cache, p, extra, json!("new"));
        let n = cache.lock().unwrap().len();
        assert_eq!(n, DEDUPE_CAPACITY / 2 + 1);
        assert!(dedupe_get(&cache, p, ids[0]).is_none(), "oldest evicted");
        assert_eq!(dedupe_get(&cache, p, ids[DEDUPE_CAPACITY - 1]), Some(json!(DEDUPE_CAPACITY - 1)), "newest kept");
        assert_eq!(dedupe_get(&cache, p, extra), Some(json!("new")));
    }

    #[test]
    fn dedupe_key_is_canonical_and_entries_expire() {
        let d = Uuid::now_v7();
        let a = dedupe_key("edit_doc", Some(d), &json!({"old": "x", "new": "y"}));
        let b = dedupe_key("edit_doc", Some(d), &json!({"new": "y", "old": "x"}));
        assert_eq!(a, b, "key order does not matter");
        assert_ne!(a, dedupe_key("append", Some(d), &json!({"old": "x", "new": "y"})), "tool matters");
        assert_ne!(a, dedupe_key("edit_doc", Some(Uuid::now_v7()), &json!({"old": "x", "new": "y"})), "doc matters");
        assert_ne!(a, dedupe_key("edit_doc", Some(d), &json!({"old": "x", "new": "z"})), "payload matters");

        let cache = new_dedupe();
        let p = Uuid::now_v7();
        dedupe_put(&cache, p, a, json!("once"));
        assert_eq!(dedupe_get(&cache, p, a), Some(json!("once")));
        assert!(dedupe_get(&cache, Uuid::now_v7(), a).is_none(), "keyed by principal");
        // age the entry past the TTL
        cache.lock().unwrap().get_mut(&(p, a)).unwrap().1 = Instant::now() - DEDUPE_TTL - Duration::from_secs(1);
        assert!(dedupe_get(&cache, p, a).is_none(), "expired");
    }

    #[test]
    fn principal_names_are_bounded_and_printable() {
        assert_eq!(valid_principal_name("  claude:proj-task "), Ok("claude:proj-task"));
        assert!(valid_principal_name("").is_err());
        assert!(valid_principal_name("   ").is_err());
        assert!(valid_principal_name("a\u{7}b").is_err());
        assert!(valid_principal_name("line\nbreak").is_err());
        assert!(valid_principal_name(&"x".repeat(64)).is_ok());
        assert!(valid_principal_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn cwd_derives_a_project_principal() {
        assert_eq!(principal_for_cwd("/Users/me/src/qompass"), Some("claude:qompass".into()));
        assert_eq!(principal_for_cwd("/Users/me/src/qompass/"), Some("claude:qompass".into()));
        assert_eq!(principal_for_cwd("C:\\work\\portus"), Some("claude:portus".into()));
        assert_eq!(principal_for_cwd(" my project "), Some("claude:my project".into()));
        assert_eq!(principal_for_cwd("/"), None);
        assert_eq!(principal_for_cwd(""), None);
        assert_eq!(principal_for_cwd("."), None);
        assert_eq!(principal_for_cwd(&format!("/x/{}", "y".repeat(80))), None, "too long → shared default");
        assert_eq!(principal_for_cwd("/x/bad\u{7}name"), None);
    }

    fn parts(uri: &str, headers: &[(&str, &str)]) -> axum::http::request::Parts {
        let mut b = axum::http::Request::builder().uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(()).unwrap().into_parts().0
    }

    #[test]
    fn request_hint_reads_header_and_query_with_precedence() {
        let h = RequestHint::from_parts(&parts("/mcp?cwd=%2FUsers%2Fme%2Fgrimoire&as=claude%3Aexplicit", &[]));
        assert_eq!(h.query_as.as_deref(), Some("claude:explicit"));
        assert_eq!(h.cwd.as_deref(), Some("/Users/me/grimoire"));
        assert_eq!(h.default_name().as_deref(), Some("claude:explicit"), "?as beats ?cwd");

        let h = RequestHint::from_parts(&parts("/mcp?cwd=/Users/me/grimoire", &[]));
        assert_eq!(h.default_name().as_deref(), Some("claude:grimoire"));

        let h = RequestHint::from_parts(&parts("/mcp?as=claude:q", &[("X-Grimoire-Principal", "claude:hdr")]));
        assert_eq!(h.default_name().as_deref(), Some("claude:hdr"), "header beats query");

        let h = RequestHint::from_parts(&parts("/mcp", &[]));
        assert_eq!(h, RequestHint::default());
        assert_eq!(h.default_name(), None);
        let h = RequestHint::from_parts(&parts("/mcp?cwd=/&as=", &[]));
        assert_eq!(h.default_name(), None, "empty values fall through to the shared default");
        assert_eq!(percent_decode("a+b%20c%2"), "a b c%2");
    }

    /// The find-or-create-by-name surface is unauthenticated, so it is capped
    /// per boot. At the cap, a NEW name is refused while EXISTING names still
    /// resolve — an agent that already identified never loses its principal.
    #[test]
    fn auto_created_principals_are_capped_per_boot() {
        let _serial = AUTO_CREATED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut store = SqliteStore::open_in_memory().unwrap();
        let previous = swap_auto_created_for_test(0);
        let known = agent_principal_by_name(&mut store, "claude:known").unwrap();

        swap_auto_created_for_test(MAX_AUTO_PRINCIPALS_PER_BOOT);
        let err = agent_principal_by_name(&mut store, "claude:brand-new").unwrap_err();
        assert!(err.contains("too many new agent principals"), "{err}");
        assert_eq!(
            agent_principal_by_name(&mut store, "claude:known").unwrap(),
            known,
            "an existing name still resolves at the cap"
        );
        // one under the cap lets exactly one more through
        swap_auto_created_for_test(MAX_AUTO_PRINCIPALS_PER_BOOT - 1);
        assert!(agent_principal_by_name(&mut store, "claude:last-one").is_ok());
        assert!(agent_principal_by_name(&mut store, "claude:one-too-many").is_err());

        swap_auto_created_for_test(previous);
    }

    /// rmcp reads the request body itself, so axum's `DefaultBodyLimit` never
    /// fires on `/mcp`: the 16 MB cap is rmcp's own. Over it the request is
    /// rejected before the service sees it.
    #[tokio::test]
    async fn mcp_body_over_16mb_is_rejected() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        const LIMIT: usize = MAX_MCP_BODY;

        let mut store = SqliteStore::open_in_memory().unwrap();
        let agent = store
            .create_principal(grimoire_store::PrincipalKind::Agent, "claude", None)
            .unwrap()
            .id;
        let app = router(Arc::new(Mutex::new(store)), agent, test_hot("body"), new_dedupe(), None);

        let send = |body: Vec<u8>| {
            let app = app.clone();
            async move {
                app.oneshot(
                    Request::post("/mcp")
                        .header("host", "127.0.0.1:7425")
                        .header("content-type", "application/json")
                        .header("accept", "application/json, text/event-stream")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };

        // a well-formed initialize just under the cap: padded in an unused
        // field so the body is huge but the JSON still parses
        let pad = "x".repeat(LIMIT - 4096);
        let under = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"{pad}","version":"1"}}}}}}"#
        );
        assert!(under.len() < LIMIT, "under the cap: {}", under.len());
        assert_eq!(send(under.into_bytes()).await, StatusCode::OK);

        let pad = "x".repeat(LIMIT + 1024);
        let over = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"{pad}","version":"1"}}}}}}"#
        );
        assert!(over.len() > LIMIT);
        assert_eq!(send(over.into_bytes()).await, StatusCode::PAYLOAD_TOO_LARGE);
    }

    fn text_of(r: CallToolResult) -> (bool, Value) {
        let is_err = r.is_error.unwrap_or(false);
        let text = r.content[0].as_text().map(|t| t.text.clone()).unwrap_or_default();
        (is_err, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    fn raw(r: CallToolResult) -> (bool, String) {
        let is_err = r.is_error.unwrap_or(false);
        (is_err, r.content[0].as_text().map(|t| t.text.clone()).unwrap_or_default())
    }

    fn test_hot(tag: &str) -> crate::hot::HotState {
        crate::hot::HotState::new(std::env::temp_dir().join(format!("grimoire-mcp-{tag}-{}", Uuid::now_v7())))
    }

    fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
        serde_json::from_value(v).unwrap()
    }

    const NONE: RequestHint = RequestHint { header: None, query_as: None, cwd: None };

    /// The AX tools through the tool fns: compact hits, validated `kind`,
    /// a clear regex error, `related` naming the missing embedder and
    /// serving tags.
    #[tokio::test]
    async fn ax_tools_return_compact_shapes_and_clear_errors() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let tom = store.create_principal(grimoire_store::PrincipalKind::Human, "tom", None).unwrap().id;
        let agent = store.create_principal(grimoire_store::PrincipalKind::Agent, "claude", None).unwrap().id;
        let (shell, _) = import_markdown(&mut store, "Shell", None, tom, "---\ntags:\n  - ui\n---\n\nDrag the window by its title bar.\n").unwrap();
        import_markdown(&mut store, "Entitlements", None, tom, "The entitlement check runs at login.\n").unwrap();
        let mcp = KsMcp::new(Arc::new(Mutex::new(store)), agent, new_dedupe(), new_name_cache(), test_hot("ax"));

        let (is_err, hits) = text_of(mcp.search(Parameters(p(json!({"query": "title bar"})))).await.unwrap());
        assert!(!is_err);
        assert_eq!(hits[0]["doc_title"], "Shell");
        assert_eq!(hits[0]["path"], "Shell");
        assert!(hits[0]["snippet"].as_str().unwrap().contains("title bar"));
        assert!(hits[0].get("block_id").is_some() && hits[0].get("score").is_some());
        assert!(hits[0].get("block").is_none(), "compact: no full block");

        let (is_err, docs) = text_of(mcp.search(Parameters(p(json!({"query": "title bar", "kind": "docs"})))).await.unwrap());
        assert!(!is_err);
        assert_eq!(docs[0]["hits"], 1);
        let (is_err, msg) = text_of(mcp.search(Parameters(p(json!({"query": "x", "kind": "pages"})))).await.unwrap());
        assert!(is_err && msg.as_str().unwrap().contains("kind must be"));

        let (is_err, msg) = text_of(mcp.grep(Parameters(p(json!({"pattern": "(oops"})))).await.unwrap());
        assert!(is_err && msg.as_str().unwrap().starts_with("invalid regex:"), "{msg}");
        let (_, out) = text_of(mcp.grep(Parameters(p(json!({"pattern": "title|entitlement"})))).await.unwrap());
        assert_eq!(out["total_groups"], 2);
        assert_eq!(out["truncated"], false);

        let (is_err, out) = text_of(mcp.related(Parameters(p(json!({"doc_id": shell.to_string()})))).await.unwrap());
        assert!(!is_err);
        assert_eq!(out["embedder"], false);
        assert!(out["note"].as_str().unwrap().contains("similar"));
        assert!(out["related"].as_array().unwrap().iter().any(|r| r["why"] == "sibling" && r["title"] == "Entitlements"));
        let (is_err, msg) = text_of(mcp.related(Parameters(p(json!({})))).await.unwrap());
        assert!(is_err && msg.as_str().unwrap().contains("block_id or doc_id"));
        // tags absorbed into related
        let (is_err, tags) = text_of(mcp.related(Parameters(p(json!({"tags": true})))).await.unwrap());
        assert!(!is_err);
        assert_eq!(tags[0]["tag"], "ui");
        assert_eq!(tags[0]["docs"], 1);
        let (is_err, by) = text_of(mcp.related(Parameters(p(json!({"by": "tag", "tag": "ui"})))).await.unwrap());
        assert!(!is_err, "{by}");
        assert_eq!(by[0]["title"], "Shell");
        let (is_err, _) = text_of(mcp.related(Parameters(p(json!({"by": "tag"})))).await.unwrap());
        assert!(is_err);
        // a block ref anchors related too
        let bid = Uuid::parse_str(hits[0]["block_id"].as_str().unwrap()).unwrap();
        let (is_err, out) = text_of(mcp.related(Parameters(p(json!({"block_id": short_ref(bid)})))).await.unwrap());
        assert!(!is_err, "{out}");

        let (is_err, map) = text_of(mcp.orient(Parameters(p(json!({})))).await.unwrap());
        assert!(!is_err);
        let map = map.as_str().unwrap();
        assert!(map.starts_with("# Corpus — 2 docs") && map.contains("- Shell · "), "{map}");
        let (_, deep) = text_of(mcp.orient(Parameters(p(json!({"depth": 5})))).await.unwrap());
        assert!(deep.as_str().unwrap().contains("- Shell · "));
    }

    /// `as` beats the request hint (header > ?as > ?cwd), which beats the
    /// shared default; both forms of `as` resolve (a name finds-or-creates, a
    /// UUID must exist); the human and remote principals are refused by name
    /// and by id.
    #[test]
    fn acting_principal_precedence_forms_and_refusals() {
        let _serial = AUTO_CREATED_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = swap_auto_created_for_test(0);
        let mut store = SqliteStore::open_in_memory().unwrap();
        let tom = store.create_principal(grimoire_store::PrincipalKind::Human, "tom", None).unwrap().id;
        let peer = store.create_principal(grimoire_store::PrincipalKind::Remote, "laptop", None).unwrap().id;
        let claude = store.create_principal(grimoire_store::PrincipalKind::Agent, "claude", None).unwrap().id;
        let session = store.create_principal(grimoire_store::PrincipalKind::Agent, "claude:session", None).unwrap().id;
        let names = new_name_cache();
        let act = |store: &mut SqliteStore, as_: Option<&str>, hint: &RequestHint| {
            acting_principal(store, &names, as_, hint, claude)
        };
        let hint = |header: Option<&str>, q: Option<&str>, cwd: Option<&str>| RequestHint {
            header: header.map(String::from),
            query_as: q.map(String::from),
            cwd: cwd.map(String::from),
        };

        // precedence
        assert_eq!(act(&mut store, None, &NONE), Ok(claude), "default");
        assert_eq!(act(&mut store, None, &hint(None, Some("claude:session"), None)), Ok(session), "?as beats default");
        let by_cwd = act(&mut store, None, &hint(None, None, Some("/Users/me/portus"))).unwrap();
        assert_eq!(agent_principal_by_name(&mut store, "claude:portus").unwrap(), by_cwd, "?cwd derives claude:portus");
        assert_eq!(act(&mut store, None, &hint(None, Some("claude:session"), Some("/x/portus"))), Ok(session), "?as beats ?cwd");
        let by_hdr = act(&mut store, None, &hint(Some("claude:hdr"), Some("claude:session"), None)).unwrap();
        assert_ne!(by_hdr, session, "header beats ?as");
        let named = act(&mut store, Some("claude:proj-task"), &hint(Some("claude:hdr"), None, None)).unwrap();
        assert_ne!(named, by_hdr, "as beats header");
        assert_eq!(agent_principal_by_name(&mut store, "claude:proj-task").unwrap(), named, "name form created it");
        assert_eq!(act(&mut store, Some(" claude:proj-task "), &NONE), Ok(named), "trimmed, idempotent");
        // UUID form
        assert_eq!(act(&mut store, Some(&session.to_string()), &NONE), Ok(session));
        let e = act(&mut store, Some(&Uuid::now_v7().to_string()), &NONE).unwrap_err();
        assert!(e.contains("no principal with id"), "{e}");
        // never the human or a remote peer — by argument or by header
        for (label, as_) in [("human name", "tom".to_string()), ("human id", tom.to_string()), ("remote name", "laptop".into()), ("remote id", peer.to_string())] {
            let e = act(&mut store, Some(&as_), &NONE).unwrap_err();
            assert!(e.contains("not an agent"), "{label}: {e}");
        }
        let e = act(&mut store, None, &hint(Some("tom"), None, None)).unwrap_err();
        assert!(e.contains("not an agent"), "header: {e}");
        // an invalid name is an error, not a fallback to the default
        assert!(act(&mut store, Some(""), &hint(None, Some("claude:session"), None)).is_err());
        assert!(act(&mut store, Some("line\nbreak"), &NONE).is_err());
        // the human is not cached by mistake
        assert!(cached_principal(&names, "tom").is_none());
        swap_auto_created_for_test(previous);
    }

    /// A cache hit never touches the store: a key seeded in the cache resolves
    /// to its id even though no such principal exists in this store, and a
    /// resolved name lands in the cache for the next request.
    #[test]
    fn acting_principal_cache_hit_skips_the_store() {
        let _serial = AUTO_CREATED_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = swap_auto_created_for_test(0);
        let mut store = SqliteStore::open_in_memory().unwrap();
        let claude = store.create_principal(grimoire_store::PrincipalKind::Agent, "claude", None).unwrap().id;
        let names = new_name_cache();
        let ghost = Uuid::now_v7();
        names.lock().unwrap().insert("claude:elsewhere".into(), ghost);
        assert_eq!(acting_principal(&mut store, &names, Some("claude:elsewhere"), &NONE, claude), Ok(ghost));
        assert!(store.get_principal(ghost).is_err(), "the store never saw it");

        let fresh = acting_principal(&mut store, &names, Some("claude:fresh"), &NONE, claude).unwrap();
        assert_eq!(cached_principal(&names, "claude:fresh"), Some(fresh));
        assert_eq!(cached_principal(&names, &fresh.to_string()), None, "cached under the string given");
        // at the creation cap the cached name still resolves (no store scan, no create)
        swap_auto_created_for_test(MAX_AUTO_PRINCIPALS_PER_BOOT);
        assert_eq!(acting_principal(&mut store, &names, Some("claude:fresh"), &NONE, claude), Ok(fresh));
        swap_auto_created_for_test(previous);
    }

    struct Fx {
        store: Arc<Mutex<SqliteStore>>,
        mcp: KsMcp,
        doc: Uuid,
        tom: Uuid,
        claude: Uuid,
    }

    const DAILY: &str = "---\ntags:\n  - daily\n---\n\n## qompass\n\n### Done\n\n- shipped x\n\n### Plans\n\n- plan q\n\n## portus\n\n### Plans\n\n- plan p\n\n## grimoire\n\nintro line\n";

    fn fixture(md: &str, tag: &str) -> Fx {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let tom = store.create_principal(grimoire_store::PrincipalKind::Human, "tom", None).unwrap().id;
        let claude = store.create_principal(grimoire_store::PrincipalKind::Agent, "claude", None).unwrap().id;
        let (doc, _) = import_markdown(&mut store, "2026-09-10", None, tom, md).unwrap();
        let store = Arc::new(Mutex::new(store));
        let mcp = KsMcp::new(store.clone(), claude, new_dedupe(), new_name_cache(), test_hot(tag));
        Fx { store, mcp, doc, tom, claude }
    }

    impl Fx {
        fn export(&self) -> String {
            grimoire_store::export::export_doc(&*self.store.lock().unwrap(), self.doc).unwrap()
        }
        fn epoch(&self) -> i64 {
            self.store.lock().unwrap().get_doc(self.doc).unwrap().current_epoch
        }
        async fn edit(&self, v: Value) -> (bool, String) {
            let mut v = v;
            v["doc_id"] = json!(self.doc.to_string());
            raw(self.mcp.edit_doc_impl(NONE, p(v)).await.unwrap())
        }
        async fn append(&self, v: Value) -> (bool, String) {
            let mut v = v;
            v["doc_id"] = json!(self.doc.to_string());
            raw(self.mcp.append_impl(NONE, p(v)).await.unwrap())
        }
        async fn read(&self, v: Value) -> (bool, String) {
            let mut v = v;
            v["doc_id"] = json!(self.doc.to_string());
            raw(self.mcp.read_doc(Parameters(p(v))).await.unwrap())
        }
    }

    #[tokio::test]
    async fn edit_doc_one_match_replaces_and_answers_one_line() {
        let fx = fixture(DAILY, "edit1");
        let e0 = fx.epoch();
        let (is_err, out) = fx.edit(json!({"old": "- plan q", "new": "- plan q (done)"})).await;
        assert!(!is_err, "{out}");
        assert_eq!(out, format!("ok · 1 replace · epoch {e0}→{}", e0 + 1));
        assert!(fx.export().contains("- plan q (done)\n\n## portus"));
        // the op is attributed to the shared default
        let ops = fx.store.lock().unwrap().ops_since(fx.doc, e0).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].principal, fx.claude);
        // no-ops
        let (is_err, out) = fx.edit(json!({"old": "same", "new": "same"})).await;
        assert!(!is_err);
        assert_eq!(out, format!("ok · no changes · epoch {}", e0 + 1));
        // multi-block replacement: the new text spans two blocks → replace + insert with new: refs
        let (is_err, out) = fx.edit(json!({"old": "intro line", "new": "intro line\n\nsecond para"})).await;
        assert!(!is_err, "{out}");
        assert!(out.starts_with("ok · 1 insert · epoch"), "{out}");
        assert!(out.lines().any(|l| l.starts_with("new: ^")), "{out}");
        // deletion via empty new
        let (is_err, out) = fx.edit(json!({"old": "\n\nsecond para", "new": ""})).await;
        assert!(!is_err, "{out}");
        assert!(out.starts_with("ok · 1 delete · epoch"), "{out}");
        assert!(!fx.export().contains("second para"));
    }

    #[tokio::test]
    async fn edit_doc_zero_and_many_matches_are_actionable_errors() {
        let fx = fixture(DAILY, "edit0");
        let (is_err, out) = fx.edit(json!({"old": "- shipped y", "new": "z"})).await;
        assert!(is_err);
        assert!(out.starts_with("old not found in doc (epoch"), "{out}");
        assert!(out.contains("closest block ^") && out.contains("- shipped x"), "{out}");
        // whitespace-normalised fallback finds it
        let (is_err, out) = fx.edit(json!({"old": "###   Done\n- shipped x", "new": "### Done\n\n- shipped x, y"})).await;
        assert!(!is_err, "{out}");
        assert!(fx.export().contains("- shipped x, y"));
        // ambiguous: two "### Plans"
        let (is_err, out) = fx.edit(json!({"old": "### Plans", "new": "### Plan"})).await;
        assert!(is_err);
        assert!(out.starts_with("old matches 2 times"), "{out}");
        assert_eq!(out.matches('^').count(), 2, "{out}");
        assert!(out.contains("### Plans"), "{out}");
        // replace_all takes both
        let (is_err, out) = fx.edit(json!({"old": "### Plans", "new": "### Plan", "replace_all": true})).await;
        assert!(!is_err, "{out}");
        assert!(out.starts_with("ok · 2 replace"), "{out}");
        assert_eq!(fx.export().matches("### Plan\n").count(), 2);
        let (is_err, out) = fx.edit(json!({"old": "  ", "new": "x"})).await;
        assert!(is_err && out.contains("old must not be empty"));
        // verbose gives the JSON
        let (_, out) = fx.edit(json!({"old": "- plan p", "new": "- plan p!", "verbose": true})).await;
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["verdicts"][0]["verdict"], "green");
    }

    #[tokio::test]
    async fn edit_doc_repeat_within_ttl_replays_and_hot_docs_refuse() {
        let fx = fixture(DAILY, "edit-dedupe");
        let e0 = fx.epoch();
        let (_, first) = fx.edit(json!({"old": "- plan q", "new": "- plan Q"})).await;
        let (_, again) = fx.edit(json!({"old": "- plan q", "new": "- plan Q"})).await;
        assert_eq!(first, again, "a retry replays the original verdict");
        assert_eq!(fx.epoch(), e0 + 1, "and applies nothing");
        let (_, again_json) = fx.edit(json!({"old": "- plan q", "new": "- plan Q", "verbose": true})).await;
        assert!(again_json.starts_with('{'), "a verbose retry replays the JSON: {again_json}");
        // the same edit by another principal is not a replay (it fails on its own merits)
        let (is_err, out) = fx.edit(json!({"old": "- plan q", "new": "- plan Q", "as": "claude:other"})).await;
        assert!(is_err && out.starts_with("old not found"), "{out}");
        // hot doc
        fx.mcp.hot.start(fx.doc, fx.epoch()).unwrap();
        let (is_err, out) = fx.edit(json!({"old": "- plan p", "new": "x"})).await;
        assert!(is_err && out.contains("live session"), "{out}");
        let (is_err, out) = fx.append(json!({"markdown": "x"})).await;
        assert!(is_err && out.contains("live session"), "{out}");
    }

    #[tokio::test]
    async fn append_lands_at_end_start_and_creates_missing_paths() {
        let fx = fixture(DAILY, "append");
        let e0 = fx.epoch();
        let (is_err, out) = fx.append(json!({"markdown": "- plan q2", "to": "qompass › Plans"})).await;
        assert!(!is_err, "{out}");
        assert!(out.starts_with(&format!("ok · 1 insert · epoch {e0}→{}", e0 + 1)), "{out}");
        assert!(out.lines().nth(1).unwrap().starts_with("new: ^"), "{out}");
        assert!(fx.export().contains("- plan q\n\n- plan q2\n\n## portus"), "{}", fx.export());
        // start of a section: right after the heading
        let (is_err, _) = fx.append(json!({"markdown": "- first", "to": "## qompass / ### plans", "at": "start"})).await;
        assert!(!is_err);
        assert!(fx.export().contains("### Plans\n\n- first\n\n- plan q\n"), "{}", fx.export());
        // ambiguous path → error, nothing written, even with create_missing
        let before = fx.export();
        let (is_err, out) = fx.append(json!({"markdown": "x", "to": "Plans", "create_missing": true})).await;
        assert!(is_err && out.contains("ambiguous") && out.contains("qompass › Plans") && out.contains("portus › Plans"), "{out}");
        assert_eq!(fx.export(), before);
        // missing path without create_missing → error that says how
        let (is_err, out) = fx.append(json!({"markdown": "x", "to": "grimoire › Plans"})).await;
        assert!(is_err && out.contains("create_missing"), "{out}");
        // create_missing: parent-level + 1 under grimoire (##) → ###
        let (is_err, out) = fx.append(json!({"markdown": "- plan g", "to": "grimoire › Plans", "create_missing": true})).await;
        assert!(!is_err, "{out}");
        assert!(out.starts_with("ok · 2 insert"), "{out}");
        assert!(fx.export().ends_with("intro line\n\n### Plans\n\n- plan g\n"), "{}", fx.export());
        // a brand-new top-level section takes the doc's root level (##), explicit levels win
        let (is_err, _) = fx.append(json!({"markdown": "- note", "to": "nats › Notable", "create_missing": true})).await;
        assert!(!is_err);
        assert!(fx.export().ends_with("- plan g\n\n## nats\n\n### Notable\n\n- note\n"), "{}", fx.export());
        let (is_err, _) = fx.append(json!({"markdown": "deep", "to": "# top / #### deep", "create_missing": true})).await;
        assert!(!is_err);
        assert!(fx.export().ends_with("# top\n\n#### deep\n\ndeep\n"), "{}", fx.export());
        // end of doc when `to` is omitted; a block ref as anchor
        let (is_err, _) = fx.append(json!({"markdown": "tail"})).await;
        assert!(!is_err);
        assert!(fx.export().ends_with("deep\n\ntail\n"));
        let shipped = fx.store.lock().unwrap().read_doc(fx.doc).unwrap();
        let shipped = locate::content_blocks(&shipped.roots).iter().find(|b| b.content == "- shipped x").unwrap().id;
        let (is_err, out) = fx.append(json!({"markdown": "- shipped y", "to": short_ref(shipped)})).await;
        assert!(!is_err, "{out}");
        assert!(fx.export().contains("- shipped x\n\n- shipped y\n\n### Plans"), "{}", fx.export());
        let (is_err, out) = fx.append(json!({"markdown": "x", "at": "middle"})).await;
        assert!(is_err && out.contains("at must be"));
        let (is_err, _) = fx.append(json!({"markdown": "  "})).await;
        assert!(is_err);
    }

    #[tokio::test]
    async fn read_doc_is_text_with_header_refs_section_block_and_comments() {
        let fx = fixture(DAILY, "read");
        let (is_err, out) = fx.read(json!({})).await;
        assert!(!is_err);
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), format!("doc {} · epoch {} · 2026-09-10", fx.doc, fx.epoch()));
        assert_eq!(lines.next().unwrap(), "");
        assert_eq!(out.split_once("\n\n").unwrap().1, DAILY);

        // refs: a ^ line above each block, and the read round-trips as zero ops
        let (_, with_refs) = fx.read(json!({"refs": true})).await;
        let body = with_refs.split_once("\n\n").unwrap().1;
        assert!(body.starts_with('^'), "{body}");
        let tree = fx.store.lock().unwrap().read_doc(fx.doc).unwrap();
        assert!(grimoire_store::mddiff::markdown_to_ops(&tree.roots, body).is_empty());
        let (is_err, out) = raw(
            fx.mcp
                .propose_markdown_impl(NONE, p(json!({"doc_id": fx.doc.to_string(), "base_epoch": fx.epoch(), "markdown": body})))
                .await
                .unwrap(),
        );
        assert!(!is_err && out.starts_with("ok · no changes"), "{out}");

        // section
        let (is_err, out) = fx.read(json!({"section": "qompass › Plans", "refs": true})).await;
        assert!(!is_err, "{out}");
        assert!(out.lines().nth(1).unwrap() == "section qompass › Plans", "{out}");
        let body = out.split_once("\n\n").unwrap().1;
        assert_eq!(grimoire_store::mddiff::strip_block_markers(body), "### Plans\n\n- plan q\n");
        let (is_err, out) = fx.read(json!({"section": "Plans"})).await;
        assert!(is_err && out.contains("ambiguous"), "{out}");

        // block
        let plan_q = locate::content_blocks(&tree.roots).iter().find(|b| b.content == "- plan q").unwrap().id;
        let (is_err, out) = fx.read(json!({"block": short_ref(plan_q)})).await;
        assert!(!is_err, "{out}");
        assert!(out.lines().nth(1).unwrap().starts_with(&format!("block {} · paragraph", short_ref(plan_q))), "{out}");
        assert!(out.ends_with("\n\n- plan q\n"), "{out}");
        let (is_err, out) = fx.read(json!({"block": "^000000"})).await;
        assert!(is_err && out.contains("no block ends with"), "{out}");

        // comments
        let (is_err, out) = raw(
            fx.mcp
                .add_comment_impl(NONE, p(json!({"block_id": short_ref(plan_q), "text": "is this still true?", "as": "claude:rev"})))
                .await
                .unwrap(),
        );
        assert!(!is_err, "{out}");
        assert!(out.starts_with("ok · comment ^") && out.contains(&format!("on {}", short_ref(plan_q))), "{out}");
        let cid = out.split_whitespace().nth(3).unwrap().to_string();
        let (is_err, out) = raw(
            fx.mcp
                .add_comment_impl(NONE, p(json!({"block_id": plan_q.to_string(), "text": "yes", "reply_to": cid, "as": "claude:rev2"})))
                .await
                .unwrap(),
        );
        assert!(!is_err, "{out}");
        let (is_err, out) = fx.read(json!({"comments": true})).await;
        assert!(!is_err);
        let comments = out.split("## comments\n").nth(1).unwrap();
        assert!(comments.starts_with(&format!("on {} “- plan q”\n  - {cid} claude:rev: is this still true?\n    - ^", short_ref(plan_q))), "{comments}");
        assert!(comments.contains("claude:rev2: yes"), "{comments}");
        // comments never leak into the markdown body
        let (_, plain) = fx.read(json!({})).await;
        assert!(!plain.contains("is this still true"));
        // outline is still JSON
        let (_, outline) = fx.read(json!({"mode": "outline"})).await;
        let v: Value = serde_json::from_str(&outline).unwrap();
        assert_eq!(v["mode"], "outline");
        assert!(v["blocks"].as_array().unwrap().len() >= 10);
        let (is_err, _) = fx.read(json!({"mode": "full"})).await;
        assert!(is_err, "mode full is gone");
    }

    #[tokio::test]
    async fn verdict_rendering_covers_yellow_red_and_doc_ops() {
        let fx = fixture(DAILY, "verdicts");
        let e0 = fx.epoch();
        // a stale propose against a block someone else changed → non-green lines
        let tree = fx.store.lock().unwrap().read_doc(fx.doc).unwrap();
        let plan_q = locate::content_blocks(&tree.roots).iter().find(|b| b.content == "- plan q").unwrap().id;
        let intro = locate::content_blocks(&tree.roots).iter().find(|b| b.content == "intro line").unwrap().id;
        fx.store
            .lock()
            .unwrap()
            .apply(fx.doc, e0, fx.tom, vec![OpInput { kind: OpKind::Replace { target: plan_q, content: "- plan q v2".into() }, source_refs: vec![] }])
            .unwrap();
        let (is_err, out) = raw(
            fx.mcp
                .propose_impl(
                    NONE,
                    p(json!({"doc_id": fx.doc.to_string(), "base_epoch": e0, "ops": [
                        {"kind": {"op": "replace", "target": plan_q, "content": "- plan q v3"}},
                        {"kind": {"op": "replace", "target": intro, "content": "intro edited"}},
                        {"kind": {"op": "insert", "parent_id": null, "order_key": "", "block_type": "paragraph", "content": "brand new"}}
                    ]})),
                )
                .await
                .unwrap(),
        );
        assert!(!is_err, "{out}");
        let first = out.lines().next().unwrap();
        assert!(first.starts_with("ok · 2 replace · 1 insert · "), "{first}");
        assert!(first.contains("green") && (first.contains("yellow (flagged)") || first.contains("red — proposed text preserved")), "{first}");
        assert!(first.contains(&format!("epoch {}→{}", e0 + 1, e0 + 2)), "{first}");
        assert!(out.lines().skip(1).any(|l| (l.starts_with("yellow ^") || l.starts_with("red ^")) && l.contains(&short_ref(plan_q)[1..])), "{out}");
        assert!(out.lines().last().unwrap().starts_with("new: ^"), "{out}");

        // pending lists the flagged/parked op; resolve renders one line
        let (is_err, pending) = text_of(fx.mcp.proposals_impl(NONE, p(json!({"kind": "pending"}))).await.unwrap());
        assert!(!is_err);
        assert_eq!(pending["total"], 1, "{pending}");
        let ann = pending["items"][0]["annotation"]["id"].as_str().unwrap().to_string();
        let (is_err, out) = raw(fx.mcp.resolve_impl(NONE, p(json!({"annotation_id": ann, "decision": "accept"}))).await.unwrap());
        assert!(is_err && out.contains("own"), "proposer ≠ approver: {out}");
        let (is_err, mine) = text_of(fx.mcp.proposals_impl(NONE, p(json!({}))).await.unwrap());
        assert!(!is_err);
        assert_eq!(mine["proposals"].as_array().unwrap().len(), 3);
        assert!(mine["proposals"][0]["op"].get("principal").is_none(), "compact");
        let (is_err, _) = text_of(fx.mcp.proposals_impl(NONE, p(json!({"kind": "all"}))).await.unwrap());
        assert!(is_err);

        // doc ops: rename yellow, delete red, merge summary
        let (is_err, out) = raw(fx.mcp.doc_op_impl(NONE, p(json!({"op": "rename", "doc_id": fx.doc.to_string(), "title": "Renamed"}))).await.unwrap());
        assert!(!is_err, "{out}");
        assert!(out.starts_with("ok · 1 rename · 1 yellow (flagged) · epoch"), "{out}");
        assert!(out.lines().nth(1).unwrap().starts_with("yellow doc"), "{out}");
        assert_eq!(fx.store.lock().unwrap().get_doc(fx.doc).unwrap().title, "Renamed");
        let (is_err, out) = raw(fx.mcp.doc_op_impl(NONE, p(json!({"op": "status", "doc_id": fx.doc.to_string(), "status": "decided"}))).await.unwrap());
        assert!(!is_err && out.starts_with("ok · 1 status · 1 yellow"), "{out}");
        let (is_err, out) = raw(fx.mcp.doc_op_impl(NONE, p(json!({"op": "delete", "doc_id": fx.doc.to_string()}))).await.unwrap());
        assert!(!is_err, "{out}");
        assert!(out.starts_with("parked · 1 delete doc · 1 red — proposed text preserved · epoch"), "{out}");
        assert!(out.contains("(unchanged)"), "{out}");
        let (is_err, out) = raw(fx.mcp.doc_op_impl(NONE, p(json!({"op": "rename", "doc_id": fx.doc.to_string()}))).await.unwrap());
        assert!(is_err && out.contains("needs title"));
        let (is_err, out) = raw(fx.mcp.doc_op_impl(NONE, p(json!({"op": "explode", "doc_id": fx.doc.to_string()}))).await.unwrap());
        assert!(is_err && out.contains("op must be"));
        // merge: the other doc into this one
        let other = {
            let mut s = fx.store.lock().unwrap();
            import_markdown(&mut *s, "Other", None, fx.tom, "## other\n\nbody\n").unwrap().0
        };
        let (is_err, out) = raw(
            fx.mcp
                .doc_op_impl(NONE, p(json!({"op": "merge", "doc_id": other.to_string(), "into_doc_id": fx.doc.to_string()})))
                .await
                .unwrap(),
        );
        assert!(!is_err, "{out}");
        assert!(out.starts_with("ok · merge · ") && out.contains(" appended (yellow, flagged) · delete parked red"), "{out}");
        // create_doc: one line, reuse
        let (is_err, out) = raw(fx.mcp.create_doc_impl(NONE, p(json!({"title": "New", "markdown": "hi"}))).await.unwrap());
        assert!(!is_err, "{out}");
        assert!(out.starts_with("ok · created “New” · doc ") && out.ends_with("· epoch 1"), "{out}");
        let (is_err, out) = raw(fx.mcp.create_doc_impl(NONE, p(json!({"title": "New", "if_exists": "reuse"}))).await.unwrap());
        assert!(!is_err && out.starts_with("ok · reused “New” · doc "), "{out}");
        let (is_err, _) = raw(fx.mcp.create_doc_impl(NONE, p(json!({"title": "New"}))).await.unwrap());
        assert!(is_err);
    }

    #[tokio::test]
    async fn propose_markdown_stale_base_is_a_one_line_error_with_the_misses() {
        let fx = fixture(DAILY, "stale");
        let e0 = fx.epoch();
        fx.edit(json!({"old": "- plan p", "new": "- plan p2"})).await;
        let (is_err, out) = raw(
            fx.mcp
                .propose_markdown_impl(NONE, p(json!({"doc_id": fx.doc.to_string(), "base_epoch": e0, "markdown": "whatever"})))
                .await
                .unwrap(),
        );
        assert!(is_err, "{out}");
        assert!(out.starts_with(&format!("stale_base: doc is at epoch {} (you read {e0}); missed 1 op(s): replace ^", e0 + 1)), "{out}");
        let (is_err, out) = text_of(
            fx.mcp
                .propose_markdown_impl(NONE, p(json!({"doc_id": fx.doc.to_string(), "base_epoch": e0, "markdown": "whatever", "verbose": true})))
                .await
                .unwrap(),
        );
        assert!(!is_err && out["error"] == "stale_base" && out["missed_ops"].as_array().unwrap().len() == 1, "{out}");
        // a fresh full rewrite works and keeps ids
        let md = fx.export().replace("intro line", "intro rewritten");
        let (is_err, out) = raw(
            fx.mcp
                .propose_markdown_impl(NONE, p(json!({"doc_id": fx.doc.to_string(), "base_epoch": e0 + 1, "markdown": md})))
                .await
                .unwrap(),
        );
        assert!(!is_err && out.starts_with("ok · 1 replace · epoch"), "{out}");
    }

    /// Through the tool: a write with `as` records that principal on the
    /// ledger op; a second KsMcp (a fresh per-request instance, as on
    /// stateless protocols) sees the same principal via the shared cache; a
    /// request hint (?as= / header / ?cwd=) attributes the same way, and
    /// `proposals(kind: mine)` with that `as` lists the op.
    #[tokio::test]
    async fn writes_with_as_or_request_hint_record_that_principal() {
        let _serial = AUTO_CREATED_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = swap_auto_created_for_test(0);
        let mut store = SqliteStore::open_in_memory().unwrap();
        let tom = store.create_principal(grimoire_store::PrincipalKind::Human, "tom", None).unwrap().id;
        let claude = store.create_principal(grimoire_store::PrincipalKind::Agent, "claude", None).unwrap().id;
        let (doc, _) = import_markdown(&mut store, "Notes", None, tom, "first\n").unwrap();
        let epoch = store.read_doc(doc).unwrap().doc.current_epoch;
        let store = Arc::new(Mutex::new(store));
        let (dedupe, names) = (new_dedupe(), new_name_cache());
        let fresh = || KsMcp::new(store.clone(), claude, dedupe.clone(), names.clone(), test_hot("as"));
        let hint = |q: Option<&str>, cwd: Option<&str>, h: Option<&str>| RequestHint {
            header: h.map(String::from),
            query_as: q.map(String::from),
            cwd: cwd.map(String::from),
        };

        let (is_err, out) = raw(fresh().append_impl(NONE, p(json!({"doc_id": doc.to_string(), "markdown": "by task", "as": "claude:proj-task"}))).await.unwrap());
        assert!(!is_err, "{out}");
        let task = names.lock().unwrap().get("claude:proj-task").copied().expect("as cached");
        assert_ne!(task, claude);
        let ops = store.lock().unwrap().ops_since(doc, epoch).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].principal, task, "the op is attributed to the `as` principal");

        // ?as= on the URL
        let (is_err, out) = raw(fresh().append_impl(hint(Some("claude:via-query"), None, None), p(json!({"doc_id": doc.to_string(), "markdown": "by query"}))).await.unwrap());
        assert!(!is_err, "{out}");
        let ops = store.lock().unwrap().ops_since(doc, epoch).unwrap();
        assert_eq!(ops[1].principal, agent_principal_by_name(&mut store.lock().unwrap(), "claude:via-query").unwrap());
        // ?cwd=
        let (is_err, out) = raw(fresh().append_impl(hint(None, Some("/Users/me/portus"), None), p(json!({"doc_id": doc.to_string(), "markdown": "by cwd"}))).await.unwrap());
        assert!(!is_err, "{out}");
        let ops = store.lock().unwrap().ops_since(doc, epoch).unwrap();
        assert_eq!(ops[2].principal, agent_principal_by_name(&mut store.lock().unwrap(), "claude:portus").unwrap());
        // header beats both, `as` beats the header
        let (is_err, _) = raw(fresh().append_impl(hint(Some("claude:q"), Some("/x/c"), Some("claude:hdr")), p(json!({"doc_id": doc.to_string(), "markdown": "by header"}))).await.unwrap());
        assert!(!is_err);
        let (is_err, _) = raw(fresh().append_impl(hint(Some("claude:q"), Some("/x/c"), Some("claude:hdr")), p(json!({"doc_id": doc.to_string(), "markdown": "by arg", "as": "claude:proj-task"}))).await.unwrap());
        assert!(!is_err);
        let ops = store.lock().unwrap().ops_since(doc, epoch).unwrap();
        assert_eq!(ops[3].principal, agent_principal_by_name(&mut store.lock().unwrap(), "claude:hdr").unwrap());
        assert_eq!(ops[4].principal, task);
        // no hint, no as → the shared default
        let (is_err, _) = raw(fresh().append_impl(NONE, p(json!({"doc_id": doc.to_string(), "markdown": "by default"}))).await.unwrap());
        assert!(!is_err);
        assert_eq!(store.lock().unwrap().ops_since(doc, epoch).unwrap()[5].principal, claude);

        // the human is refused at the tool boundary, nothing written
        let (is_err, msg) = raw(fresh().append_impl(NONE, p(json!({"doc_id": doc.to_string(), "markdown": "nope", "as": "tom"}))).await.unwrap());
        assert!(is_err && msg.contains("not an agent"), "{msg}");
        assert_eq!(store.lock().unwrap().ops_since(doc, epoch).unwrap().len(), 6);

        let (_, mine) = text_of(fresh().proposals_impl(NONE, p(json!({"as": "claude:proj-task"}))).await.unwrap());
        assert_eq!(mine["principal"], json!(task));
        assert_eq!(mine["proposals"].as_array().unwrap().len(), 2);
        let (_, mine) = text_of(fresh().proposals_impl(hint(None, Some("/Users/me/portus"), None), p(json!({}))).await.unwrap());
        assert_eq!(mine["proposals"].as_array().unwrap().len(), 1, "the hint scopes proposals too");
        swap_auto_created_for_test(previous);
    }
}
