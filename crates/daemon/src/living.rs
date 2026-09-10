//! Living answers: an ask-the-vault answer stays true to the blocks it cites.
//!
//! `ask.rs` records every cited block with the epoch it was read at
//! (`answer_sources`) and keeps the question in the answer doc's frontmatter
//! (`question:`). The refresher — the 16:00 daily cut plus
//! `POST /admin/living/refresh` — finds answers whose cited blocks moved on
//! (edited or tombstoned), re-runs retrieval for the stored question and,
//! when Claude Code is around, the synthesis, and lands the new receipts as
//! REVIEWABLE YELLOWS under the `scribe` principal. Only blocks the scribe
//! authored are replaced: a block a human wrote into the answer is never
//! touched. Budget: at most `REFRESH_BUDGET` answers per sweep, the answer
//! whose sources went stale longest ago first.

use crate::store_ext::with_store;
use axum::extract::{Path, State};
use axum::Json;
use grimoire_store::{AnswerSource, BlockNode, BlockStore, DocTree, OpInput, OpKind, SearchHit, SqliteStore, order_key};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use uuid::Uuid;

/// Answers refreshed per sweep — the rest wait for tomorrow's cut.
pub const REFRESH_BUDGET: usize = 10;
/// One synthesis per answer, same clock as a fresh ask.
pub const REFRESH_WALL_CLOCK: Duration = Duration::from_secs(120);

/// The embedder ask-the-vault uses, handed over once at startup so the daily
/// cut (which predates the embedder in `main`) and the admin route retrieve
/// the same way a fresh ask does. Unset = keyword retrieval only.
static EMBEDDER: OnceLock<Option<Arc<crate::embed::Embedder>>> = OnceLock::new();

pub fn set_embedder(e: Option<Arc<crate::embed::Embedder>>) {
    let _ = EMBEDDER.set(e);
}

fn embedder() -> Option<Arc<crate::embed::Embedder>> {
    EMBEDDER.get().cloned().flatten()
}

/// The answer doc's first block: the question, kept where a tagging pass
/// can add to it without losing it (`tag_ops` appends under `---`).
pub fn question_frontmatter(question: &str) -> String {
    let one_line = question.trim().replace(['\n', '\r'], " ");
    format!("---\nquestion: \"{}\"\n---", one_line.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Read the question back: the frontmatter `question:` line, else the H1.
pub fn question_of(tree: &DocTree) -> Option<String> {
    for n in &tree.roots {
        let c = n.block.content.as_str();
        if grimoire_store::import::is_frontmatter(c) {
            for line in c.lines() {
                if let Some(v) = line.strip_prefix("question:") {
                    let v = v.trim();
                    let v = v
                        .strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .map(|s| s.replace("\\\"", "\"").replace("\\\\", "\\"))
                        .unwrap_or_else(|| v.to_string());
                    if !v.is_empty() {
                        return Some(v);
                    }
                }
            }
        }
    }
    tree.roots
        .iter()
        .find_map(|n| n.block.content.strip_prefix("# ").map(|h| h.trim().to_string()))
        .filter(|h| !h.is_empty())
}

/// Record what an answer rests on, at the epochs it was read.
pub fn record_sources(store: &mut SqliteStore, answer: Uuid, excerpts: &[SearchHit]) -> grimoire_store::Result<()> {
    let rows: Vec<(Uuid, i64)> = excerpts.iter().map(|h| (h.block.id, h.block.epoch)).collect();
    store.record_answer_sources(answer, &rows)
}

/// A stale answer: any cited block edited past the epoch it was read at, or gone.
pub fn is_stale(sources: &[AnswerSource]) -> bool {
    sources.iter().any(|s| s.changed)
}

#[derive(Debug, serde::Serialize)]
pub struct LivingStatus {
    pub is_answer: bool,
    pub question: Option<String>,
    pub sources: Vec<AnswerSource>,
    pub changed: usize,
    pub last_refreshed: Option<String>,
}

pub fn status(store: &SqliteStore, doc_id: Uuid) -> LivingStatus {
    let sources = store.answer_sources(doc_id).unwrap_or_default();
    if sources.is_empty() {
        return LivingStatus { is_answer: false, question: None, sources, changed: 0, last_refreshed: None };
    }
    let question = store.read_doc(doc_id).ok().and_then(|t| question_of(&t));
    let changed = sources.iter().filter(|s| s.changed).count();
    let last_refreshed = sources.iter().map(|s| s.recorded_at.clone()).max();
    LivingStatus { is_answer: true, question, sources, changed, last_refreshed }
}

fn walk<'a>(nodes: &'a [BlockNode], out: &mut Vec<&'a BlockNode>) {
    for n in nodes {
        out.push(n);
        walk(&n.children, out);
    }
}

/// Blocks the refresh may replace: authored by `agent`, never edited by
/// anyone else since, and neither the frontmatter nor the H1 (those carry
/// the question). Everything else in the doc is a human's and stays.
pub fn refreshable_blocks(store: &SqliteStore, tree: &DocTree, agent: Uuid) -> Vec<Uuid> {
    let touched_by_others: HashSet<Uuid> = store
        .ops_since(tree.doc.id, 0)
        .unwrap_or_default()
        .into_iter()
        .filter(|o| o.principal != agent && o.epoch_applied.is_some())
        .filter_map(|o| o.kind.target_block())
        .collect();
    let mut all = Vec::new();
    walk(&tree.roots, &mut all);
    all.into_iter()
        .filter(|n| n.block.created_by == agent)
        .filter(|n| !touched_by_others.contains(&n.block.id))
        .filter(|n| !grimoire_store::import::is_frontmatter(&n.block.content))
        .filter(|n| grimoire_store::import::heading_level(&n.block.content) != Some(1))
        .filter(|n| n.block.block_type != grimoire_store::BlockType::Comment)
        .map(|n| n.block.id)
        .collect()
}

/// The ops of one refresh: delete every refreshable block, insert the new
/// body right under the H1 (before any surviving human block). Inserted
/// roots are re-keyed against the H1's remaining children; nested blocks
/// keep the keys `to_ops` minted under their own new parents.
pub fn refresh_ops(tree: &DocTree, refreshable: &[Uuid], body_md: &str, source_ref: &str) -> Vec<OpInput> {
    let gone: HashSet<Uuid> = refreshable.iter().copied().collect();
    let h1 = tree
        .roots
        .iter()
        .find(|n| grimoire_store::import::heading_level(&n.block.content) == Some(1));
    let (parent, surviving_first) = match h1 {
        Some(h) => (
            Some(h.block.id),
            h.children.iter().find(|c| !gone.contains(&c.block.id)).map(|c| c.block.order_key.clone()),
        ),
        None => (
            None,
            tree.roots
                .iter()
                .filter(|n| !gone.contains(&n.block.id))
                .filter(|n| !grimoire_store::import::is_frontmatter(&n.block.content))
                .map(|n| n.block.order_key.clone())
                .next(),
        ),
    };
    let refs = vec![source_ref.to_string()];
    let mut ops: Vec<OpInput> = refreshable
        .iter()
        .map(|t| OpInput { kind: OpKind::Delete { target: *t }, source_refs: refs.clone() })
        .collect();
    let mut prev: Option<String> = None;
    for mut op in grimoire_store::import::to_ops(grimoire_store::import::segment(body_md)) {
        if let OpKind::Insert { parent_id, order_key, .. } = &mut op.kind
            && parent_id.is_none()
        {
            let key = order_key::between(prev.as_deref(), surviving_first.as_deref());
            prev = Some(key.clone());
            *order_key = key;
            *parent_id = parent;
        }
        op.source_refs = refs.clone();
        ops.push(op);
    }
    ops
}

/// The refreshed body: synthesis (or its placeholder text), receipts, footer.
pub fn refreshed_body(synthesis: Option<&str>, excerpts: &[SearchHit], changed: usize, total: usize) -> String {
    let docs: HashSet<Uuid> = excerpts.iter().map(|h| h.block.doc_id).collect();
    let date = chrono::Local::now().format("%Y-%m-%d");
    let mut md = String::new();
    if let Some(s) = synthesis {
        md.push_str(s.trim());
        md.push_str("\n\n");
    }
    md.push_str(&crate::ask::receipts_markdown(excerpts));
    md.push_str(&format!(
        "---\n\n*Refreshed {date} · {n} block{s} across {d} doc{ds} · {changed} of {total} cited block{cs} had changed.*\n",
        n = excerpts.len(),
        s = if excerpts.len() == 1 { "" } else { "s" },
        d = docs.len(),
        ds = if docs.len() == 1 { "" } else { "s" },
        cs = if total == 1 { "" } else { "s" },
    ));
    md
}

/// Refresh one answer. Returns a run-log line; `Err` lines are skips.
pub async fn refresh_one(
    store: Arc<Mutex<SqliteStore>>,
    hot: crate::hot::HotState,
    doc_id: Uuid,
) -> Result<String, String> {
    let embedder = embedder();
    // phase 1: what changed, the question, fresh retrieval
    let (question, excerpts, changed, total, agent) = {
        let hot = hot.clone();
        with_store(&store, move |s| -> Result<_, String> {
            let tree = s.read_doc(doc_id).map_err(|e| e.to_string())?;
            let sources = s.answer_sources(doc_id).map_err(|e| e.to_string())?;
            let changed = sources.iter().filter(|x| x.changed).count();
            if changed == 0 {
                return Err(format!("{}: fresh, nothing to do", tree.doc.title));
            }
            if hot.is_hot(doc_id) {
                return Err(format!("{}: deferred, doc is in a live session", tree.doc.title));
            }
            let question = question_of(&tree).ok_or_else(|| format!("{}: no question recorded", tree.doc.title))?;
            let excerpts = crate::ask::retrieve(s, embedder.as_deref(), &question);
            if excerpts.is_empty() {
                return Err(format!("{}: nothing in the vault answers it any more; left as is", tree.doc.title));
            }
            let agent = crate::room::agent_principal(s).map_err(|e| e.to_string())?;
            Ok((question, excerpts, changed, sources.len(), agent))
        })
        .await?
    };
    // phase 2: synthesis, off the lock, only when Claude Code is here
    let synthesis = if crate::garden::claude_bin().is_some() {
        let prompt = crate::ask::compose(&question, &excerpts);
        match crate::garden::invoke_claude_bounded(&prompt, REFRESH_WALL_CLOCK).await {
            Ok((t, _)) => Some(t.trim().to_string()),
            Err(e) => {
                tracing::warn!(%doc_id, "living answer synthesis failed: {e}");
                Some(format!("*The synthesis could not be rewritten ({e}); the excerpts below stand on their own.*"))
            }
        }
    } else {
        None
    };
    // phase 3: land as reviewable yellows, re-record the sources
    with_store(&store, move |s| -> Result<String, String> {
        let tree = s.read_doc(doc_id).map_err(|e| e.to_string())?;
        let refreshable = refreshable_blocks(s, &tree, agent);
        let body = refreshed_body(synthesis.as_deref(), &excerpts, changed, total);
        let source_ref = format!("living-answer: refreshed, {changed} of {total} cited blocks changed");
        let ops = refresh_ops(&tree, &refreshable, &body, &source_ref);
        let out = s
            .propose_reviewed(doc_id, tree.doc.current_epoch, agent, ops)
            .map_err(|e| e.to_string())?;
        record_sources(s, doc_id, &excerpts).map_err(|e| e.to_string())?;
        Ok(format!(
            "{} → epoch {}: {changed} of {total} sources changed; {} blocks replaced by {} ({})",
            tree.doc.title,
            out.epoch,
            refreshable.len(),
            excerpts.len(),
            crate::garden::verdict_counts(&out)
        ))
    })
    .await
}

/// Stale answers, the one whose sources went stale longest ago first (by
/// the answer's own recording time — a proxy the table can answer).
pub fn stale_answers(store: &SqliteStore, only: Option<Uuid>) -> Vec<Uuid> {
    let ids = match only {
        Some(d) => vec![d],
        None => store.answer_docs().unwrap_or_default(),
    };
    let mut stale: Vec<(String, Uuid)> = ids
        .into_iter()
        .filter_map(|d| {
            let sources = store.answer_sources(d).ok()?;
            is_stale(&sources).then(|| (sources.iter().map(|s| s.recorded_at.clone()).max().unwrap_or_default(), d))
        })
        .collect();
    stale.sort();
    stale.into_iter().map(|(_, d)| d).collect()
}

/// One sweep: up to `REFRESH_BUDGET` stale answers. Logged like a gardener
/// run (one line per answer); returns the lines.
pub async fn refresh_sweep(
    store: Arc<Mutex<SqliteStore>>,
    hot: crate::hot::HotState,
    only: Option<Uuid>,
) -> Vec<String> {
    let candidates = with_store(&store, move |s| stale_answers(s, only)).await;
    let mut lines = Vec::new();
    if candidates.is_empty() {
        lines.push("living answers: nothing stale".into());
        return lines;
    }
    for d in candidates.into_iter().take(REFRESH_BUDGET) {
        let line = match refresh_one(store.clone(), hot.clone(), d).await {
            Ok(l) => l,
            Err(l) => l,
        };
        tracing::info!("living answer {d}: {line}");
        lines.push(line);
    }
    lines
}

// --- routes ---

/// `GET /api/doc/{id}/living`
pub async fn living_status(State(st): State<crate::api::ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    with_store(&st.store, move |s| Json(json!(status(s, id)))).await
}

#[derive(Deserialize, Default)]
pub struct RefreshReq {
    pub doc_id: Option<Uuid>,
}

/// `POST /admin/living/refresh` — run the sweep now, optionally for one doc.
pub async fn refresh_now(
    State(st): State<crate::admin::AdminState>,
    body: Option<Json<RefreshReq>>,
) -> Json<Value> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let lines = refresh_sweep(st.store.clone(), st.hot.clone(), req.doc_id).await;
    Json(json!({"ok": true, "lines": lines}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::{PrincipalKind, Verdict, import::import_markdown};

    fn seed() -> (Arc<Mutex<SqliteStore>>, Uuid, Uuid, Uuid, Vec<SearchHit>) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let human_folder = crate::ask::ANSWERS_FOLDER;
        import_markdown(&mut s, "Grants", None, tom, "# Grants\n\nThe grant flow uses temporary delegation.\n\nGrants expire after one hour.\n").unwrap();
        let answers = s.create_doc(human_folder, None, tom).unwrap().id;
        let agent = crate::room::agent_principal(&mut s).unwrap();
        let q = "how does the grant flow work?";
        let excerpts = crate::ask::retrieve(&s, None, q);
        assert!(!excerpts.is_empty());
        let md = format!(
            "{}\n\n# {q}\n\n{}{}---\n\n*Asked today.*\n",
            question_frontmatter(q),
            crate::ask::SYNTH_PLACEHOLDER,
            crate::ask::receipts_markdown(&excerpts)
        );
        let (doc, _) = crate::garden::create_doc_through_gate(
            &mut s, "how does the grant flow work", Some(answers), agent, &md, grimoire_store::ConfidencePolicy::Gate,
        )
        .unwrap();
        record_sources(&mut s, doc, &excerpts).unwrap();
        (Arc::new(Mutex::new(s)), tom, agent, doc, excerpts)
    }

    #[test]
    fn question_round_trips_through_frontmatter_with_quotes() {
        let q = r#"what did we "decide" about C:\paths?"#;
        let fm = question_frontmatter(q);
        assert!(grimoire_store::import::is_frontmatter(&fm));
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let (d, _) = import_markdown(&mut s, "A", None, tom, &format!("{fm}\n\n# {q}\n\nbody\n")).unwrap();
        assert_eq!(question_of(&s.read_doc(d).unwrap()).as_deref(), Some(q));
        // no frontmatter: the H1 is the fallback
        let (d2, _) = import_markdown(&mut s, "B", None, tom, "# plain question\n\nbody\n").unwrap();
        assert_eq!(question_of(&s.read_doc(d2).unwrap()).as_deref(), Some("plain question"));
        // a tagging pass appends to the same block without losing the question
        let tree = s.read_doc(d).unwrap();
        let fm_block = tree.roots[0].block.clone();
        let tagged = fm_block.content.replacen("---\n", "---\ntags:\n  - x\n", 1);
        s.apply(d, tree.doc.current_epoch, tom, vec![OpInput { kind: OpKind::Replace { target: fm_block.id, content: tagged }, source_refs: vec![] }]).unwrap();
        assert_eq!(question_of(&s.read_doc(d).unwrap()).as_deref(), Some(q));
    }

    #[test]
    fn a_changed_cited_block_marks_the_answer_stale_and_an_unchanged_one_does_not() {
        let (store, tom, _, doc, excerpts) = seed();
        let mut s = store.lock().unwrap();
        let st = status(&s, doc);
        assert!(st.is_answer && st.changed == 0);
        assert_eq!(st.question.as_deref(), Some("how does the grant flow work?"));
        assert!(stale_answers(&s, None).is_empty());
        // an unrelated edit in the source doc (a new block) changes nothing cited
        let src = excerpts[0].block.doc_id;
        let epoch = s.get_doc(src).unwrap().current_epoch;
        s.apply(src, epoch, tom, vec![OpInput {
            kind: OpKind::Insert {
                block_id: Uuid::now_v7(), parent_id: None, order_key: order_key::between(Some("zz"), None),
                block_type: grimoire_store::BlockType::Paragraph, content: "unrelated addition".into(), refers_to: None,
            },
            source_refs: vec![],
        }]).unwrap();
        assert_eq!(status(&s, doc).changed, 0, "an unchanged cited block stays fresh");
        // editing a cited block: stale
        let cited = excerpts[0].block.id;
        let epoch = s.get_doc(src).unwrap().current_epoch;
        s.apply(src, epoch, tom, vec![OpInput { kind: OpKind::Replace { target: cited, content: "The grant flow now uses long-lived tokens.".into() }, source_refs: vec![] }]).unwrap();
        let st = status(&s, doc);
        assert_eq!(st.changed, 1);
        assert!(is_stale(&st.sources));
        assert_eq!(stale_answers(&s, None), vec![doc]);
        assert!(stale_answers(&s, Some(Uuid::now_v7())).is_empty());
    }

    #[test]
    fn refresh_ops_replace_only_agent_blocks_and_land_under_the_h1() {
        let (store, tom, agent, doc, excerpts) = seed();
        let mut s = store.lock().unwrap();
        // a human adds a note into the answer
        let tree = s.read_doc(doc).unwrap();
        let h1 = tree.roots.iter().find(|n| n.block.content.starts_with("# ")).unwrap().block.clone();
        s.apply(doc, tree.doc.current_epoch, tom, vec![OpInput {
            kind: OpKind::Insert {
                block_id: Uuid::now_v7(), parent_id: Some(h1.id), order_key: order_key::between(Some("zzz"), None),
                block_type: grimoire_store::BlockType::Paragraph, content: "HUMAN NOTE: keep this".into(), refers_to: None,
            },
            source_refs: vec![],
        }]).unwrap();
        let tree = s.read_doc(doc).unwrap();
        let refreshable = refreshable_blocks(&s, &tree, agent);
        let mut all = Vec::new();
        walk(&tree.roots, &mut all);
        let human: Vec<Uuid> = all.iter().filter(|n| n.block.content.contains("HUMAN NOTE")).map(|n| n.block.id).collect();
        assert_eq!(human.len(), 1);
        assert!(!refreshable.contains(&human[0]), "human block is never refreshable");
        assert!(!refreshable.contains(&h1.id), "the H1 stays");
        assert!(!refreshable.contains(&tree.roots[0].block.id), "the frontmatter stays");
        assert!(refreshable.len() >= 3, "{refreshable:?}");

        let body = refreshed_body(Some("New synthesis."), &excerpts, 1, excerpts.len());
        let ops = refresh_ops(&tree, &refreshable, &body, "living-answer: refreshed, 1 of 2 cited blocks changed");
        let out = s.propose_reviewed(doc, tree.doc.current_epoch, agent, ops).unwrap();
        assert!(out.verdicts.iter().all(|v| v.verdict == Verdict::Yellow), "every change is reviewable");
        let md = grimoire_store::export::export_doc(&*s, doc).unwrap();
        assert!(md.contains("HUMAN NOTE: keep this"));
        assert!(md.contains("New synthesis."));
        assert!(md.contains("Refreshed "));
        assert!(!md.contains("*Asked today.*"));
        assert!(md.starts_with("---\nquestion:"));
        // the new content sits under the H1, before the human note
        let synth_at = md.find("New synthesis.").unwrap();
        assert!(md.find("# how does").unwrap() < synth_at && synth_at < md.find("HUMAN NOTE").unwrap(), "{md}");
    }

    /// No Claude binary in the test env → receipts-only refresh, through the gate.
    #[tokio::test]
    async fn sweep_refreshes_stale_answers_without_a_synthesis_and_records_new_sources() {
        // point the resolver at a path that is not a file so the test never spawns claude
        unsafe { std::env::set_var("GRIMOIRE_CLAUDE_BIN", "/nonexistent/claude") };
        let (store, tom, _, doc, excerpts) = seed();
        let hot = crate::hot::HotState::new(std::env::temp_dir().join(format!("grimoire-living-{}", Uuid::now_v7())));
        let lines = refresh_sweep(store.clone(), hot.clone(), None).await;
        assert_eq!(lines, vec!["living answers: nothing stale".to_string()]);

        {
            let mut s = store.lock().unwrap();
            let src = excerpts[0].block.doc_id;
            let epoch = s.get_doc(src).unwrap().current_epoch;
            s.apply(src, epoch, tom, vec![OpInput { kind: OpKind::Replace { target: excerpts[0].block.id, content: "The grant flow uses temporary delegation and expires fast.".into() }, source_refs: vec![] }]).unwrap();
        }
        let lines = refresh_sweep(store.clone(), hot.clone(), None).await;
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("1 of"), "{lines:?}");
        {
            let s = store.lock().unwrap();
            let st = status(&s, doc);
            assert_eq!(st.changed, 0, "sources re-recorded at their new epochs");
            assert!(s.answer_sources(doc).unwrap().iter().any(|x| x.block_id == excerpts[0].block.id));
            let md = grimoire_store::export::export_doc(&*s, doc).unwrap();
            assert!(md.contains("expires fast"), "new receipt text landed: {md}");
            assert!(!md.contains(crate::ask::SYNTH_PLACEHOLDER), "no placeholder without a synthesis");
            let q = s.review_queue(Some(doc)).unwrap();
            assert!(!q.is_empty(), "the refresh is reviewable");
            assert!(q.iter().all(|i| i.op.source_refs.iter().any(|r| r.starts_with("living-answer: refreshed, 1 of"))));
        }
        // a live session defers the doc
        {
            let mut s = store.lock().unwrap();
            let src = excerpts[0].block.doc_id;
            let epoch = s.get_doc(src).unwrap().current_epoch;
            s.apply(src, epoch, tom, vec![OpInput { kind: OpKind::Replace { target: excerpts[0].block.id, content: "again".into() }, source_refs: vec![] }]).unwrap();
        }
        let frozen = crate::hot::HotState::new(std::env::temp_dir().join(format!("grimoire-living-{}", Uuid::now_v7())));
        frozen.start(doc, 0).unwrap();
        let lines = refresh_sweep(store.clone(), frozen, Some(doc)).await;
        assert!(lines[0].contains("live session"), "{lines:?}");
    }
}
