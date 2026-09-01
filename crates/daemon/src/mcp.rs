//! MCP tools over streamable HTTP (tickets 3.1–3.6, 3.7, #52/#53).
//!
//! All writes act as the `claude` agent principal and go through the propose
//! gate — the MCP surface has no direct-write path by design.

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
pub type DedupeCache = Arc<Mutex<std::collections::HashMap<(Uuid, Uuid), serde_json::Value>>>;

pub fn dedupe_get(cache: &DedupeCache, principal: Uuid, id: Uuid) -> Option<serde_json::Value> {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(principal, id))
        .cloned()
}

pub fn dedupe_put(cache: &DedupeCache, principal: Uuid, id: Uuid, v: serde_json::Value) {
    let mut c = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.len() >= 512 {
        c.clear(); // crude LRU: dedupe only needs to survive the retry window
    }
    c.insert((principal, id), v);
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
    /// "outline" (default: block ids + first lines, token-cheap) or "full".
    pub mode: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadBlockParams {
    /// Block UUID.
    pub block_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeParams {
    /// Doc UUID.
    pub doc_id: String,
    /// The doc epoch your read was based on (from read_doc).
    pub base_epoch: i64,
    /// Ops array. Each op: {"kind": {"op": "insert"|"replace"|"delete"|"move", ...},
    /// "source_refs": ["..."]}. insert: block_id (new UUID you generate), parent_id
    /// (block UUID or null), order_key, block_type, content. replace: target,
    /// content. delete: target. move: target, new_parent, new_order_key.
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
    /// Session name, e.g. "claude:qompass-refactor".
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
        description = "Identify this session with a name (e.g. 'claude:qompass-refactor'). Your writes are then attributed to that principal instead of the shared 'claude' — do this first in any session that writes, so provenance distinguishes concurrent agents."
    )]
    async fn identify(
        &self,
        Parameters(p): Parameters<IdentifyParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = p.name.trim();
        if name.is_empty() || name.len() > 60 {
            return err("name must be 1-60 chars".into());
        }
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let existing = store
            .list_principals()
            .ok()
            .and_then(|ps| ps.into_iter().find(|pr| pr.display_name == name));
        let principal = match existing {
            Some(pr) => pr,
            None => {
                match store.create_principal(grimoire_store::PrincipalKind::Agent, name, None) {
                    Ok(pr) => pr,
                    Err(e) => return err(e.to_string()),
                }
            }
        };
        *self
            .identity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(principal.id);
        ok_json(&json!({"identified_as": name, "principal": principal.id}))
    }

    #[tool(
        description = "What happened to this session's recent proposals: each op with its verdict, whether its review annotation was accepted/declined/open, and who resolved it. Use to learn from declines."
    )]
    async fn my_proposals(
        &self,
        Parameters(p): Parameters<MyProposalsParams>,
    ) -> Result<CallToolResult, McpError> {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.proposal_outcomes(self.principal(), p.limit.unwrap_or(20) as usize) {
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
    }

    #[tool(description = "All tags with doc counts — the vocabulary.")]
    async fn list_tags(&self) -> Result<CallToolResult, McpError> {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.list_tags() {
            Ok(t) => ok_json(&t),
            Err(e) => err(e.to_string()),
        }
    }

    #[tool(description = "Docs carrying a tag.")]
    async fn docs_by_tag(
        &self,
        Parameters(p): Parameters<DocsByTagParams>,
    ) -> Result<CallToolResult, McpError> {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.docs_by_tag(&p.tag) {
            Ok(d) => ok_json(&d),
            Err(e) => err(e.to_string()),
        }
    }

    #[tool(
        description = "List docs: id, title, parent_id, current_epoch, review_policy. Pass parent_doc_id to list only that subtree — cheaper than the whole corpus."
    )]
    async fn list_docs(
        &self,
        Parameters(p): Parameters<ListDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match p.parent_doc_id.as_deref() {
            Some(root) => match parse_uuid(root, "parent_doc_id") {
                Ok(root) => match store.doc_subtree(root) {
                    Ok(docs) => ok_json(&docs),
                    Err(e) => err(e.to_string()),
                },
                Err(m) => err(m),
            },
            None => match store.list_docs() {
                Ok(docs) => ok_json(&docs),
                Err(e) => err(e.to_string()),
            },
        }
    }

    #[tool(
        description = "Read a doc as blocks. Returns the doc's current epoch — quote it as base_epoch when proposing. mode 'outline' (default) returns block ids, types and first lines within a small token budget; fetch full blocks with read_block or mode 'full'."
    )]
    async fn read_doc(
        &self,
        Parameters(p): Parameters<ReadDocParams>,
    ) -> Result<CallToolResult, McpError> {
        let doc_id = match parse_uuid(&p.doc_id, "doc_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let full = p.mode.as_deref() == Some("full");
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.read_doc(doc_id) {
            Ok(tree) => {
                let mut blocks = Vec::new();
                flatten(&tree.roots, 0, full, &mut blocks);
                ok_json(&json!({
                    "doc": tree.doc,
                    "epoch": tree.doc.current_epoch,
                    "mode": if full { "full" } else { "outline" },
                    "blocks": blocks,
                }))
            }
            Err(e) => err(e.to_string()),
        }
    }

    #[tool(description = "Read one block in full (any block id from read_doc or search).")]
    async fn read_block(
        &self,
        Parameters(p): Parameters<ReadBlockParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = match parse_uuid(&p.block_id, "block_id") {
            Ok(u) => u,
            Err(m) => return err(m),
        };
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.read_block(id) {
            Ok(b) => ok_json(&b),
            Err(e) => err(e.to_string()),
        }
    }

    #[tool(
        description = "Propose block edits through the review gate. Returns per-op structured verdicts: green = applied; yellow = applied, flagged for review; red = parked unapplied (your text is preserved for a reviewer). A stale base_epoch is fine — ops against unchanged blocks still green. Never guess base_epoch: read_doc first and quote its epoch. For inserts, set order_key to \"\" to append after the last sibling, or \"after:<block-uuid>\" to insert after a specific block — the server assigns the real key; never compute keys yourself."
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
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // block ops against a stale base are SCORED per op (the gate's whole
        // point: unchanged targets still green, conflicts yellow/red) — a
        // stale base is not an error here; see propose_markdown for the
        // whole-doc path, where it is.
        match store.propose(doc_id, p.base_epoch, principal, ops) {
            Ok(out) => {
                if let Some(rid) = request_id {
                    dedupe_put(
                        &self.dedupe,
                        principal,
                        rid,
                        serde_json::to_value(&out).unwrap_or_default(),
                    );
                }
                ok_json(&out)
            }
            Err(e) => err(e.to_string()),
        }
    }

    #[tool(
        description = "THE EASY WRITE PATH: hand over a doc's complete new markdown; the server diffs it against the current blocks and proposes minimal ops through the gate — unchanged blocks keep their ids (provenance and comment anchors survive), edits become replaces, new/removed paragraphs become inserts/deletes. Read the doc (mode 'full'), edit the markdown, send it back. Prefer this over hand-built block ops for anything beyond a single-block change."
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
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                        &self.dedupe,
                        principal,
                        rid,
                        serde_json::to_value(&out).unwrap_or_default(),
                    );
                }
                ok_json(&out)
            }
            Err(e) => err(e.to_string()),
        }
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
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.ops_since(doc_id, p.since_epoch) {
            Ok(ops) => ok_json(&ops),
            Err(e) => err(e.to_string()),
        }
    }

    #[tool(
        description = "Search live block content (substring). Hits are blocks with their doc title — the editable unit, not whole pages."
    )]
    async fn search(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.search_blocks(&p.query, p.limit.unwrap_or(20) as usize) {
            Ok(hits) => ok_json(&hits),
            Err(e) => err(e.to_string()),
        }
    }

    #[tool(
        description = "Open review annotations (applied-but-flagged yellows, parked reds) with their ops, oldest first."
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
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.review_queue(doc_id) {
            Ok(q) => ok_json(&q),
            Err(e) => err(e.to_string()),
        }
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
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(doc) = crate::hot::annotation_doc(&store, id)
            && let Err(m) = self.hot.assert_cold(doc)
        {
            return err(m);
        }
        match store.resolve(id, self.principal(), decision) {
            Ok(receipt) => ok_json(&json!({ "resolved": true, "receipt": receipt })),
            Err(e) => err(e.to_string()),
        }
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
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.backlinks(doc_id) {
            Ok(hits) => ok_json(&hits),
            Err(e) => err(e.to_string()),
        }
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
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.add_comment(block_id, self.principal(), &p.text, reply_to) {
            Ok(c) => ok_json(&c),
            Err(e) => err(e.to_string()),
        }
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
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.list_comments(block_id) {
            Ok(c) => ok_json(&c),
            Err(e) => err(e.to_string()),
        }
    }

    #[tool(description = "Create a new empty doc; add content with propose.")]
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
        let mut store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match store.create_doc(&p.title, parent, self.principal()) {
            Ok(doc) => ok_json(&doc),
            Err(e) => err(e.to_string()),
        }
    }
}

#[tool_handler]
impl ServerHandler for KsMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "knowledge-system: docs as block trees with a review gate. Read \
                 (read_doc returns the epoch), then propose edits quoting that epoch \
                 as base_epoch. Verdicts: green applied, yellow applied+flagged, red \
                 parked for review. If told stale_base, call diff_since and re-propose.",
        )
    }
}

pub fn router(
    store: Arc<Mutex<SqliteStore>>,
    agent: Uuid,
    hot: crate::hot::HotState,
) -> axum::Router {
    let dedupe: DedupeCache = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let service = StreamableHttpService::new(
        move || Ok(KsMcp::new(store.clone(), agent, dedupe.clone(), hot.clone())),
        LocalSessionManager::default().into(),
        Default::default(),
    );
    axum::Router::new().nest_service("/mcp", service)
}
