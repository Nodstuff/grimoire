//! Agent-facing retrieval (the "AX" search slice): the fewest bytes that
//! answer the question.
//!
//! - `search`  — ranked hybrid (keyword + trigram + dense, reciprocal-rank
//!   fused) with an exact-phrase tier on top, so `"title bar"` outranks the
//!   trigram noise that made `entitlement` a hit. Compact hits: id, path,
//!   ≤200-char snippet, score. `kind: docs` groups by doc.
//! - `grep`    — exhaustive regex over live block content, grouped by doc
//!   with totals and a `truncated` flag. One compiled regex, one pass.
//! - `related` — from a known block/doc: backlinks, dense neighbours, folder
//!   siblings, each tagged with `why`.
//! - `orient`  — the map: a subtree's tree to depth 2 with counts, its most
//!   linked docs with their first paragraph, its tags. Token-budgeted.
//!
//! Ask-the-vault (`ask::retrieve`) is untouched: this module composes its
//! public legs (`retrieve_keyword_with`, the dense cut constants) with its
//! own fusion, filters and tiers, so the answer path's ranking cannot drift.

use crate::embed::Embedder;
use grimoire_store::{BlockStore, BlockType, Doc, SearchHit, SqliteStore};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Snippet width (chars) around the match.
pub const SNIPPET_CHARS: usize = 200;
/// Per-word candidate pool for the keyword leg: deeper than ask's 20 because
/// a scope filter runs AFTER retrieval and must not starve the result.
const POOL_UNSCOPED: usize = 40;
const POOL_SCOPED: usize = 200;
/// Dense candidates considered before the cosine cut.
const DENSE_POOL: usize = 60;

// ─── MCP parameter types (kept here so mcp.rs only grows by the tool fns) ───

#[derive(Deserialize, JsonSchema)]
pub struct SearchParams {
    /// What to find: a phrase, a few keywords, or a question.
    pub query: String,
    /// Restrict to this doc and its descendants (UUID).
    pub scope_doc_id: Option<String>,
    /// Max hits (default 10).
    pub limit: Option<u32>,
    /// "blocks" (default: one hit per block) or "docs" (grouped by doc).
    pub kind: Option<String>,
    /// Skip earlier ask-the-vault answers (default true).
    pub exclude_answers: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GrepParams {
    /// Rust regex (no lookaround/backreferences), matched per line of block content.
    pub pattern: String,
    /// Restrict to this doc and its descendants (UUID).
    pub scope_doc_id: Option<String>,
    /// Default false.
    pub case_insensitive: Option<bool>,
    /// Max docs returned (default 20); `truncated` says if more matched.
    pub max_groups: Option<u32>,
    /// Max matching lines shown per doc (default 5); `match_count` is the full number.
    pub max_matches_per_group: Option<u32>,
    /// Also search comments and frontmatter (default false).
    pub include_hidden: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RelatedParams {
    /// Start from this block (UUID) — or pass doc_id instead.
    pub block_id: Option<String>,
    /// Start from this doc (UUID).
    pub doc_id: Option<String>,
    /// Max entries per relation (default 8).
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OrientParams {
    /// Map this doc's subtree; omit for the whole corpus.
    pub root_doc_id: Option<String>,
    /// Approximate output budget in tokens (default 1500; 1 token ≈ 4 chars).
    pub max_tokens: Option<u32>,
}

// ─── shared: doc map and paths ───

/// All live docs by id — one `list_docs` per call, for paths and siblings.
pub struct DocMap {
    docs: HashMap<Uuid, Doc>,
}

impl DocMap {
    pub fn load(store: &SqliteStore) -> Self {
        let docs = store
            .list_docs()
            .unwrap_or_default()
            .into_iter()
            .map(|d| (d.id, d))
            .collect();
        Self { docs }
    }

    pub fn get(&self, id: Uuid) -> Option<&Doc> {
        self.docs.get(&id)
    }

    /// `Folder › Sub › Title`; a doc whose ancestry is missing shows what it has.
    pub fn path(&self, id: Uuid) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(cid) = cur
            && parts.len() < 32
        {
            match self.docs.get(&cid) {
                Some(d) => {
                    parts.push(d.title.clone());
                    cur = d.parent_id;
                }
                None => break,
            }
        }
        parts.reverse();
        parts.join(" › ")
    }

    /// Live children of `parent` (None = roots) in sidebar order.
    pub fn children(&self, parent: Option<Uuid>) -> Vec<&Doc> {
        let mut out: Vec<&Doc> = self.docs.values().filter(|d| d.parent_id == parent).collect();
        out.sort_by(|a, b| {
            a.sort_key
                .is_none()
                .cmp(&b.sort_key.is_none())
                .then_with(|| a.sort_key.cmp(&b.sort_key))
                .then_with(|| a.title.cmp(&b.title))
        });
        out
    }

    fn descendant_count(&self, id: Uuid) -> usize {
        let mut n = 0;
        let mut stack = vec![id];
        let mut seen = HashSet::new();
        while let Some(cur) = stack.pop() {
            for d in self.docs.values().filter(|d| d.parent_id == Some(cur)) {
                if seen.insert(d.id) {
                    n += 1;
                    stack.push(d.id);
                }
            }
        }
        n
    }
}

fn scope_set(store: &SqliteStore, scope: Option<Uuid>) -> Result<Option<HashSet<Uuid>>, String> {
    match scope {
        None => Ok(None),
        Some(root) => {
            store.get_doc(root).map_err(|e| format!("scope_doc_id: {e}"))?;
            let ids = store.doc_subtree_ids(root).map_err(|e| e.to_string())?;
            Ok(Some(ids.into_iter().collect()))
        }
    }
}

fn is_hidden(h: &SearchHit) -> bool {
    h.block.block_type == BlockType::Comment
        || h.block.block_type == BlockType::CanvasScene
        || grimoire_store::import::is_frontmatter(&h.block.content)
}

/// One line, whitespace collapsed, ≤ `max` chars with an ellipsis.
fn one_line(s: &str, max: usize) -> String {
    let joined: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = joined.chars().take(max).collect();
    if joined.chars().count() > max {
        out.push('…');
    }
    out
}

/// Per-char lowercase that keeps a 1:1 alignment with the source (the
/// first lowercase form of each char), so an index into it is an index
/// into the original.
fn lower_aligned(s: &str) -> Vec<char> {
    s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

fn find_chars(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// ≤ SNIPPET_CHARS around the first occurrence of `phrase`, else of the
/// first of `words`, else the head of the block.
pub fn snippet(content: &str, phrase: &str, words: &[String]) -> String {
    let flat: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= SNIPPET_CHARS {
        return flat;
    }
    let lc = lower_aligned(&flat);
    let phrase_lc: Vec<char> = phrase.to_lowercase().chars().collect();
    let mut at = find_chars(&lc, &phrase_lc);
    if at.is_none() {
        for w in words {
            let wl: Vec<char> = w.chars().collect();
            if let Some(i) = find_chars(&lc, &wl) {
                at = Some(i);
                break;
            }
        }
    }
    let at = at.unwrap_or(0);
    let start = at.saturating_sub(SNIPPET_CHARS / 3).min(chars.len().saturating_sub(SNIPPET_CHARS));
    let end = (start + SNIPPET_CHARS).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

// ─── search ───

#[derive(Clone, Copy, Debug)]
pub struct SearchOpts {
    pub scope: Option<Uuid>,
    pub exclude_answers: bool,
    pub limit: usize,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            scope: None,
            exclude_answers: true,
            limit: 10,
        }
    }
}

#[derive(Serialize, Debug, Clone)]
pub struct BlockHit {
    pub doc_id: Uuid,
    pub doc_title: String,
    pub path: String,
    pub block_id: Uuid,
    pub block_type: &'static str,
    pub snippet: String,
    /// ≥2 exact phrase in block or title; ≥1 every word present whole; else
    /// the fused rank score alone (<0.1).
    pub score: f32,
}

#[derive(Serialize, Debug, Clone)]
pub struct DocHit {
    pub doc_id: Uuid,
    pub title: String,
    pub path: String,
    pub best_snippet: String,
    pub hits: usize,
    pub score: f32,
}

/// A ranked, filtered hit with its score — the shared core of both `kind`s
/// and of the HTTP endpoint.
pub struct Ranked {
    pub hit: SearchHit,
    pub score: f32,
}

/// Reciprocal rank fusion (k = 60) keeping the fused score.
fn rrf_scored(lists: &[Vec<SearchHit>]) -> Vec<(f32, SearchHit)> {
    let mut score: HashMap<Uuid, (f32, SearchHit)> = HashMap::new();
    let mut order: Vec<Uuid> = Vec::new();
    for list in lists {
        for (rank, h) in list.iter().enumerate() {
            let e = score.entry(h.block.id).or_insert_with(|| {
                order.push(h.block.id);
                (0.0, h.clone())
            });
            e.0 += 1.0 / (60.0 + rank as f32 + 1.0);
        }
    }
    let mut out: Vec<(f32, SearchHit)> = order.into_iter().filter_map(|id| score.remove(&id)).collect();
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Whole-word containment: `title` is in "title bar" but not in "entitlement".
fn has_word(lc_content: &str, word: &str) -> bool {
    lc_content
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .any(|t| t == word)
}

/// Query words for the whole-word tier: ask's keywords when it yields any,
/// else the raw lowercase tokens (short queries like "ax" or "mcp gate").
fn query_words(q: &str) -> Vec<String> {
    let kw = crate::ask::keywords(q);
    if !kw.is_empty() {
        return kw;
    }
    q.split_whitespace().map(|w| w.to_lowercase()).collect()
}

pub fn search_ranked(store: &SqliteStore, embedder: Option<&Embedder>, query: &str, opts: SearchOpts) -> Result<Vec<Ranked>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let scope = scope_set(store, opts.scope)?;
    let answers = if opts.exclude_answers { crate::ask::answers_folder_id(store) } else { None };
    let usable = |h: &SearchHit| {
        !is_hidden(h)
            && scope.as_ref().is_none_or(|s| s.contains(&h.block.doc_id))
            && !crate::ask::under_answers(store, h.block.doc_id, answers)
    };
    let pool = if scope.is_some() { POOL_SCOPED } else { POOL_UNSCOPED };

    let keyword: Vec<SearchHit> = crate::ask::retrieve_keyword_with(store, q, pool)
        .into_iter()
        .filter(usable)
        .collect();
    // the raw trigram leg keeps typo tolerance ("gardnr" → gardener) and
    // short queries the keyword leg drops; it ranks below the tiers above
    let trigram: Vec<SearchHit> = store
        .search_blocks(q, pool)
        .unwrap_or_default()
        .into_iter()
        .filter(usable)
        .collect();
    let mut legs = vec![keyword, trigram];
    if let Some(emb) = embedder {
        let scored = emb.search(q, DENSE_POOL);
        let top = scored.first().map(|(_, s)| *s).unwrap_or(0.0);
        let ids: Vec<Uuid> = scored
            .into_iter()
            .filter(|(_, s)| *s >= crate::ask::DENSE_FLOOR && *s >= top * crate::ask::DENSE_RELATIVE)
            .map(|(id, _)| id)
            .collect();
        let dense: Vec<SearchHit> = store
            .blocks_as_hits(&ids)
            .unwrap_or_default()
            .into_iter()
            .filter(usable)
            .collect();
        legs.insert(0, dense);
    }
    let fused = rrf_scored(&legs);

    let phrase = q.to_lowercase();
    let words = query_words(q);
    let mut tiered: Vec<(u8, f32, SearchHit)> = fused
        .into_iter()
        .map(|(rrf, h)| {
            let lc = h.block.content.to_lowercase();
            let tier = if lc.contains(&phrase) || h.doc_title.to_lowercase().contains(&phrase) {
                2
            } else if !words.is_empty() && words.iter().all(|w| has_word(&lc, w)) {
                1
            } else {
                0
            };
            (tier, rrf, h)
        })
        .collect();
    tiered.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.2.block.content.len().cmp(&b.2.block.content.len()))
    });
    Ok(tiered
        .into_iter()
        .map(|(tier, rrf, hit)| Ranked {
            hit,
            score: ((tier as f32 + rrf) * 10_000.0).round() / 10_000.0,
        })
        .collect())
}

pub fn search_blocks(store: &SqliteStore, embedder: Option<&Embedder>, query: &str, opts: SearchOpts) -> Result<Vec<BlockHit>, String> {
    let ranked = search_ranked(store, embedder, query, opts)?;
    let docs = DocMap::load(store);
    let words = query_words(query);
    Ok(ranked
        .into_iter()
        .take(opts.limit)
        .map(|r| BlockHit {
            doc_id: r.hit.block.doc_id,
            path: docs.path(r.hit.block.doc_id),
            doc_title: r.hit.doc_title,
            block_id: r.hit.block.id,
            block_type: r.hit.block.block_type.as_str(),
            snippet: snippet(&r.hit.block.content, query.trim(), &words),
            score: r.score,
        })
        .collect())
}

pub fn search_docs(store: &SqliteStore, embedder: Option<&Embedder>, query: &str, opts: SearchOpts) -> Result<Vec<DocHit>, String> {
    let ranked = search_ranked(store, embedder, query, opts)?;
    let docs = DocMap::load(store);
    let words = query_words(query);
    let mut out: Vec<DocHit> = Vec::new();
    for r in ranked {
        if let Some(d) = out.iter_mut().find(|d| d.doc_id == r.hit.block.doc_id) {
            d.hits += 1;
            continue;
        }
        if out.len() >= opts.limit {
            // still count hits for docs already listed; new docs are over budget
            continue;
        }
        out.push(DocHit {
            doc_id: r.hit.block.doc_id,
            path: docs.path(r.hit.block.doc_id),
            title: r.hit.doc_title,
            best_snippet: snippet(&r.hit.block.content, query.trim(), &words),
            hits: 1,
            score: r.score,
        });
    }
    Ok(out)
}

// ─── grep ───

#[derive(Clone, Copy, Debug)]
pub struct GrepOpts {
    pub scope: Option<Uuid>,
    pub case_insensitive: bool,
    pub max_groups: usize,
    pub max_matches_per_group: usize,
    pub include_hidden: bool,
}

impl Default for GrepOpts {
    fn default() -> Self {
        Self {
            scope: None,
            case_insensitive: false,
            max_groups: 20,
            max_matches_per_group: 5,
            include_hidden: false,
        }
    }
}

#[derive(Serialize, Debug)]
pub struct GrepMatch {
    pub block_id: Uuid,
    /// 1-based line within the block.
    pub line: usize,
    pub text: String,
}

#[derive(Serialize, Debug)]
pub struct GrepGroup {
    pub doc_id: Uuid,
    pub title: String,
    pub path: String,
    /// Matching lines in this doc (all of them, shown or not).
    pub match_count: usize,
    pub matches: Vec<GrepMatch>,
}

#[derive(Serialize, Debug)]
pub struct GrepOut {
    pub total_groups: usize,
    pub total_matches: usize,
    pub truncated: bool,
    pub groups: Vec<GrepGroup>,
}

pub fn grep(store: &SqliteStore, pattern: &str, opts: GrepOpts) -> Result<GrepOut, String> {
    if pattern.is_empty() {
        return Err("pattern is empty".into());
    }
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(opts.case_insensitive)
        .build()
        .map_err(|e| format!("invalid regex: {e}"))?;
    let scope = scope_set(store, opts.scope)?;
    let blocks = store.live_blocks_with_titles().map_err(|e| e.to_string())?;
    let docs = DocMap::load(store);

    let mut groups: Vec<GrepGroup> = Vec::new();
    let mut index: HashMap<Uuid, usize> = HashMap::new();
    let mut total_matches = 0usize;
    for h in blocks.iter() {
        if scope.as_ref().is_some_and(|s| !s.contains(&h.block.doc_id)) || (!opts.include_hidden && is_hidden(h)) {
            continue;
        }
        for (i, line) in h.block.content.lines().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            total_matches += 1;
            let gi = *index.entry(h.block.doc_id).or_insert_with(|| {
                groups.push(GrepGroup {
                    doc_id: h.block.doc_id,
                    title: h.doc_title.clone(),
                    path: docs.path(h.block.doc_id),
                    match_count: 0,
                    matches: Vec::new(),
                });
                groups.len() - 1
            });
            let g = &mut groups[gi];
            g.match_count += 1;
            if g.matches.len() < opts.max_matches_per_group {
                g.matches.push(GrepMatch {
                    block_id: h.block.id,
                    line: i + 1,
                    text: one_line(line, SNIPPET_CHARS),
                });
            }
        }
    }
    groups.sort_by(|a, b| b.match_count.cmp(&a.match_count).then_with(|| a.path.cmp(&b.path)));
    let total_groups = groups.len();
    let mut truncated = groups.iter().any(|g| g.match_count > g.matches.len());
    if groups.len() > opts.max_groups {
        groups.truncate(opts.max_groups);
        truncated = true;
    }
    Ok(GrepOut {
        total_groups,
        total_matches,
        truncated,
        groups,
    })
}

// ─── related ───

#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    Block(Uuid),
    Doc(Uuid),
}

#[derive(Serialize, Debug)]
pub struct Related {
    /// "backlink" | "similar" | "sibling"
    pub why: &'static str,
    pub doc_id: Uuid,
    pub title: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

#[derive(Serialize, Debug)]
pub struct RelatedOut {
    pub anchor: serde_json::Value,
    /// Whether dense "similar" neighbours were computed.
    pub embedder: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub related: Vec<Related>,
}

/// The text a doc "is about" for the dense leg: title plus its first few
/// visible blocks.
fn doc_gist(store: &SqliteStore, doc_id: Uuid) -> String {
    let Ok(tree) = store.read_doc(doc_id) else { return String::new() };
    let mut parts = vec![tree.doc.title.clone()];
    let mut n = 0;
    let mut stack: Vec<&grimoire_store::BlockNode> = tree.roots.iter().rev().collect();
    while let Some(node) = stack.pop() {
        let b = &node.block;
        if b.block_type != BlockType::Comment && !grimoire_store::import::is_frontmatter(&b.content) && !b.content.trim().is_empty() {
            parts.push(b.content.clone());
            n += 1;
            if n >= 3 {
                break;
            }
        }
        for c in node.children.iter().rev() {
            stack.push(c);
        }
    }
    parts.join("\n")
}

pub fn related(store: &SqliteStore, embedder: Option<&Embedder>, anchor: Anchor, limit: usize) -> Result<RelatedOut, String> {
    let (doc_id, anchor_block, text) = match anchor {
        Anchor::Block(id) => {
            let b = store.read_block(id).map_err(|e| format!("block_id: {e}"))?;
            (b.doc_id, Some(b.id), b.content)
        }
        Anchor::Doc(id) => {
            store.get_doc(id).map_err(|e| format!("doc_id: {e}"))?;
            (id, None, doc_gist(store, id))
        }
    };
    let docs = DocMap::load(store);
    let doc = docs.get(doc_id).ok_or_else(|| format!("doc {doc_id} not found"))?;
    let mut out = Vec::new();

    // (a) what links here
    for h in store.backlinks(doc_id).map_err(|e| e.to_string())?.into_iter().filter(|h| !is_hidden(h)).take(limit) {
        out.push(Related {
            why: "backlink",
            doc_id: h.block.doc_id,
            path: docs.path(h.block.doc_id),
            title: h.doc_title,
            block_id: Some(h.block.id),
            snippet: Some(one_line(&h.block.content, SNIPPET_CHARS)),
            score: None,
        });
    }

    // (b) nearest by meaning, outside the anchor's own doc
    let mut note = None;
    match embedder {
        Some(emb) if !text.trim().is_empty() => {
            let scored: Vec<(Uuid, f32)> = emb
                .search(&text, limit + DENSE_POOL)
                .into_iter()
                .filter(|(id, s)| Some(*id) != anchor_block && *s >= crate::ask::DENSE_FLOOR)
                .collect();
            let by_id: HashMap<Uuid, f32> = scored.iter().copied().collect();
            let ids: Vec<Uuid> = scored.iter().map(|(id, _)| *id).collect();
            let hits = store.blocks_as_hits(&ids).unwrap_or_default();
            for h in hits.into_iter().filter(|h| h.block.doc_id != doc_id && !is_hidden(h)).take(limit) {
                out.push(Related {
                    why: "similar",
                    doc_id: h.block.doc_id,
                    path: docs.path(h.block.doc_id),
                    title: h.doc_title,
                    block_id: Some(h.block.id),
                    snippet: Some(one_line(&h.block.content, SNIPPET_CHARS)),
                    score: by_id.get(&h.block.id).map(|s| (s * 1000.0).round() / 1000.0),
                });
            }
        }
        Some(_) => note = Some("anchor has no text to embed; 'similar' omitted".into()),
        None => note = Some("no embedding model loaded in this daemon; 'similar' omitted (backlinks and siblings only)".into()),
    }

    // (c) same folder
    for d in docs.children(doc.parent_id).into_iter().filter(|d| d.id != doc_id).take(limit) {
        out.push(Related {
            why: "sibling",
            doc_id: d.id,
            title: d.title.clone(),
            path: docs.path(d.id),
            block_id: None,
            snippet: None,
            score: None,
        });
    }

    Ok(RelatedOut {
        anchor: serde_json::json!({
            "doc_id": doc_id,
            "title": doc.title,
            "path": docs.path(doc_id),
            "block_id": anchor_block,
        }),
        embedder: embedder.is_some(),
        note,
        related: out,
    })
}

// ─── orient ───

const ORIENT_CHILDREN_SHOWN: usize = 12;
const ORIENT_LINKED_SHOWN: usize = 8;
const ORIENT_TAGS_SHOWN: usize = 30;
const ORIENT_GIST_CHARS: usize = 160;

/// A line-budgeted text writer: refuses lines that would blow the budget
/// and records that it did.
struct Budget {
    out: String,
    limit: usize,
    truncated: bool,
}

impl Budget {
    fn line(&mut self, s: &str) -> bool {
        if self.truncated || self.out.len() + s.len() + 1 > self.limit {
            self.truncated = true;
            return false;
        }
        self.out.push_str(s);
        self.out.push('\n');
        true
    }
}

fn first_paragraph(store: &SqliteStore, doc_id: Uuid) -> Option<String> {
    let tree = store.read_doc(doc_id).ok()?;
    let mut stack: Vec<&grimoire_store::BlockNode> = tree.roots.iter().rev().collect();
    while let Some(node) = stack.pop() {
        let b = &node.block;
        let visible = matches!(b.block_type, BlockType::Paragraph | BlockType::Decision)
            && !grimoire_store::import::is_frontmatter(&b.content)
            && !b.content.trim().is_empty();
        if visible {
            return Some(one_line(&b.content, ORIENT_GIST_CHARS));
        }
        for c in node.children.iter().rev() {
            stack.push(c);
        }
    }
    None
}

pub fn orient(store: &SqliteStore, root: Option<Uuid>, max_tokens: usize) -> Result<String, String> {
    let docs = DocMap::load(store);
    if let Some(r) = root {
        store.get_doc(r).map_err(|e| format!("root_doc_id: {e}"))?;
    }
    let in_scope: HashSet<Uuid> = match root {
        Some(r) => store.doc_subtree_ids(r).map_err(|e| e.to_string())?.into_iter().collect(),
        None => docs.docs.keys().copied().collect(),
    };
    let blocks = store.block_counts().unwrap_or_default();
    let block_total: i64 = in_scope.iter().map(|d| blocks.get(d).copied().unwrap_or(0)).sum();
    let limit = max_tokens.max(100) * 4;
    let note = "… truncated: raise max_tokens or pass a narrower root_doc_id";
    let mut b = Budget {
        out: String::new(),
        limit: limit.saturating_sub(note.len() + 1),
        truncated: false,
    };

    match root {
        Some(r) => b.line(&format!(
            "# {} — {} docs, {} blocks (root {})",
            docs.path(r),
            in_scope.len(),
            block_total,
            r
        )),
        None => b.line(&format!("# Corpus — {} docs, {} blocks", in_scope.len(), block_total)),
    };

    // tree to depth 2
    b.line("");
    b.line("## Tree");
    let entry = |d: &Doc, indent: &str| {
        let below = docs.descendant_count(d.id);
        let n = blocks.get(&d.id).copied().unwrap_or(0);
        let mut s = format!("{indent}- {} · {}", d.title, d.id);
        if below > 0 {
            s.push_str(&format!(" · {below} below"));
        }
        if n > 0 {
            s.push_str(&format!(" · {n} blocks"));
        }
        s
    };
    let top = docs.children(root);
    'tree: for (i, d) in top.iter().enumerate() {
        if i >= ORIENT_CHILDREN_SHOWN {
            b.line(&format!("- … +{} more", top.len() - i));
            break;
        }
        if !b.line(&entry(d, "")) {
            break;
        }
        let kids = docs.children(Some(d.id));
        for (j, k) in kids.iter().enumerate() {
            if j >= ORIENT_CHILDREN_SHOWN {
                b.line(&format!("  - … +{} more", kids.len() - j));
                break;
            }
            if !b.line(&entry(k, "  ")) {
                break 'tree;
            }
        }
    }

    // most linked (inbound doc→doc wikilinks resolved by title)
    let mut inbound: HashMap<Uuid, usize> = HashMap::new();
    for (_, to) in store.raw_links().unwrap_or_default() {
        if let Ok(id) = Uuid::parse_str(&to)
            && in_scope.contains(&id)
        {
            *inbound.entry(id).or_default() += 1;
        }
    }
    let mut linked: Vec<(Uuid, usize)> = inbound.into_iter().collect();
    linked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| docs.path(a.0).cmp(&docs.path(b.0))));
    if !linked.is_empty() && !b.truncated {
        b.line("");
        b.line("## Most linked");
        for (id, n) in linked.iter().take(ORIENT_LINKED_SHOWN) {
            let title = docs.get(*id).map(|d| d.title.as_str()).unwrap_or("?");
            let gist = first_paragraph(store, *id).unwrap_or_default();
            let line = if gist.is_empty() {
                format!("- {title} ← {n} · {id}")
            } else {
                format!("- {title} ← {n} · {id}\n  {gist}")
            };
            if !b.line(&line) {
                break;
            }
        }
    }

    // tags in scope
    let mut tag_counts: HashMap<String, usize> = HashMap::new();
    for (doc, tags) in store.raw_doc_tags().unwrap_or_default() {
        if let Ok(id) = Uuid::parse_str(&doc)
            && in_scope.contains(&id)
        {
            let mut uniq: Vec<String> = tags;
            uniq.sort();
            uniq.dedup();
            for t in uniq {
                *tag_counts.entry(t).or_default() += 1;
            }
        }
    }
    if !tag_counts.is_empty() && !b.truncated {
        let mut tags: Vec<(String, usize)> = tag_counts.into_iter().collect();
        tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        b.line("");
        b.line(&format!("## Tags ({})", tags.len()));
        let shown: Vec<String> = tags.iter().take(ORIENT_TAGS_SHOWN).map(|(t, n)| format!("{t} ({n})")).collect();
        let mut line = shown.join(", ");
        if tags.len() > ORIENT_TAGS_SHOWN {
            line.push_str(&format!(", … +{} more", tags.len() - ORIENT_TAGS_SHOWN));
        }
        b.line(&line);
    }

    let mut out = b.out;
    if b.truncated {
        out.push_str(note);
        out.push('\n');
    }
    Ok(out)
}

// ─── tests ───

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::{PrincipalKind, import::import_markdown};

    fn store() -> (SqliteStore, Uuid) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        (s, tom)
    }

    fn doc(s: &mut SqliteStore, who: Uuid, title: &str, parent: Option<Uuid>, md: &str) -> Uuid {
        import_markdown(s, title, parent, who, md).unwrap().0
    }

    #[test]
    fn phrase_match_outranks_trigram_noise() {
        let (mut s, tom) = store();
        doc(&mut s, tom, "Entitlements", None, "The entitlement check runs at login.\n\nEvery entitlement has a title string and a bar code.\n");
        doc(&mut s, tom, "Shell", None, "Drag the window by its title bar to move it.\n");
        // the raw store search does surface the noise…
        let raw = s.search_blocks("title bar", 20).unwrap();
        assert!(raw.iter().any(|h| h.doc_title == "Entitlements"), "fixture must reproduce the noise");
        // …and the ranked search puts the exact phrase first
        let hits = search_blocks(&s, None, "title bar", SearchOpts::default()).unwrap();
        assert_eq!(hits[0].doc_title, "Shell", "{hits:?}");
        assert!(hits[0].score >= 2.0, "phrase tier: {}", hits[0].score);
        assert!(hits[0].snippet.contains("title bar"));
        assert_eq!(hits[0].path, "Shell");
        // "title string and a bar" has both words whole → tier 1, above the pure noise
        let whole = hits.iter().find(|h| h.snippet.contains("title string")).expect("whole-word hit present");
        let noise = hits.iter().find(|h| h.snippet.contains("check runs")).expect("trigram-only hit present");
        assert!(whole.score >= 1.0 && whole.score < 2.0, "{}", whole.score);
        assert!(noise.score < 1.0, "{}", noise.score);
    }

    #[test]
    fn scope_restricts_to_the_subtree_and_paths_show_ancestry() {
        let (mut s, tom) = store();
        let a = s.create_doc("Alpha", None, tom).unwrap().id;
        let a_sub = s.create_doc("Deep", Some(a), tom).unwrap().id;
        doc(&mut s, tom, "Notes A", Some(a_sub), "The gardener sweeps nightly.\n");
        let b = s.create_doc("Beta", None, tom).unwrap().id;
        doc(&mut s, tom, "Notes B", Some(b), "The gardener sweeps nightly.\n");
        let all = search_blocks(&s, None, "gardener sweeps", SearchOpts::default()).unwrap();
        assert_eq!(all.len(), 2);
        let scoped = search_blocks(&s, None, "gardener sweeps", SearchOpts { scope: Some(a), ..Default::default() }).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].path, "Alpha › Deep › Notes A");
        assert!(search_blocks(&s, None, "x", SearchOpts { scope: Some(Uuid::now_v7()), ..Default::default() }).is_err());
    }

    #[test]
    fn answers_are_excluded_by_default_and_docs_kind_groups() {
        let (mut s, tom) = store();
        doc(&mut s, tom, "Grants", None, "The grant flow uses delegation.\n\nDelegation is scoped per grant.\n");
        let answers = s.create_doc(crate::ask::ANSWERS_FOLDER, None, tom).unwrap().id;
        doc(&mut s, tom, "old answer", Some(answers), "Earlier we said the grant flow uses delegation.\n");
        let hits = search_blocks(&s, None, "grant delegation", SearchOpts::default()).unwrap();
        assert!(hits.iter().all(|h| h.doc_title == "Grants"), "{hits:?}");
        let with = search_blocks(&s, None, "grant delegation", SearchOpts { exclude_answers: false, ..Default::default() }).unwrap();
        assert!(with.iter().any(|h| h.doc_title == "old answer"));
        let docs = search_docs(&s, None, "grant delegation", SearchOpts::default()).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].hits, 2);
        assert_eq!(docs[0].title, "Grants");
        // limit applies to docs, hit counts still accumulate for listed docs
        let limited = search_docs(&s, None, "grant delegation", SearchOpts { exclude_answers: false, limit: 1, ..Default::default() }).unwrap();
        assert_eq!(limited.len(), 1);
    }

    #[test]
    fn short_and_typo_queries_still_hit_via_the_trigram_leg() {
        let (mut s, tom) = store();
        doc(&mut s, tom, "Gardeners", None, "The gardener sweeps nightly.\n");
        assert_eq!(search_blocks(&s, None, "gardnr", SearchOpts::default()).unwrap().len(), 1);
        assert!(search_blocks(&s, None, "", SearchOpts::default()).unwrap().is_empty());
    }

    #[test]
    fn snippet_centres_on_the_match_and_caps_length() {
        let long = format!("{} needle in the middle {}", "hay ".repeat(100), "stack ".repeat(100));
        let snip = snippet(&long, "needle", &[]);
        assert!(snip.contains("needle"));
        assert!(snip.chars().count() <= SNIPPET_CHARS + 2, "{}", snip.chars().count());
        assert!(snip.starts_with('…') && snip.ends_with('…'));
        assert_eq!(snippet("short  text\nhere", "zzz", &[]), "short text here");
    }

    #[test]
    fn grep_groups_by_doc_with_totals_and_truncation() {
        let (mut s, tom) = store();
        let f = s.create_doc("Folder", None, tom).unwrap().id;
        doc(&mut s, tom, "Many", Some(f), "TODO one\nTODO two\nTODO three\n\nTODO four\n");
        doc(&mut s, tom, "One", Some(f), "a TODO here\n");
        doc(&mut s, tom, "None", None, "nothing to see\n");
        let out = grep(&s, r"TODO \w+", GrepOpts { max_matches_per_group: 2, ..Default::default() }).unwrap();
        assert_eq!(out.total_groups, 2);
        assert_eq!(out.total_matches, 5);
        assert!(out.truncated, "Many has 4 matches, 2 shown");
        assert_eq!(out.groups[0].title, "Many");
        assert_eq!(out.groups[0].match_count, 4);
        assert_eq!(out.groups[0].matches.len(), 2);
        assert_eq!(out.groups[0].matches[0].line, 1);
        assert_eq!(out.groups[0].matches[1].text, "TODO two");
        assert_eq!(out.groups[0].path, "Folder › Many");
        assert_eq!(out.groups[1].match_count, 1);

        let capped = grep(&s, "TODO", GrepOpts { max_groups: 1, max_matches_per_group: 10, ..Default::default() }).unwrap();
        assert_eq!(capped.groups.len(), 1);
        assert_eq!(capped.total_groups, 2);
        assert!(capped.truncated);

        let full = grep(&s, "TODO", GrepOpts { max_matches_per_group: 10, ..Default::default() }).unwrap();
        assert!(!full.truncated);

        let ci = grep(&s, "todo", GrepOpts { case_insensitive: true, ..Default::default() }).unwrap();
        assert_eq!(ci.total_matches, 5);
        assert_eq!(grep(&s, "todo", GrepOpts::default()).unwrap().total_matches, 0);
    }

    #[test]
    fn grep_rejects_invalid_regex_and_hides_comments_and_frontmatter_by_default() {
        let (mut s, tom) = store();
        let d = doc(&mut s, tom, "Tagged", None, "---\ntags:\n  - secret\n---\n\nbody text\n");
        let block = s.read_doc(d).unwrap().roots.iter().map(|n| n.block.clone()).find(|b| b.content == "body text").unwrap();
        s.add_comment(block.id, tom, "secret comment", None).unwrap();
        let err = grep(&s, "(unclosed", GrepOpts::default()).unwrap_err();
        assert!(err.starts_with("invalid regex:"), "{err}");
        assert!(grep(&s, "", GrepOpts::default()).is_err());
        assert_eq!(grep(&s, "secret", GrepOpts::default()).unwrap().total_matches, 0);
        assert_eq!(grep(&s, "secret", GrepOpts { include_hidden: true, ..Default::default() }).unwrap().total_matches, 2);
        // scope
        let other = doc(&mut s, tom, "Other", None, "body text elsewhere\n");
        assert_eq!(grep(&s, "body", GrepOpts::default()).unwrap().total_groups, 2);
        assert_eq!(grep(&s, "body", GrepOpts { scope: Some(other), ..Default::default() }).unwrap().total_groups, 1);
    }

    #[test]
    fn related_without_embedder_gives_backlinks_and_siblings_and_says_why() {
        let (mut s, tom) = store();
        let f = s.create_doc("Folder", None, tom).unwrap().id;
        let target = doc(&mut s, tom, "Target", Some(f), "The target doc.\n");
        doc(&mut s, tom, "Sibling", Some(f), "Nothing to do with it.\n");
        doc(&mut s, tom, "Linker", None, "See [[Target]] for details.\n");
        let out = related(&s, None, Anchor::Doc(target), 8).unwrap();
        assert!(!out.embedder);
        assert!(out.note.as_deref().unwrap().contains("similar"));
        let whys: Vec<(&str, &str)> = out.related.iter().map(|r| (r.why, r.title.as_str())).collect();
        assert!(whys.contains(&("backlink", "Linker")), "{whys:?}");
        assert!(whys.contains(&("sibling", "Sibling")), "{whys:?}");
        assert!(!whys.iter().any(|(_, t)| *t == "Target"), "never itself");
        assert!(out.related.iter().all(|r| r.why != "similar"));
        let bl = out.related.iter().find(|r| r.why == "backlink").unwrap();
        assert!(bl.block_id.is_some() && bl.snippet.as_deref().unwrap().contains("[[Target]]"));
        // from a block
        let blk = s.read_doc(target).unwrap().roots[0].block.id;
        let from_block = related(&s, None, Anchor::Block(blk), 8).unwrap();
        assert_eq!(from_block.anchor["block_id"], serde_json::json!(blk));
        assert!(related(&s, None, Anchor::Block(Uuid::now_v7()), 8).is_err());
    }

    #[test]
    fn related_with_embedder_adds_similar_blocks_from_other_docs() {
        let emb = Embedder::load().expect("model compiled in");
        let (mut s, tom) = store();
        let a = doc(&mut s, tom, "Backups", None, "The backup runs nightly with VACUUM INTO a snapshot file.\n\nA second paragraph about the same backup schedule.\n");
        doc(&mut s, tom, "Snapshots", None, "Database snapshots are taken every night and kept for a week.\n");
        doc(&mut s, tom, "Bread", None, "Sourdough wants a long cold proof.\n");
        let shared = std::sync::Arc::new(std::sync::Mutex::new(s));
        emb.embed_stale(&shared).unwrap();
        let s = shared.lock().unwrap();
        let blk = s.read_doc(a).unwrap().roots[0].block.id;
        let out = related(&s, Some(&emb), Anchor::Block(blk), 3).unwrap();
        assert!(out.embedder && out.note.is_none());
        let similar: Vec<&Related> = out.related.iter().filter(|r| r.why == "similar").collect();
        assert!(!similar.is_empty(), "{:?}", out.related);
        assert_eq!(similar[0].title, "Snapshots", "{similar:?}");
        assert!(similar.iter().all(|r| r.doc_id != a), "own doc excluded");
        assert!(similar[0].score.unwrap() >= crate::ask::DENSE_FLOOR);
    }

    #[test]
    fn orient_maps_tree_links_tags_and_respects_the_budget() {
        let (mut s, tom) = store();
        let root = s.create_doc("Root", None, tom).unwrap().id;
        let hub = doc(&mut s, tom, "Hub", Some(root), "---\ntags:\n  - core\n---\n\n# Hub\n\nThe hub is where everything meets.\n");
        for i in 0..5 {
            doc(&mut s, tom, &format!("Leaf {i}"), Some(hub), &format!("---\ntags:\n  - leaf\n---\n\nLeaf {i} links to [[Hub]].\n"));
        }
        doc(&mut s, tom, "Outside", None, "Also links to [[Hub]] but is outside the root.\n");
        let map = orient(&s, Some(root), 1500).unwrap();
        assert!(map.starts_with("# Root — 7 docs"), "{map}");
        assert!(map.contains("- Hub · "), "{map}");
        assert!(map.contains("5 below"), "{map}");
        assert!(map.contains("  - Leaf 0 · "), "{map}");
        assert!(map.contains("## Most linked"), "{map}");
        assert!(map.contains("- Hub ← 6 · "), "{map}");
        assert!(map.contains("The hub is where everything meets."), "{map}");
        assert!(map.contains("## Tags (2)") && map.contains("leaf (5), core (1)"), "{map}");
        assert!(!map.contains("truncated"));

        let small = orient(&s, Some(root), 100).unwrap();
        assert!(small.len() <= 100 * 4 + 1, "{}", small.len());
        assert!(small.contains("truncated"), "{small}");

        let whole = orient(&s, None, 1500).unwrap();
        assert!(whole.starts_with("# Corpus — 8 docs"), "{whole}");
        assert!(whole.contains("- Outside · "));
        assert!(orient(&s, Some(Uuid::now_v7()), 1500).is_err());
    }
}
