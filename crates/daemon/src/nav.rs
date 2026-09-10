//! Agent navigation (AX slice A): fuzzy doc lookup over titles and
//! breadcrumb paths. Pure functions over the doc list so they are
//! unit-testable without a store; the MCP tools in `mcp.rs` are thin
//! wrappers (`find_doc`, and the `doc <id> · epoch · path` header of `read_doc`).

use grimoire_store::{Doc, DocStatus};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Breadcrumb separator in paths (`Folder › Sub › Title`).
pub const CRUMB: &str = " › ";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DocMatch {
    pub id: Uuid,
    pub title: String,
    /// `Folder › Sub › Title`.
    pub path: String,
    pub parent_id: Option<Uuid>,
    pub epoch: i64,
    pub status: Option<DocStatus>,
}

/// `id → "Root › … › Title"` for every doc (cycles and orphans stop at the
/// first missing parent).
pub fn breadcrumbs(docs: &[Doc]) -> HashMap<Uuid, String> {
    let by_id: HashMap<Uuid, &Doc> = docs.iter().map(|d| (d.id, d)).collect();
    docs.iter()
        .map(|d| {
            let mut parts = vec![d.title.as_str()];
            let mut seen = HashSet::from([d.id]);
            let mut cur = d.parent_id;
            while let Some(p) = cur
                && seen.insert(p)
                && let Some(pd) = by_id.get(&p)
            {
                parts.push(pd.title.as_str());
                cur = pd.parent_id;
            }
            parts.reverse();
            (d.id, parts.join(CRUMB))
        })
        .collect()
}

/// Live docs in `root`'s subtree, root included (order preserved).
fn subtree<'a>(docs: &'a [Doc], root: Uuid) -> Vec<&'a Doc> {
    let mut children: HashMap<Option<Uuid>, Vec<&Doc>> = HashMap::new();
    for d in docs {
        children.entry(d.parent_id).or_default().push(d);
    }
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(d) = docs.iter().find(|d| d.id == id) {
            out.push(d);
        }
        if let Some(kids) = children.get(&Some(id)) {
            stack.extend(kids.iter().rev().map(|k| k.id));
        }
    }
    out
}

fn trigrams(s: &str) -> HashSet<String> {
    let padded: Vec<char> = format!("  {s} ").chars().collect();
    padded.windows(3).map(|w| w.iter().collect()).collect()
}

/// Share of the query's trigrams present in the candidate: 1.0 = every
/// trigram of the query occurs in the title.
fn trigram_score(query: &str, candidate: &str) -> f64 {
    let q = trigrams(query);
    if q.is_empty() {
        return 0.0;
    }
    let c = trigrams(candidate);
    q.intersection(&c).count() as f64 / q.len() as f64
}

/// Minimum trigram overlap for a fuzzy hit (typos, word order).
const FUZZY_MIN: f64 = 0.45;

/// Rank tier (lower = better) and a within-tier score (lower = better).
fn score(query: &str, title: &str, path: &str) -> Option<(u8, f64)> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    let t = title.to_lowercase();
    let p = path.to_lowercase();
    if t == q {
        return Some((0, 0.0));
    }
    if t.starts_with(&q) {
        return Some((1, t.len() as f64));
    }
    if let Some(pos) = t.find(&q) {
        return Some((2, pos as f64 + t.len() as f64 / 1000.0));
    }
    // every query word somewhere in the path ("daily 2026-09" → Daily › 2026-09-08)
    let words: Vec<&str> = q.split_whitespace().collect();
    if words.iter().all(|w| p.contains(w)) {
        return Some((3, p.len() as f64));
    }
    let sim = trigram_score(&q, &t);
    (sim >= FUZZY_MIN).then_some((4, 1.0 - sim))
}

/// Fuzzy match `query` against titles and breadcrumb paths. Case-insensitive;
/// exact title > prefix > substring > all-words-in-path > trigram fuzzy.
/// `parent` restricts to that subtree (root included).
pub fn find_docs(docs: &[Doc], query: &str, parent: Option<Uuid>, limit: usize) -> Vec<DocMatch> {
    let crumbs = breadcrumbs(docs);
    let candidates: Vec<&Doc> = match parent {
        Some(p) => subtree(docs, p),
        None => docs.iter().collect(),
    };
    let mut scored: Vec<((u8, f64), &Doc)> = candidates
        .into_iter()
        .filter_map(|d| {
            let path = crumbs.get(&d.id).map(String::as_str).unwrap_or(&d.title);
            score(query, &d.title, path).map(|s| (s, d))
        })
        .collect();
    scored.sort_by(|a, b| {
        a.0.0
            .cmp(&b.0.0)
            .then(a.0.1.partial_cmp(&b.0.1).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.1.title.len().cmp(&b.1.title.len()))
            .then_with(|| a.1.title.cmp(&b.1.title))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, d)| DocMatch {
            id: d.id,
            title: d.title.clone(),
            path: crumbs.get(&d.id).cloned().unwrap_or_else(|| d.title.clone()),
            parent_id: d.parent_id,
            epoch: d.current_epoch,
            status: d.status,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(title: &str, parent: Option<Uuid>) -> Doc {
        Doc {
            id: Uuid::now_v7(),
            parent_id: parent,
            title: title.into(),
            review_policy: None,
            current_epoch: 3,
            created_by: Uuid::nil(),
            status: None,
            sort_key: Some("i".into()),
        }
    }

    /// Daily › 2026-09-08, Grimoire › Roadmap, Grimoire › Review Gate, Gardeners (root)
    fn corpus() -> Vec<Doc> {
        let daily = doc("Daily", None);
        let day = doc("2026-09-08", Some(daily.id));
        let grim = doc("Grimoire", None);
        let roadmap = doc("Roadmap", Some(grim.id));
        let gate = doc("Review Gate", Some(grim.id));
        let gardeners = doc("Gardeners", None);
        vec![daily, day, grim, roadmap, gate, gardeners]
    }

    #[test]
    fn breadcrumbs_join_titles_root_first() {
        let docs = corpus();
        let c = breadcrumbs(&docs);
        assert_eq!(c[&docs[1].id], "Daily › 2026-09-08");
        assert_eq!(c[&docs[5].id], "Gardeners");
    }

    #[test]
    fn find_ranks_exact_prefix_substring_path_fuzzy() {
        let docs = corpus();
        let hits = find_docs(&docs, "roadmap", None, 8);
        assert_eq!(hits[0].title, "Roadmap");
        assert_eq!(hits[0].path, "Grimoire › Roadmap");
        assert_eq!(hits[0].epoch, 3);
        // prefix beats substring
        let hits = find_docs(&docs, "gr", None, 8);
        assert_eq!(hits[0].title, "Grimoire");
        // words spread over the path
        let hits = find_docs(&docs, "daily 2026", None, 8);
        assert_eq!(hits[0].title, "2026-09-08", "path match outranks a fuzzy 'Daily': {hits:?}");
        // typo → trigram fuzzy
        let hits = find_docs(&docs, "gardners", None, 8);
        assert_eq!(hits.first().map(|h| h.title.as_str()), Some("Gardeners"));
        // nothing → empty, never an error
        assert!(find_docs(&docs, "zzzz", None, 8).is_empty());
        assert!(find_docs(&docs, "   ", None, 8).is_empty());
    }

    #[test]
    fn find_respects_parent_and_limit() {
        let docs = corpus();
        let grim = docs[2].id;
        let hits = find_docs(&docs, "r", Some(grim), 8);
        let titles: Vec<&str> = hits.iter().map(|h| h.title.as_str()).collect();
        assert!(titles.contains(&"Roadmap") && titles.contains(&"Review Gate"));
        assert!(!titles.contains(&"Gardeners"), "outside the subtree");
        assert_eq!(find_docs(&docs, "r", None, 1).len(), 1);
    }
}

/// Indented text tree: one line per doc, `- Title  [id]`, children indented
/// two spaces, subtrees below `depth` collapsed to `(N more)`. With a root,
/// the root is the first line at depth 0; without, the corpus roots are.
pub fn render_tree(docs: &[Doc], root: Option<Uuid>, depth: usize) -> String {
    let mut children: HashMap<Option<Uuid>, Vec<&Doc>> = HashMap::new();
    for d in docs {
        children.entry(d.parent_id).or_default().push(d);
    }
    fn count(children: &HashMap<Option<Uuid>, Vec<&Doc>>, id: Uuid, seen: &mut HashSet<Uuid>) -> usize {
        if !seen.insert(id) {
            return 0;
        }
        children
            .get(&Some(id))
            .map(|kids| kids.iter().map(|k| 1 + count(children, k.id, seen)).sum())
            .unwrap_or(0)
    }
    fn line(out: &mut String, d: &Doc, level: usize, more: usize) {
        out.push_str(&"  ".repeat(level));
        out.push_str("- ");
        out.push_str(&d.title);
        out.push_str("  [");
        out.push_str(&d.id.to_string());
        out.push(']');
        if more > 0 {
            out.push_str(&format!("  ({more} more)"));
        }
        out.push('\n');
    }
    fn walk(
        out: &mut String,
        children: &HashMap<Option<Uuid>, Vec<&Doc>>,
        parent: Option<Uuid>,
        level: usize,
        depth: usize,
        seen: &mut HashSet<Uuid>,
    ) {
        let Some(kids) = children.get(&parent) else { return };
        for d in kids {
            if !seen.insert(d.id) {
                continue;
            }
            let collapsed = level + 1 >= depth;
            let more = if collapsed {
                count(children, d.id, &mut HashSet::new())
            } else {
                0
            };
            line(out, d, level, more);
            if !collapsed {
                walk(out, children, Some(d.id), level + 1, depth, seen);
            }
        }
    }
    let depth = depth.max(1);
    let mut out = String::new();
    let mut seen = HashSet::new();
    match root {
        Some(r) => {
            let Some(d) = docs.iter().find(|d| d.id == r) else {
                return format!("doc {r} not found\n");
            };
            seen.insert(r);
            line(&mut out, d, 0, 0);
            walk(&mut out, &children, Some(r), 1, depth + 1, &mut seen);
        }
        None => walk(&mut out, &children, None, 0, depth, &mut seen),
    }
    out
}

