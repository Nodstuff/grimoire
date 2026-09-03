//! Ask the vault, with receipts: a question → an answer doc whose every
//! claim links `[[Doc Title#^block-id]]` to the exact block it rests on.
//!
//! Retrieval is the block-granular search the app already has (FTS5
//! trigram): the question's content words are each searched, hits are
//! scored by how many words they carry, and the top blocks go to the model
//! as numbered excerpts with their ids. The model may ONLY use those
//! excerpts and must cite; if they don't answer the question it says so.
//! The answer becomes a doc under the `Answers` folder, written through the
//! import path under the agent principal, so it is itself searchable,
//! gardened and auditable (its `source_refs` are the cited block ids).

use crate::store_ext::with_store;
use grimoire_store::{BlockStore, SearchHit, SqliteStore};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub const ANSWERS_FOLDER: &str = "Answers";
const MAX_BLOCKS: usize = 40;
const MAX_CHARS: usize = 16_000;
const PER_WORD_LIMIT: usize = 20;
const WALL_CLOCK: std::time::Duration = std::time::Duration::from_secs(120);

const STOP: &[&str] = &[
    "the", "and", "that", "this", "with", "from", "what", "when", "where", "which", "while",
    "about", "have", "does", "did", "how", "why", "who", "are", "was", "were", "will", "would",
    "should", "could", "there", "their", "they", "them", "then", "than", "into", "onto", "over",
    "under", "after", "before", "between", "because", "also", "just", "like", "some", "any",
    "our", "your", "its", "for", "not", "but", "you", "can", "all", "one", "two", "use", "used",
    "using", "make", "made", "need", "want", "know", "tell", "show", "give", "decide", "decided",
    "decision", "decisions", "explain", "summarise", "summarize", "list", "please", "grimoire",
];

/// Content words worth searching: ≥3 chars, not a stopword, deduped, in order.
pub fn keywords(question: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in question.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_') {
        let w = raw.trim_matches('-').to_lowercase();
        if w.len() < 3 || STOP.contains(&w.as_str()) || !seen.insert(w.clone()) {
            continue;
        }
        out.push(w);
    }
    out
}

/// Ranked excerpts, hybrid. On a real vault (10k+ blocks) neither leg is
/// trustworthy alone: a static embedding's cosine is flat across the top 20
/// (0.44–0.52, mostly noise), and a single common question word matches a
/// hundred blocks by keyword. So: candidates = keyword hits carrying at least
/// half the question's content words ∪ the dense top-20; each scored by
/// cosine + a coverage bonus for the fraction of content words it contains;
/// cut at a gap below the best, capped, then the char budget. Works without
/// an embedder (coverage only). Bare headings, comments, frontmatter and
/// earlier Answers are never evidence.
pub fn retrieve(store: &SqliteStore, embedder: Option<&crate::embed::Embedder>, question: &str) -> Vec<SearchHit> {
    let words = keywords(question);
    let answers = answers_folder_id(store);
    let usable = |h: &SearchHit| {
        h.block.block_type != grimoire_store::BlockType::Comment
            && !grimoire_store::import::is_frontmatter(&h.block.content)
            && !is_bare_heading(&h.block.content)
            && !under_answers(store, h.block.doc_id, answers)
    };
    let stems: Vec<String> = words.iter().map(|w| stem(w)).collect();
    // the doc title is part of what a block says ("Review Gate" → its body
    // never repeats "gate"); match on crude stems so pin/pinning, version/
    // versions count as the same word
    let coverage = |h: &SearchHit| -> f32 {
        if stems.is_empty() {
            return 0.0;
        }
        let lc = format!("{} {}", h.doc_title, h.block.content).to_lowercase();
        stems.iter().filter(|st| lc.contains(st.as_str())).count() as f32 / stems.len() as f32
    };
    let need = ((words.len() + 1) / 2).max(1) as f32 / words.len().max(1) as f32;

    let mut cands: HashMap<Uuid, SearchHit> = HashMap::new();
    for h in retrieve_keyword(store, question) {
        if usable(&h) && coverage(&h) >= need {
            cands.entry(h.block.id).or_insert(h);
        }
    }
    let q_vec = embedder.map(|e| e.encode_one(question));
    if let Some(e) = embedder {
        let ids: Vec<Uuid> = e.search(question, DENSE_TOP).into_iter().map(|(id, _)| id).collect();
        for h in store.blocks_as_hits(&ids).unwrap_or_default() {
            // a dense neighbour sharing NO content word with a multi-word
            // question is the model being generous to a long block
            if usable(&h) && (stems.len() < 2 || coverage(&h) > 0.0) {
                cands.entry(h.block.id).or_insert(h);
            }
        }
    }
    let mut scored: Vec<(f32, SearchHit)> = cands
        .into_values()
        .map(|h| {
            let dense = match (&q_vec, embedder) {
                (Some(q), Some(e)) => e.score(q, h.block.id).unwrap_or(0.0),
                _ => 0.0,
            };
            // long blocks mention every word by accident of size: scale
            // coverage by sqrt(300/len) past 300 chars (1,200 chars → half)
            let len = h.block.content.chars().count().max(LEN_NORM) as f32;
            let len_factor = (LEN_NORM as f32 / len).sqrt();
            (DENSE_WEIGHT * dense + COVERAGE_WEIGHT * coverage(&h) * len_factor, h)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let top = scored.first().map(|(s, _)| *s).unwrap_or(0.0);
    let ranked: Vec<SearchHit> = scored
        .into_iter()
        .enumerate()
        .take_while(|(i, (s, _))| *i < MIN_KEEP || *s >= top - GAP)
        .map(|(_, (_, h))| h)
        .take(MAX_KEEP)
        .collect();
    budget(ranked)
}

/// Crude English stem for containment tests: strip a common suffix when
/// enough of the word is left. `pinning`→`pin`, `versions`→`version`,
/// `decided`→`decid` (matches decide/decided/deciding).
pub fn stem(w: &str) -> String {
    for suf in ["ing", "ies", "ed", "es", "s"] {
        if let Some(base) = w.strip_suffix(suf)
            && base.len() >= 3
        {
            // pinning → pinn → pin
            if suf == "ing" && base.len() >= 4 && base.as_bytes()[base.len() - 1] == base.as_bytes()[base.len() - 2] {
                return base[..base.len() - 1].to_string();
            }
            return base.to_string();
        }
    }
    w.to_string()
}

/// Dense candidates considered per question.
const DENSE_TOP: usize = 20;
/// A static embedding rewards long, word-rich blocks (a daily-log bullet
/// scores 0.45+ against almost any question) while a terse, exactly-right
/// note sits at 0.3. So coverage carries the ranking and cosine breaks ties:
/// full coverage is worth about twice the whole useful cosine range.
const COVERAGE_WEIGHT: f32 = 0.8;
const DENSE_WEIGHT: f32 = 0.5;
/// Blocks longer than this have their coverage discounted by sqrt(300/len).
const LEN_NORM: usize = 300;
/// Keep everything within this of the best combined score…
const GAP: f32 = 0.15;
/// …but always at least this many (if available) and never more than this.
const MIN_KEEP: usize = 3;
const MAX_KEEP: usize = 12;

/// `# Title` alone carries no fact worth citing.
fn is_bare_heading(content: &str) -> bool {
    let t = content.trim();
    t.starts_with('#') && !t.contains('\n')
}

fn answers_folder_id(store: &SqliteStore) -> Option<Uuid> {
    store
        .list_docs()
        .ok()?
        .into_iter()
        .find(|d| d.parent_id.is_none() && d.title == ANSWERS_FOLDER)
        .map(|d| d.id)
}

/// Answer docs must never be evidence for the next answer (they'd echo).
fn under_answers(store: &SqliteStore, doc_id: Uuid, answers: Option<Uuid>) -> bool {
    let Some(a) = answers else { return false };
    let mut cur = Some(doc_id);
    while let Some(id) = cur {
        if id == a {
            return true;
        }
        cur = store.get_doc(id).ok().and_then(|d| d.parent_id);
    }
    false
}

fn budget(ranked: Vec<SearchHit>) -> Vec<SearchHit> {
    let mut out = Vec::new();
    let mut chars = 0usize;
    for h in ranked {
        if out.len() >= MAX_BLOCKS || chars + h.block.content.len() > MAX_CHARS {
            break;
        }
        chars += h.block.content.len();
        out.push(h);
    }
    out
}

/// Keyword leg: blocks hit by the most distinct question words first.
pub fn retrieve_keyword(store: &SqliteStore, question: &str) -> Vec<SearchHit> {
    let words = keywords(question);
    let mut score: HashMap<Uuid, (usize, SearchHit)> = HashMap::new();
    // the trigram index is deliberately fuzzy (typos welcome); for grounding
    // we want the opposite — a block counts for a word only if it really
    // contains it, so the model never sees a near-miss as evidence
    let mut consider = |hit: SearchHit, weight: usize| {
        if hit.block.block_type == grimoire_store::BlockType::Comment
            || grimoire_store::import::is_frontmatter(&hit.block.content)
        {
            return;
        }
        let e = score.entry(hit.block.id).or_insert((0, hit));
        e.0 += weight;
    };
    let contains = |hit: &SearchHit, w: &str| hit.block.content.to_lowercase().contains(w);
    if let Ok(hits) = store.search_blocks(question, PER_WORD_LIMIT) {
        for h in hits {
            let n = words.iter().filter(|w| contains(&h, w)).count();
            if n > 0 {
                consider(h, 2 + n);
            }
        }
    }
    for w in &words {
        if let Ok(hits) = store.search_blocks(w, PER_WORD_LIMIT) {
            for h in hits {
                if contains(&h, w) {
                    consider(h, 1);
                }
            }
        }
    }
    let mut ranked: Vec<(usize, SearchHit)> = score.into_values().collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.block.content.len().cmp(&b.1.block.content.len())));
    ranked.into_iter().map(|(_, h)| h).collect()
}

/// The receipts: excerpts grouped by doc, each a deep link. Instant, free,
/// nothing invented — for "where did we say X" this IS the answer.
pub fn receipts_markdown(excerpts: &[SearchHit]) -> String {
    let mut by_doc: Vec<(String, Vec<&SearchHit>)> = Vec::new();
    for h in excerpts {
        match by_doc.iter_mut().find(|(t, _)| *t == h.doc_title) {
            Some((_, v)) => v.push(h),
            None => by_doc.push((h.doc_title.clone(), vec![h])),
        }
    }
    let mut md = String::new();
    for (title, hits) in by_doc {
        md.push_str(&format!("## {title}\n\n"));
        for h in hits {
            let one_line = h.block.content.trim().replace('\n', " ");
            let shown: String = one_line.chars().take(400).collect();
            let ell = if one_line.chars().count() > 400 { "…" } else { "" };
            md.push_str(&format!("> {shown}{ell}\n>\n> — [[{title}#^{}]]\n\n", h.block.id));
        }
    }
    md
}

pub const SYNTH_PLACEHOLDER: &str = "*✨ Writing a synthesis from the excerpts below…*";

pub fn compose(question: &str, excerpts: &[SearchHit]) -> String {
    let mut ex = String::new();
    for (i, h) in excerpts.iter().enumerate() {
        ex.push_str(&format!(
            "[{n}] doc: {title} · id: {id}\n{content}\n\n",
            n = i + 1,
            title = h.doc_title,
            id = h.block.id,
            content = h.block.content.trim()
        ));
    }
    format!(
        "You answer questions about a person's own notes. Use ONLY the excerpts below — never \
outside knowledge, never guesses. Every factual sentence in your answer must end with a \
citation in exactly this form: [[<doc title>#^<id>]] using the excerpt's doc title and id \
verbatim (several citations may follow one sentence). If the excerpts do not answer the \
question, say what IS covered and what is missing — do not invent.\n\n\
Question: {question}\n\n\
Excerpts:\n\n{ex}\
Write the answer as plain markdown: short paragraphs, a heading only if the answer has \
distinct parts, no preamble, no closing summary, no mention of \"excerpts\"."
    )
}

/// Ensure the `Answers` root folder exists (owned by the human).
fn answers_folder(store: &mut SqliteStore, human: Uuid) -> grimoire_store::Result<Uuid> {
    if let Some(d) = store
        .list_docs()?
        .into_iter()
        .find(|d| d.parent_id.is_none() && d.title == ANSWERS_FOLDER)
    {
        return Ok(d.id);
    }
    Ok(store.create_doc(ANSWERS_FOLDER, None, human)?.id)
}

pub fn title_for(question: &str) -> String {
    let q = question.trim().trim_end_matches('?').trim();
    let mut t: String = q.chars().take(80).collect();
    if q.chars().count() > 80 {
        t.push('…');
    }
    if t.is_empty() { "Question".into() } else { t }
}

#[derive(Debug, serde::Serialize)]
pub struct Answer {
    /// None when nothing in the vault matched — no doc is created for that.
    pub doc_id: Option<Uuid>,
    pub title: String,
    pub sources: usize,
    pub docs: usize,
}

pub async fn ask(
    store: Arc<Mutex<SqliteStore>>,
    embedder: Option<Arc<crate::embed::Embedder>>,
    human: Uuid,
    question: String,
) -> Result<Answer, String> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("ask something".into());
    }
    let excerpts = {
        let (embedder, question) = (embedder.clone(), question.clone());
        with_store(&store, move |s| retrieve(s, embedder.as_deref(), &question)).await
    };
    let docs: std::collections::HashSet<Uuid> = excerpts.iter().map(|h| h.block.doc_id).collect();
    if excerpts.is_empty() {
        // nothing to ground an answer in: say so, leave no doc behind
        return Ok(Answer { doc_id: None, title: title_for(&question), sources: 0, docs: 0 });
    }
    let synthesise = crate::garden::claude_bin().is_some();
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let md = format!(
        "# {q}\n\n{synth}{receipts}---\n\n*Asked {date} · {n} block{s} across {d} doc{ds}.*\n",
        q = question,
        synth = if synthesise { format!("{SYNTH_PLACEHOLDER}\n\n") } else { String::new() },
        receipts = receipts_markdown(&excerpts),
        n = excerpts.len(),
        s = if excerpts.len() == 1 { "" } else { "s" },
        d = docs.len(),
        ds = if docs.len() == 1 { "" } else { "s" },
    );
    let title = title_for(&question);
    let (doc_id, agent) = {
        let title = title.clone();
        with_store(&store, move |s| -> Result<(Uuid, Uuid), String> {
            let folder = answers_folder(s, human).map_err(|e| e.to_string())?;
            let agent = crate::room::agent_principal(s).map_err(|e| e.to_string())?;
            // through the gate under the agent, never apply (ledgered verdicts)
            let (doc_id, _) = crate::garden::create_doc_through_gate(
                s,
                &title,
                Some(folder),
                agent,
                &md,
                grimoire_store::ConfidencePolicy::Gate,
            )
            .map_err(|e| e.to_string())?;
            Ok((doc_id, agent))
        })
        .await?
    };
    let sources = excerpts.len();
    if synthesise {
        // the prose lands a few seconds later in the same doc, replacing the
        // placeholder block through the gate under the agent principal
        let store = store.clone();
        let q = question.clone();
        tokio::spawn(async move {
            let prompt = compose(&q, &excerpts);
            let text = match crate::garden::invoke_claude_bounded(&prompt, WALL_CLOCK).await {
                Ok((t, _)) => t.trim().to_string(),
                Err(e) => {
                    tracing::warn!(%doc_id, "ask synthesis failed: {e}");
                    format!("*The synthesis could not be written ({e}); the excerpts below stand on their own.*")
                }
            };
            with_store(&store, move |s| {
                let Ok(tree) = s.read_doc(doc_id) else { return };
                let placeholder = tree
                    .roots
                    .iter()
                    .flat_map(|n| std::iter::once(&n.block).chain(n.children.iter().map(|c| &c.block)))
                    .find(|b| b.content == SYNTH_PLACEHOLDER)
                    .map(|b| b.id);
                let Some(target) = placeholder else { return };
                let op = grimoire_store::OpInput {
                    kind: grimoire_store::OpKind::Replace { target, content: text },
                    source_refs: vec!["ask-the-vault: synthesis".into()],
                };
                if let Err(e) = s.propose(doc_id, tree.doc.current_epoch, agent, vec![op]) {
                    tracing::warn!(%doc_id, "ask synthesis could not land: {e}");
                }
            })
            .await;
        });
    }
    Ok(Answer {
        doc_id: Some(doc_id),
        title,
        sources,
        docs: docs.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::{PrincipalKind, import::import_markdown};

    #[test]
    fn keywords_drop_stopwords_and_dedupe() {
        assert_eq!(
            keywords("What did we decide about the grant flow, and why the grant?"),
            vec!["grant", "flow"]
        );
    }

    #[test]
    fn retrieve_ranks_blocks_hit_by_more_keywords_first_and_skips_frontmatter() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        import_markdown(&mut s, "Grants", None, tom.id,
            "---\ntags:\n  - grant\n---\n\nThe grant flow uses temporary delegation.\n\nUnrelated paragraph about lunch.\n").unwrap();
        import_markdown(&mut s, "Other", None, tom.id, "Delegation is mentioned here alone.\n").unwrap();
        let hits = retrieve(&s, None, "how does the grant flow use delegation?");
        assert!(!hits.is_empty());
        assert!(hits[0].block.content.contains("grant flow uses temporary delegation"));
        assert!(hits.iter().all(|h| !h.block.content.starts_with("---")));
        assert!(hits.iter().all(|h| !h.block.content.contains("lunch")));
    }

    #[test]
    fn compose_numbers_excerpts_with_ids_and_demands_citations() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        import_markdown(&mut s, "Doc A", None, tom.id, "alpha fact\n").unwrap();
        let hits = retrieve(&s, None, "alpha fact");
        let p = compose("what is alpha?", &hits);
        assert!(p.contains("[1] doc: Doc A · id: "));
        assert!(p.contains("[[<doc title>#^<id>]]"));
        assert!(p.contains("Question: what is alpha?"));
    }

    #[test]
    fn receipts_group_by_doc_and_deep_link_every_excerpt() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        import_markdown(&mut s, "Doc A", None, tom.id, "alpha one\n\nalpha two\n").unwrap();
        import_markdown(&mut s, "Doc B", None, tom.id, "alpha three\n").unwrap();
        let hits = retrieve(&s, None, "alpha");
        let md = receipts_markdown(&hits);
        assert_eq!(md.matches("## ").count(), 2);
        assert_eq!(md.matches("#^").count(), 3);
        for h in &hits {
            assert!(md.contains(&format!("[[{}#^{}]]", h.doc_title, h.block.id)));
        }
    }

    #[test]
    fn retrieval_skips_bare_headings_and_earlier_answers() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        import_markdown(&mut s, "Grants", None, tom.id, "# Grants\n\nThe grant flow uses delegation.\n").unwrap();
        let answers = s.create_doc(ANSWERS_FOLDER, None, tom.id).unwrap();
        import_markdown(&mut s, "old answer", Some(answers.id), tom.id, "Earlier we said the grant flow uses delegation.\n").unwrap();
        let hits = retrieve(&s, None, "grant flow delegation");
        assert_eq!(hits.len(), 1, "{:?}", hits.iter().map(|h| &h.block.content).collect::<Vec<_>>());
        assert_eq!(hits[0].doc_title, "Grants");
        assert!(!hits[0].block.content.starts_with('#'));
    }

    #[test]
    fn stems_collapse_common_suffixes() {
        assert_eq!(stem("pinning"), "pin");
        assert_eq!(stem("versions"), "version");
        assert_eq!(stem("dependencies"), "dependenc");
        assert_eq!(stem("gate"), "gate");
        assert_eq!(stem("bus"), "bus");
    }

    #[test]
    fn title_truncates_and_strips_the_question_mark() {
        assert_eq!(title_for("Why is the sky blue?"), "Why is the sky blue");
        assert!(title_for(&"x".repeat(200)).ends_with('…'));
        assert_eq!(title_for("  ?  "), "Question");
    }
}
