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

/// Ranked excerpts: blocks hit by the most distinct keywords first; ties by
/// shorter content (denser). Frontmatter/comment blocks never make the cut.
pub fn retrieve(store: &SqliteStore, question: &str) -> Vec<SearchHit> {
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
    let mut out = Vec::new();
    let mut chars = 0usize;
    for (_, h) in ranked {
        if out.len() >= MAX_BLOCKS || chars + h.block.content.len() > MAX_CHARS {
            break;
        }
        chars += h.block.content.len();
        out.push(h);
    }
    out
}

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
    human: Uuid,
    question: String,
) -> Result<Answer, String> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("ask something".into());
    }
    let excerpts = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        retrieve(&s, &question)
    };
    let docs: std::collections::HashSet<Uuid> = excerpts.iter().map(|h| h.block.doc_id).collect();
    if excerpts.is_empty() {
        // nothing to ground an answer in: say so, leave no doc behind
        return Ok(Answer { doc_id: None, title: title_for(&question), sources: 0, docs: 0 });
    }
    let prompt = compose(&question, &excerpts);
    let (text, _tokens) = crate::garden::invoke_claude_bounded(&prompt, WALL_CLOCK).await?;
    let answer_md = text.trim().to_string();
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let md = format!(
        "# {q}\n\n{answer_md}\n\n---\n\n*Asked {date} · answered from {n} block{s} across {d} doc{ds}.*\n",
        q = question,
        n = excerpts.len(),
        s = if excerpts.len() == 1 { "" } else { "s" },
        d = docs.len(),
        ds = if docs.len() == 1 { "" } else { "s" },
    );
    let title = title_for(&question);
    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
    let folder = answers_folder(&mut s, human).map_err(|e| e.to_string())?;
    let agent = crate::room::agent_principal(&mut s).map_err(|e| e.to_string())?;
    let (doc_id, _) = grimoire_store::import::import_markdown(&mut *s, &title, Some(folder), agent, &md)
        .map_err(|e| e.to_string())?;
    Ok(Answer {
        doc_id: Some(doc_id),
        title,
        sources: excerpts.len(),
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
        let hits = retrieve(&s, "how does the grant flow use delegation?");
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
        let hits = retrieve(&s, "alpha fact");
        let p = compose("what is alpha?", &hits);
        assert!(p.contains("[1] doc: Doc A · id: "));
        assert!(p.contains("[[<doc title>#^<id>]]"));
        assert!(p.contains("Question: what is alpha?"));
    }

    #[test]
    fn title_truncates_and_strips_the_question_mark() {
        assert_eq!(title_for("Why is the sky blue?"), "Why is the sky blue");
        assert!(title_for(&"x".repeat(200)).ends_with('…'));
        assert_eq!(title_for("  ?  "), "Question");
    }
}
