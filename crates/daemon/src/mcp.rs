//! MCP tools over streamable HTTP (tickets 3.1–3.6, 3.7, #52/#53).
//!
//! All writes act as the `claude` agent principal and go through the propose
//! gate — the MCP surface has no direct-write path by design.

use crate::store_ext::with_store;
use grimoire_store::{BlockNode, BlockStore, OpInput, ReviewDecision, SqliteStore};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Idempotency cache: (principal, request_id) → serialized outcome. Bounded,
/// in-memory; a retried propose with the same request_id returns the stored
/// outcome instead of double-applying. Keyed by principal so one session's
/// request_id can never replay another session's outcome.
/// Values carry an insertion sequence so eviction drops the OLDEST half
/// instead of clearing — a retry storm never wipes an in-window entry.
pub type DedupeCache = Arc<Mutex<std::collections::HashMap<(Uuid, Uuid), (u64, serde_json::Value)>>>;

pub const DEDUPE_CAPACITY: usize = 512;
static DEDUPE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn dedupe_get(cache: &DedupeCache, principal: Uuid, id: Uuid) -> Option<serde_json::Value> {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(principal, id))
        .map(|(_, v)| v.clone())
}

pub fn new_dedupe() -> DedupeCache {
    Arc::new(Mutex::new(std::collections::HashMap::new()))
}

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

/// A principal name: 1–64 printable chars (no control characters), trimmed.
pub fn valid_principal_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err("name must be 1-64 printable chars".into());
    }
    Ok(name)
}

/// Find-or-create the Agent principal named `name` (the `identify` rule,
/// shared with the HTTP `X-Grimoire-Principal` header). Creation is capped
/// per boot; existing names always resolve.
pub fn agent_principal_by_name(store: &mut SqliteStore, name: &str) -> Result<Uuid, String> {
    let name = valid_principal_name(name)?;
    let existing = store
        .list_principals()
        .ok()
        .and_then(|ps| ps.into_iter().find(|pr| pr.display_name == name));
    if let Some(pr) = existing {
        return Ok(pr.id);
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

pub fn dedupe_put(cache: &DedupeCache, principal: Uuid, id: Uuid, v: serde_json::Value) {
    let mut c = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.len() >= DEDUPE_CAPACITY {
        // evict the oldest half: the newest entries are the ones a retry
        // in flight can still ask for
        let mut seqs: Vec<u64> = c.values().map(|(seq, _)| *seq).collect();
        seqs.sort_unstable();
        let cutoff = seqs[seqs.len() / 2];
        c.retain(|_, (seq, _)| *seq >= cutoff);
    }
    let seq = DEDUPE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    c.insert((principal, id), (seq, v));
}

#[cfg(test)]
mod cache_and_name_tests {
    use super::*;

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
    fn principal_names_are_bounded_and_printable() {
        assert_eq!(valid_principal_name("  claude:proj-task "), Ok("claude:proj-task"));
        assert!(valid_principal_name("").is_err());
        assert!(valid_principal_name("   ").is_err());
        assert!(valid_principal_name("a\u{7}b").is_err());
        assert!(valid_principal_name("line\nbreak").is_err());
        assert!(valid_principal_name(&"x".repeat(64)).is_ok());
        assert!(valid_principal_name(&"x".repeat(65)).is_err());
    }
}

#[derive(Clone)]
pub struct KsMcp {
    store: Arc<Mutex<SqliteStore>>,
    dedupe: DedupeCache,
    /// The freeze: content writes against a live doc are refused (P2.3).
    hot: crate::hot::HotState,
    /// Default principal for un-identified sessions.
    agent: Uuid,
    /// Per-session identity set via the `identify` tool — distinct provenance
    /// for concurrent agent sessions.
    identity: Arc<Mutex<Option<Uuid>>>,
    // referenced only through the #[tool_handler] macro's generated code
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl KsMcp {
    fn principal(&self) -> Uuid {
        self.identity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unwrap_or(self.agent)
    }
}

fn ok_json<T: Serialize>(v: &T) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("serialize error: {e}")),
    )]))
}

fn err(msg: String) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(msg)]))
}

fn parse_uuid(s: &str, what: &str) -> std::result::Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|_| format!("{what} is not a valid UUID: {s}"))
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadDocParams {
    /// Doc UUID.
    pub doc_id: String,
    /// "outline" (default: block ids + first lines, token-cheap), "full"
    /// (every block's content as JSON), or "markdown" (the whole doc as one
    /// markdown string with `<!-- block <uuid> -->` marker lines — edit it
    /// and hand it to propose_markdown; the markers are stripped server-side).
    pub mode: Option<String>,
    /// Include provenance fields on blocks (created_by, epoch, deleted,
    /// refers_to). Default false: agents rarely need them.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadBlockParams {
    /// Block UUID.
    pub block_id: String,
    /// Include provenance fields (created_by, epoch, deleted, refers_to).
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
pub struct TreeParams {
    /// Start from this doc (UUID); omit for the corpus top level.
    pub root_doc_id: Option<String>,
    /// Levels to expand (default 2); deeper subtrees show as "(N more)".
    pub depth: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RenameDocParams {
    pub doc_id: String,
    /// The new title.
    pub title: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct MoveDocParams {
    pub doc_id: String,
    /// Destination parent doc (UUID); omit for the root level.
    pub new_parent_id: Option<String>,
    /// Land right after this sibling (UUID, must be a child of the new parent); omit to append last.
    pub after_doc_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetStatusParams {
    pub doc_id: String,
    /// "draft" | "in-review" | "decided" | "superseded"; omit or "null" to clear.
    pub status: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteDocParams {
    pub doc_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct MergeDocsParams {
    /// The doc whose content moves (and which is then trashed, pending review).
    pub from_doc_id: String,
    /// The doc that receives the content.
    pub into_doc_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeParams {
    /// Doc UUID.
    pub doc_id: String,
    /// The doc epoch your read was based on (from read_doc).
    pub base_epoch: i64,
    /// Ops array. Each op: {"kind": {"op": "insert"|"replace"|"delete"|"move", ...},
    /// "source_refs": ["..."]}. insert: parent_id (block UUID or null),
    /// order_key, block_type, content, and optionally block_id (omit it and the
    /// server mints one — it comes back in the verdict's block_id). replace:
    /// target, content. delete: target. move: target, new_parent, new_order_key.
    pub ops: serde_json::Value,
    /// Optional idempotency key (any UUID you generate): retrying a timed-out
    /// propose with the same request_id returns the original outcome instead
    /// of double-applying.
    pub request_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeMarkdownParams {
    /// Doc UUID.
    pub doc_id: String,
    /// The doc epoch your read was based on (from read_doc).
    pub base_epoch: i64,
    /// The doc's complete new markdown content.
    pub markdown: String,
    /// Optional idempotency key (any UUID).
    pub request_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DiffSinceParams {
    /// Doc UUID.
    pub doc_id: String,
    /// Return ops applied after this epoch.
    pub since_epoch: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Substring to find in block content.
    pub query: String,
    /// Max hits (default 20).
    pub limit: Option<u32>,
    /// Include provenance fields on hit blocks.
    pub verbose: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReviewQueueParams {
    /// Restrict to one doc (UUID); omit for all docs.
    pub doc_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ResolveParams {
    /// Annotation UUID from review_queue.
    pub annotation_id: String,
    /// "accept" or "decline".
    pub decision: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateDocParams {
    pub title: String,
    /// Parent doc UUID for tree placement; omit for root.
    pub parent_doc_id: Option<String>,
    /// Initial content: the whole doc as markdown, written in the same call.
    pub markdown: Option<String>,
    /// "error" (default): fail if a live doc with this exact title already
    /// exists under the parent. "reuse": return that doc instead (with
    /// `reused: true`, and `markdown` ignored) — an atomic find-or-create.
    pub if_exists: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct BacklinksParams {
    /// Doc UUID whose inbound [[wikilinks]] you want.
    pub doc_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddCommentParams {
    /// Content block UUID the comment anchors to.
    pub block_id: String,
    pub text: String,
    /// Comment UUID to reply to (same thread); omit for a new thread.
    pub reply_to: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListCommentsParams {
    /// Content block UUID.
    pub block_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct IdentifyParams {
    /// Session name, e.g. "claude:myproject-refactor".
    pub name: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct MyProposalsParams {
    /// Max ops to return (default 20).
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DocsByTagParams {
    pub tag: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListDocsParams {
    /// Restrict to this doc's subtree (UUID); omit for the whole corpus.
    pub parent_doc_id: Option<String>,
}

#[derive(Serialize)]
struct FlatBlock {
    id: Uuid,
    parent_id: Option<Uuid>,
    depth: usize,
    block_type: &'static str,
    content: String,
}

fn flatten(nodes: &[BlockNode], depth: usize, full: bool, out: &mut Vec<FlatBlock>) {
    for n in nodes {
        let content = if full {
            n.block.content.clone()
        } else {
            let first = n.block.content.lines().next().unwrap_or("");
            let mut s: String = first.chars().take(100).collect();
            if s.len() < n.block.content.len() {
                s.push('…');
            }
            s
        };
        out.push(FlatBlock {
            id: n.block.id,
            parent_id: n.block.parent_id,
            depth,
            block_type: n.block.block_type.as_str(),
            content,
        });
        flatten(&n.children, depth + 1, full, out);
    }
}

#[tool_router]
impl KsMcp {
    pub fn new(
        store: Arc<Mutex<SqliteStore>>,
        agent: Uuid,
        dedupe: DedupeCache,
        hot: crate::hot::HotState,
    ) -> Self {
        Self {
            store,
            dedupe,
            hot,
            agent,
            identity: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Identify this session with a name (e.g. 'claude:myproject-refactor'). Your writes are then attributed to that principal instead of the shared 'claude' — do this first in any session that writes, so provenance distinguishes concurrent agents."
    )]
    async fn identify(
        &self,
        Parameters(p): Parameters<IdentifyParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = p.name.trim().to_string();
        let principal = {
            let name = name.clone();
            with_store(&self.store, move |store| agent_principal_by_name(store, &name)).await
        };
        let principal = match principal {
            Ok(id) => id,
            Err(m) => return err(m),
        };
        *self
            .identity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(principal);
        ok_json(&json!({"identified_as": name, "principal": principal}))
    }

    #[tool(
        description = "What happened to this session's recent proposals: each op with its verdict, whether its review annotation was accepted/declined/open, and who resolved it. Use to learn from declines."
    )]
    async fn my_proposals(
        &self,
        Parameters(p): Parameters<MyProposalsParams>,
    ) -> Result<CallToolResult, McpError> {
        let principal = self.principal();
        with_store(&self.store, move |store| {
            match store.proposal_outcomes(principal, p.limit.unwrap_or(20) as usize) {
                Ok(rows) => ok_json(
                    &rows
                        .into_iter()
                        .map(|(op, status, resolver)| {
                            json!({
                                "op": op,
                                "review_status": status,
                                "resolved_by": resolver,
                            })
                        })
                        .collect::<Vec<_>>(),
                ),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(description = "All tags with doc counts — the vocabulary.")]
    async fn list_tags(&self) -> Result<CallToolResult, McpError> {
        with_store(&self.store, move |store| {
            match store.list_tags() {
                Ok(t) => ok_json(&t),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(description = "Docs carrying a tag.")]
    async fn docs_by_tag(
        &self,
        Parameters(p): Parameters<DocsByTagParams>,
    ) -> Result<CallToolResult, McpError> {
        with_store(&self.store, move |store| {
            match store.docs_by_tag(&p.tag) {
                Ok(d) => ok_json(&d),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Compact doc rows {id, title, parent_id, epoch} for ONE subtree: pass parent_doc_id. Without it the whole corpus is returned only when it is small (≤200 docs) — otherwise use tree (shape) or find_doc (lookup by name). Not for finding a doc: find_doc is."
    )]
    async fn list_docs(
        &self,
        Parameters(p): Parameters<ListDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        with_store(&self.store, move |store| {
            let docs = match p.parent_doc_id.as_deref() {
                Some(root) => match parse_uuid(root, "parent_doc_id") {
                    Ok(root) => store.doc_subtree(root),
                    Err(m) => return err(m),
                },
                None => store.list_docs(),
            };
            let docs = match docs {
                Ok(d) => d,
                Err(e) => return err(e.to_string()),
            };
            if p.parent_doc_id.is_none() && docs.len() > crate::nav::LIST_DOCS_LIMIT {
                return err(format!(
                    "the corpus has {} docs — too many to list. Use find_doc(query) to look one up, \
                     tree(depth) to see the shape, or pass parent_doc_id to list one subtree.",
                    docs.len()
                ));
            }
            ok_json(&docs.iter().map(crate::nav::compact_doc).collect::<Vec<_>>())
        })
        .await
    }

    #[tool(
        description = "Find docs by name: fuzzy match over titles and breadcrumb paths ('Folder › Sub › Title'), case-insensitive, typo-tolerant. Ranked exact > prefix > substring > all words in path > fuzzy. Returns {id, title, path, parent_id, epoch, status}. Use this instead of list_docs to look a doc up."
    )]
    async fn find_doc(
        &self,
        Parameters(p): Parameters<FindDocParams>,
    ) -> Result<CallToolResult, McpError> {
        let parent = match p
            .parent_doc_id
            .as_deref()
            .map(|s| parse_uuid(s, "parent_doc_id"))
            .transpose()
        {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let limit = p.limit.unwrap_or(8).clamp(1, 50) as usize;
        with_store(&self.store, move |store| {
            match store.list_docs() {
                Ok(docs) => ok_json(&crate::nav::find_docs(&docs, &p.query, parent, limit)),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "The doc tree as indented text, one line per doc: '- Title  [id]', with subtrees below `depth` (default 2) collapsed to '(N more)'. Cheap orientation: call once, then find_doc/read_doc. root_doc_id zooms into one subtree."
    )]
    async fn tree(&self, Parameters(p): Parameters<TreeParams>) -> Result<CallToolResult, McpError> {
        let root = match p
            .root_doc_id
            .as_deref()
            .map(|s| parse_uuid(s, "root_doc_id"))
            .transpose()
        {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let depth = p.depth.unwrap_or(2).clamp(1, 12) as usize;
        with_store(&self.store, move |store| {
            match store.list_docs() {
                Ok(docs) => Ok(CallToolResult::success(vec![ContentBlock::text(
                    crate::nav::render_tree(&docs, root, depth),
                )])),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Read a doc. Returns the doc's current epoch — quote it as base_epoch when proposing. mode 'outline' (default): block ids, types, first lines. mode 'markdown': the whole doc as ONE markdown string with '<!-- block <uuid> -->' lines above each block — the input to edit and send back via propose_markdown (markers are stripped server-side; unchanged markdown = zero ops). mode 'full': every block as JSON. Blocks carry id, parent_id, block_type, content unless verbose."
    )]
    async fn read_doc(
        &self,
        Parameters(p): Parameters<ReadDocParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let mode = p.mode.clone().unwrap_or_else(|| "outline".into());
        if !matches!(mode.as_str(), "outline" | "full" | "markdown") {
            return err(format!("mode must be outline | full | markdown, got {mode}"));
        }
        with_store(&self.store, move |store| {
            let tree = match store.read_doc(doc_id) {
                Ok(t) => t,
                Err(e) => return err(e.to_string()),
            };
            let doc = json!({
                "id": tree.doc.id,
                "title": tree.doc.title,
                "parent_id": tree.doc.parent_id,
                "status": tree.doc.status,
                "review_policy": tree.doc.review_policy,
            });
            if mode == "markdown" {
                return ok_json(&json!({
                    "doc": doc,
                    "epoch": tree.doc.current_epoch,
                    "mode": "markdown",
                    "markdown": grimoire_store::export::markdown_of(&tree.roots, true),
                }));
            }
            let full = mode == "full";
            let mut blocks = Vec::new();
            flatten(&tree.roots, 0, full, &mut blocks);
            ok_json(&json!({
                "doc": if p.verbose.unwrap_or(false) { json!(tree.doc) } else { doc },
                "epoch": tree.doc.current_epoch,
                "mode": mode,
                "blocks": blocks,
            }))
        })
        .await
    }

    #[tool(
        description = "Read one block in full (any block id from read_doc or search). Returns id, doc_id, parent_id, order_key, block_type, content (plus provenance with verbose)."
    )]
    async fn read_block(
        &self,
        Parameters(p): Parameters<ReadBlockParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = match parse_uuid(&p.block_id, "block_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let verbose = p.verbose.unwrap_or(false);
        with_store(&self.store, move |store| {
            match store.read_block(id) {
                Ok(b) => ok_json(&crate::nav::compact_block(&b, verbose)),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Propose block edits through the review gate. Returns per-op structured verdicts {op_id, block_id, verdict, applied, note}: green = applied; yellow = applied, flagged for review; red = parked unapplied (your text is preserved for a reviewer). A stale base_epoch is fine — ops against unchanged blocks still green. Never guess base_epoch: read_doc first and quote its epoch. For inserts, block_id is optional (the server mints one and returns it in the verdict); set order_key to \"\" to append after the last sibling, or \"after:<block-uuid>\" to insert after a specific block — the server assigns the real key; never compute keys yourself. For anything beyond a one-block change prefer propose_markdown."
    )]
    async fn propose(
        &self,
        Parameters(p): Parameters<ProposeParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let ops: Vec<OpInput> = match serde_json::from_value(p.ops.clone()) {
            Ok(o) => o,
            Err(e) => return err(format!("ops did not parse: {e}")),
        };
        let request_id = match p.request_id.as_deref().map(|s| parse_uuid(s, "request_id")) {
            Some(Ok(u)) => Some(u),
            Some(Err(m)) => return err(m),
            None => None,
        };
        let principal = self.principal();
        if let Some(rid) = request_id
            && let Some(prev) = dedupe_get(&self.dedupe, principal, rid)
        {
            return ok_json(&prev);
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
            match store.propose(doc_id, p.base_epoch, principal, ops) {
                Ok(out) => {
                    if let Some(rid) = request_id {
                        dedupe_put(
                            &dedupe,
                            principal,
                            rid,
                            serde_json::to_value(&out).unwrap_or_default(),
                        );
                    }
                    ok_json(&out)
                }
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "THE EASY WRITE PATH: hand over a doc's complete new markdown; the server diffs it against the current blocks and proposes minimal ops through the gate — unchanged blocks keep their ids (provenance and comment anchors survive), edits become replaces, new/removed paragraphs become inserts/deletes. Loop: read_doc(mode 'markdown') → edit the string (keep or drop the '<!-- block … -->' markers, both work) → propose_markdown with that read's epoch → verdicts. Prefer this over hand-built block ops for anything beyond a single-block change."
    )]
    async fn propose_markdown(
        &self,
        Parameters(p): Parameters<ProposeMarkdownParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let request_id = match p.request_id.as_deref().map(|s| parse_uuid(s, "request_id")) {
            Some(Ok(u)) => Some(u),
            Some(Err(m)) => return err(m),
            None => None,
        };
        let principal = self.principal();
        if let Some(rid) = request_id
            && let Some(prev) = dedupe_get(&self.dedupe, principal, rid)
        {
            return ok_json(&prev);
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
                return ok_json(&json!({
                    "error": "stale_base",
                    "base_epoch": p.base_epoch,
                    "current_epoch": tree.doc.current_epoch,
                    "missed_ops": missed,
                    "recover": "re-read the doc (mode full), re-apply your edit to the fresh markdown, re-send with the current epoch",
                }));
            }
            let ops = grimoire_store::mddiff::markdown_to_ops(&tree.roots, &p.markdown);
            if ops.is_empty() {
                return ok_json(
                    &json!({"doc_id": doc_id, "epoch": tree.doc.current_epoch, "verdicts": [], "note": "no changes"}),
                );
            }
            match store.propose(doc_id, p.base_epoch, principal, ops) {
                Ok(out) => {
                    if let Some(rid) = request_id {
                        dedupe_put(
                            &dedupe,
                            principal,
                            rid,
                            serde_json::to_value(&out).unwrap_or_default(),
                        );
                    }
                    ok_json(&out)
                }
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Ops applied to a doc after a given epoch — what you missed. Use to recover from a stale base before re-proposing, or to see what changed."
    )]
    async fn diff_since(
        &self,
        Parameters(p): Parameters<DiffSinceParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        with_store(&self.store, move |store| {
            match store.ops_since(doc_id, p.since_epoch) {
                Ok(ops) => ok_json(&ops),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Search live block content (substring / trigram, typo-tolerant). Hits are {block, doc_title} — the editable unit, not whole pages. Blocks are compact unless verbose. To find a DOC by name use find_doc."
    )]
    async fn search(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let verbose = p.verbose.unwrap_or(false);
        with_store(&self.store, move |store| {
            match store.search_blocks(&p.query, p.limit.unwrap_or(20) as usize) {
                Ok(hits) => ok_json(
                    &hits
                        .iter()
                        .map(|h| {
                            json!({
                                "block": crate::nav::compact_block(&h.block, verbose),
                                "doc_title": h.doc_title,
                            })
                        })
                        .collect::<Vec<_>>(),
                ),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Open review annotations (applied-but-flagged yellows, parked reds) with their ops, oldest first. Includes doc ops (rename_doc / move_doc / set_status / delete_doc) alongside block ops."
    )]
    async fn review_queue(
        &self,
        Parameters(p): Parameters<ReviewQueueParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match p
            .doc_id
            .as_deref()
            .map(|s| parse_uuid(s, "doc_id"))
            .transpose()
        {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        with_store(&self.store, move |store| {
            match store.review_queue(doc_id) {
                Ok(q) => ok_json(&q),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Resolve one review annotation as this agent: accept or decline. You cannot resolve your own proposals (proposer ≠ approver)."
    )]
    async fn resolve(
        &self,
        Parameters(p): Parameters<ResolveParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = match parse_uuid(&p.annotation_id, "annotation_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let decision = match p.decision.as_str() {
            "accept" => ReviewDecision::Accept,
            "decline" => ReviewDecision::Decline,
            other => return err(format!("decision must be accept|decline, got {other}")),
        };
        let (hot, principal) = (self.hot.clone(), self.principal());
        with_store(&self.store, move |store| {
            if let Some(doc) = crate::hot::annotation_doc(&store, id)
                && let Err(m) = hot.assert_cold(doc)
            {
                return err(m);
            }
            match store.resolve(id, principal, decision) {
                Ok(receipt) => ok_json(&json!({ "resolved": true, "receipt": receipt })),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Blocks anywhere in the corpus that [[wikilink]] to this doc — reviewer context for 'what links here'."
    )]
    async fn backlinks(
        &self,
        Parameters(p): Parameters<BacklinksParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        with_store(&self.store, move |store| {
            match store.backlinks(doc_id) {
                Ok(hits) => ok_json(&hits),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Attach a comment to a content block (or reply within a thread via reply_to). Comments are blocks: provenance applies, threads survive edits."
    )]
    async fn add_comment(
        &self,
        Parameters(p): Parameters<AddCommentParams>,
    ) -> Result<CallToolResult, McpError> {
        let block_id = match parse_uuid(&p.block_id, "block_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let reply_to = match p
            .reply_to
            .as_deref()
            .map(|s| parse_uuid(s, "reply_to"))
            .transpose()
        {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let principal = self.principal();
        with_store(&self.store, move |store| {
            match store.add_comment(block_id, principal, &p.text, reply_to) {
                Ok(c) => ok_json(&c),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(description = "All comments anchored to a content block (threads via parent_id).")]
    async fn list_comments(
        &self,
        Parameters(p): Parameters<ListCommentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let block_id = match parse_uuid(&p.block_id, "block_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        with_store(&self.store, move |store| {
            match store.list_comments(block_id) {
                Ok(c) => ok_json(&c),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }

    #[tool(
        description = "Retitle a doc THROUGH THE GATE: applied now as a flagged yellow (a reviewer's decline renames it back), and every inbound [[wikilink]] is rewritten to the new title. Returns the same verdict shape as propose. Refused for docs shared with you (mirrors)."
    )]
    async fn rename_doc(
        &self,
        Parameters(p): Parameters<RenameDocParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let principal = self.principal();
        with_store(&self.store, move |store| {
            match crate::docops::rename(store, doc_id, &p.title, principal) {
                Ok(out) => ok_json(&out),
                Err(m) => err(m),
            }
        })
        .await
    }

    #[tool(
        description = "Move a doc in the tree THROUGH THE GATE: reparent to new_parent_id (omit = root) and optionally land right after after_doc_id (omit = append last). Applied now as a flagged yellow; decline moves it back. Same boundary rules as the app: nothing lands inside a tree shared with you, and shared docs move only at their share root."
    )]
    async fn move_doc(
        &self,
        Parameters(p): Parameters<MoveDocParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let new_parent = match p
            .new_parent_id
            .as_deref()
            .map(|s| parse_uuid(s, "new_parent_id"))
            .transpose()
        {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let after = match p
            .after_doc_id
            .as_deref()
            .map(|s| parse_uuid(s, "after_doc_id"))
            .transpose()
        {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let principal = self.principal();
        with_store(&self.store, move |store| {
            match crate::docops::move_doc(store, doc_id, new_parent, after, principal) {
                Ok(out) => ok_json(&out),
                Err(m) => err(m),
            }
        })
        .await
    }

    #[tool(
        description = "Set a doc's lifecycle status THROUGH THE GATE: draft | in-review | decided | superseded, or omit/\"null\" to clear. Applied now as a flagged yellow; decline restores the previous status."
    )]
    async fn set_status(
        &self,
        Parameters(p): Parameters<SetStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let status = match crate::docops::parse_status(p.status.as_deref()) {
            Ok(s) => s,
            Err(m) => return err(m),
        };
        let principal = self.principal();
        with_store(&self.store, move |store| {
            match crate::docops::set_status(store, doc_id, status, principal) {
                Ok(out) => ok_json(&out),
                Err(m) => err(m),
            }
        })
        .await
    }

    #[tool(
        description = "Propose trashing a doc and its subtree. ALWAYS RED: nothing happens until a human accepts the card in the review queue (\"<you> wants to trash 'Title' (N docs)\"); then it goes to the Trash, restorable. Refused when any doc in the subtree is shared with you or in a live session. To de-duplicate two docs use merge_docs instead."
    )]
    async fn delete_doc(
        &self,
        Parameters(p): Parameters<DeleteDocParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let (hot, principal) = (self.hot.clone(), self.principal());
        with_store(&self.store, move |store| {
            match crate::docops::delete(store, &|d| hot.is_hot(d), doc_id, principal) {
                Ok(out) => ok_json(&out),
                Err(m) => err(m),
            }
        })
        .await
    }

    #[tool(
        description = "Merge two docs (e.g. same-title daily-doc duplicates): from_doc_id's content blocks are appended after into_doc_id's last block as flagged yellows (from's frontmatter is not carried), then a RED delete_doc(from) is parked for a human to accept. Both docs must be yours and cold. Returns {into: verdicts, delete: verdict, note}."
    )]
    async fn merge_docs(
        &self,
        Parameters(p): Parameters<MergeDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        let from = match parse_uuid(&p.from_doc_id, "from_doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let into = match parse_uuid(&p.into_doc_id, "into_doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let (hot, principal) = (self.hot.clone(), self.principal());
        with_store(&self.store, move |store| {
            match crate::docops::merge(store, &|d| hot.is_hot(d), from, into, principal) {
                Ok(out) => ok_json(&out),
                Err(m) => err(m),
            }
        })
        .await
    }

    #[tool(
        description = "Create a doc — optionally with its content in the same call (markdown: the whole doc). if_exists 'reuse' makes it an atomic find-or-create: a live doc with this exact title under the same parent is returned (reused: true) instead of creating a duplicate — use it for daily docs and other well-known titles. Default 'error' refuses a duplicate title. Returns {id, title, parent_id, epoch, reused}."
    )]
    async fn create_doc(
        &self,
        Parameters(p): Parameters<CreateDocParams>,
    ) -> Result<CallToolResult, McpError> {
        let parent = match p
            .parent_doc_id
            .as_deref()
            .map(|s| parse_uuid(s, "parent_doc_id"))
            .transpose()
        {
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
        let principal = self.principal();
        with_store(&self.store, move |store| {
            // one lock for the whole find-or-create: two sessions racing on
            // the same daily title cannot both create
            let existing = match store.list_docs() {
                Ok(docs) => docs
                    .into_iter()
                    .find(|d| d.parent_id == parent && d.title == title),
                Err(e) => return err(e.to_string()),
            };
            if let Some(d) = existing {
                if reuse {
                    return ok_json(&json!({
                        "id": d.id, "title": d.title, "parent_id": d.parent_id,
                        "epoch": d.current_epoch, "reused": true,
                    }));
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
                Ok((d, _)) => ok_json(&json!({
                    "id": d.id, "title": d.title, "parent_id": d.parent_id,
                    "epoch": d.current_epoch, "reused": false,
                })),
                Err(e) => err(e.to_string()),
            }
        })
        .await
    }
}

#[tool_handler]
impl ServerHandler for KsMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Grimoire: docs as block trees behind a review gate. Everything you write \
             gets a verdict: green = applied, yellow = applied + flagged for a human \
             (revertible), red = parked until a human accepts.\n\
             The loop: identify(name) once per session → find_doc(query) to locate a doc \
             (or tree() for the shape; list_docs only for one small subtree) → \
             read_doc(doc_id, mode 'markdown') → edit the markdown → propose_markdown \
             (doc_id, that read's epoch, markdown) → read the verdicts. If told \
             stale_base: diff_since, re-read, re-propose. Use propose for a single-block \
             change (insert block_id is optional).\n\
             Find-or-create: create_doc(title, parent_doc_id, markdown, if_exists 'reuse') \
             returns the existing doc instead of a duplicate.\n\
             Tree ops carry verdicts too: rename_doc / move_doc / set_status are yellow \
             (applied, flagged, declinable); delete_doc is always red (parked until a \
             human trashes it); merge_docs = yellow append + red delete.\n\
             Human-only, never over MCP: review policy, shares, trust, gardeners, hub \
             roles, profile. my_proposals shows what happened to yours; review_queue \
             lists what awaits a human.",
        )
    }
}

/// Largest `/mcp` request body, matching `propose_markdown` on the HTTP API:
/// a whole doc's markdown can be megabytes. rmcp's own default is 4 MB, which
/// silently truncated a big `propose_markdown` long before the axum layer.
pub const MAX_MCP_BODY: usize = 16 * 1024 * 1024;

pub fn router(
    store: Arc<Mutex<SqliteStore>>,
    agent: Uuid,
    hot: crate::hot::HotState,
    dedupe: DedupeCache,
) -> axum::Router {
    let service = StreamableHttpService::new(
        move || Ok(KsMcp::new(store.clone(), agent, dedupe.clone(), hot.clone())),
        LocalSessionManager::default().into(),
        rmcp::transport::streamable_http_server::tower::StreamableHttpServerConfig::default()
            .with_max_request_body_bytes(MAX_MCP_BODY),
    );
    // rmcp reads the body itself, so neither axum's DefaultBodyLimit nor a
    // tower-http layer gets a say: rmcp's own cap is the one that applies and
    // it enforces it while streaming, answering 413. A tower-http layer here
    // only turned that 413 into a 500 on a body with no Content-Length.
    axum::Router::new().nest_service("/mcp", service)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// fires on `/mcp`: the 16 MB cap is a tower-http layer. Over it the
    /// request is rejected before the service sees it.
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
        let hot = crate::hot::HotState::new(
            std::env::temp_dir().join(format!("grimoire-mcp-test-{}", Uuid::now_v7())),
        );
        let app = router(Arc::new(Mutex::new(store)), agent, hot, new_dedupe());

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
}
