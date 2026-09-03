//! Agents in the room: a gardener-shaped agent joins a LIVE session as a
//! visible participant and writes **suggestions**, never text.
//!
//! The gate only sees a live session at flatten, so an agent typing plain
//! text into the CRDT would bypass the one rule everything else obeys. The
//! rule holds like this: everything the agent writes is a top-level node
//! carrying `suggestion="insert"|"replace"` (+ `suggestionId`, `suggestionBy`).
//! The editor renders those tinted with inline ✓/✗; accepting strips the
//! attribute (and, for a replace, deletes the original, which the agent
//! marked `suggestion="replaced"`); rejecting deletes the node and clears
//! the original. `yrender` SKIPS unaccepted insert/replace nodes and renders
//! a `replaced` original as-is, so an ignored suggestion is dropped at
//! flatten — nothing the agent wrote becomes doc content without a click.
//!
//! One ask = one `claude -p` round trip: the current session text goes out
//! with numbered blocks, structured suggestions come back and are applied
//! into the yrs doc under the daemon's own awareness identity ("🌿 scribe"),
//! so the room sees who is writing. Owner-side only: the session lives on
//! the owner's daemon and so does the agent.

use crate::hot::HotState;
use crate::store_ext::with_store;
use grimoire_store::{BlockStore, PrincipalKind, SqliteStore};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use yrs::sync::Message as YMessage;
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{Text, Transact, Xml, XmlFragment, XmlOut};

pub const AGENT_NAME: &str = "scribe";
const MAX_SUGGESTIONS: usize = 12;
const MAX_DOC_CHARS: usize = 60_000;
/// One ask is interactive: people are waiting in the room.
const WALL_CLOCK: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AgentStatus {
    pub busy: bool,
    pub last_error: Option<String>,
    pub last_ok: Option<String>,
    pub asks: u32,
}

#[derive(Debug, serde::Deserialize)]
struct SuggestionOut {
    /// 1-based index into the numbered blocks the model saw; 0 = top of doc.
    anchor: usize,
    /// "after" (insert below the anchor) or "replace" (offer a replacement).
    mode: String,
    markdown: String,
}

#[derive(Debug, serde::Deserialize)]
struct Reply {
    #[serde(default)]
    suggestions: Vec<SuggestionOut>,
    #[serde(default)]
    note: Option<String>,
}

/// The numbered view the model reasons over — one line per top-level node,
/// rendered exactly as the flatten would, minus unaccepted suggestions.
fn numbered_blocks(hot: &HotState, doc_id: Uuid) -> Option<Vec<String>> {
    let sessions = hot.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let session = sessions.get(&doc_id)?;
    let frag = session.awareness.doc().get_or_insert_xml_fragment("default");
    let txn = session.awareness.doc().transact();
    Some(crate::yrender::fragment_to_blocks(&txn, &frag))
}

fn compose(blocks: &[String], instruction: &str, participants: &str) -> String {
    let mut doc = String::new();
    for (i, b) in blocks.iter().enumerate() {
        doc.push_str(&format!("[{}] {}\n\n", i + 1, b));
    }
    if doc.len() > MAX_DOC_CHARS {
        doc.truncate(MAX_DOC_CHARS);
        doc.push_str("\n\n[… truncated …]\n");
    }
    format!(
        "You are `{AGENT_NAME}`, an agent sitting in a LIVE co-editing session of one markdown \
document. People in the room: {participants}. They asked you:\n\n\"{instruction}\"\n\n\
The document right now, one numbered block per top-level paragraph/heading/code block:\n\n\
{doc}\n\
Respond with SUGGESTIONS, not edits. Each suggestion is markdown that either goes AFTER a \
numbered block (mode \"after\"; anchor 0 = top of the document) or is offered as a \
REPLACEMENT for one block (mode \"replace\"). People accept or reject each one inline, so be \
precise and minimal: never restate unchanged text, never suggest more than {MAX_SUGGESTIONS} \
items, prefer one good suggestion over several weak ones. Keep the document's voice. Plain \
markdown paragraphs, headings (# …) and fenced code blocks only — no lists inside a single \
suggestion unless the document already uses them.\n\n\
Answer with JSON ONLY, no prose around it:\n\
{{\"suggestions\": [{{\"anchor\": <int>, \"mode\": \"after\"|\"replace\", \"markdown\": \"...\"}}], \
\"note\": \"<one short sentence for the room, or empty>\"}}"
    )
}

/// Split a markdown suggestion into top-level pieces we can materialise as
/// Tiptap nodes: headings, fenced code, paragraphs (blank-line separated).
fn pieces(md: &str) -> Vec<(String, Vec<(String, String)>, String)> {
    // (tag, attrs, text)
    let mut out = Vec::new();
    let mut lines = md.lines().peekable();
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, out: &mut Vec<(String, Vec<(String, String)>, String)>| {
        if !para.is_empty() {
            out.push(("paragraph".into(), vec![], para.join("\n").trim().to_string()));
            para.clear();
        }
    };
    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        if let Some(rest) = line.strip_prefix("```") {
            flush(&mut para, &mut out);
            let lang = rest.trim().to_string();
            let mut body = Vec::new();
            for l in lines.by_ref() {
                if l.starts_with("```") {
                    break;
                }
                body.push(l);
            }
            out.push(("codeBlock".into(), vec![("language".into(), lang)], body.join("\n")));
            continue;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line.chars().nth(hashes) == Some(' ') {
            flush(&mut para, &mut out);
            out.push((
                "heading".into(),
                vec![("level".into(), hashes.to_string())],
                line[hashes + 1..].trim().to_string(),
            ));
            continue;
        }
        para.push(line);
    }
    flush(&mut para, &mut out);
    out.retain(|(_, _, t)| !t.trim().is_empty());
    out
}

/// Apply the model's suggestions into the live doc as marked nodes. Returns
/// how many nodes landed. Anchors are resolved against the SAME numbering
/// the model saw (unaccepted suggestions excluded), so a suggestion always
/// lands where the model meant.
fn apply(hot: &HotState, doc_id: Uuid, reply: &Reply) -> Result<usize, String> {
    let mut sessions = hot.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let session = sessions.get_mut(&doc_id).ok_or("session ended")?;
    let ydoc = session.awareness.doc();
    let frag = ydoc.get_or_insert_xml_fragment("default");
    let mut txn = ydoc.transact_mut();
    // map visible block number → fragment index (skipping pending suggestions)
    let mut visible: Vec<u32> = Vec::new();
    for i in 0..frag.len(&txn) {
        if let Some(XmlOut::Element(el)) = frag.get(&txn, i) {
            let pending = matches!(
                el.get_attribute(&txn, "suggestion").map(|v| v.to_string(&txn)).as_deref(),
                Some("insert") | Some("replace")
            );
            if !pending {
                visible.push(i);
            }
        }
    }
    let mut landed = 0usize;
    // apply bottom-up so earlier indices stay valid
    let mut items: Vec<&SuggestionOut> = reply.suggestions.iter().take(MAX_SUGGESTIONS).collect();
    items.sort_by(|a, b| b.anchor.cmp(&a.anchor));
    for s in items {
        let ps = pieces(&s.markdown);
        if ps.is_empty() {
            continue;
        }
        let id = Uuid::now_v7().to_string();
        let (mode, mut at) = match s.mode.as_str() {
            "replace" if s.anchor >= 1 && s.anchor <= visible.len() => {
                let target = visible[s.anchor - 1];
                if let Some(XmlOut::Element(el)) = frag.get(&txn, target) {
                    el.insert_attribute(&mut txn, "suggestion", "replaced");
                    el.insert_attribute(&mut txn, "suggestionId", id.clone());
                    el.insert_attribute(&mut txn, "suggestionBy", AGENT_NAME);
                }
                ("replace", target + 1)
            }
            _ => {
                let after = s.anchor.min(visible.len());
                let at = if after == 0 { 0 } else { visible[after - 1] + 1 };
                ("insert", at)
            }
        };
        for (tag, attrs, text) in ps {
            let el = frag.insert(&mut txn, at, yrs::XmlElementPrelim::empty(tag.as_str()));
            for (k, v) in attrs {
                el.insert_attribute(&mut txn, k.as_str(), v);
            }
            el.insert_attribute(&mut txn, "suggestion", mode);
            el.insert_attribute(&mut txn, "suggestionId", id.clone());
            el.insert_attribute(&mut txn, "suggestionBy", AGENT_NAME);
            let t = el.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
            t.insert(&mut txn, 0, &text);
            at += 1;
            landed += 1;
        }
    }
    drop(txn);
    session.last_activity = std::time::Instant::now();
    if landed > 0 {
        session.agents.insert(AGENT_NAME.to_string());
    }
    Ok(landed)
}

/// Show the agent in the room while it works (awareness user state on the
/// daemon's own client), and drop it after.
fn presence(hot: &HotState, doc_id: Uuid, present: bool) {
    let mut sessions = hot.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let Some(session) = sessions.get_mut(&doc_id) else { return };
    let state = if present {
        Some(serde_json::json!({"user": {"name": format!("🌿 {AGENT_NAME}"), "color": "#95c99b", "agent": true}}).to_string())
    } else {
        None
    };
    let ok = match state {
        Some(s) => session.awareness.set_local_state(s).is_ok(),
        None => {
            session.awareness.clean_local_state();
            true
        }
    };
    if ok && let Ok(update) = session.awareness.update() {
        let mut enc = EncoderV1::new();
        YMessage::Awareness(update).encode(&mut enc);
        session.tx.send(enc.to_vec()).ok();
    }
}

fn participants_line(hot: &HotState, doc_id: Uuid, store: &SqliteStore) -> String {
    let sessions = hot.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let Some(session) = sessions.get(&doc_id) else { return "unknown".into() };
    let contacts = store.list_contacts().unwrap_or_default();
    let mut names: Vec<String> = session
        .participants
        .iter()
        .map(|p| match p {
            None => "the owner".to_string(),
            Some(pk) => contacts
                .iter()
                .find(|c| &c.pubkey == pk)
                .map(|c| c.petname.clone())
                .unwrap_or_else(|| "a guest".into()),
        })
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        "the owner".into()
    } else {
        names.join(", ")
    }
}

/// The agent principal every room ask runs under (created on first use).
pub fn agent_principal(store: &mut SqliteStore) -> grimoire_store::Result<Uuid> {
    if let Some(p) = store
        .list_principals()?
        .into_iter()
        .find(|p| p.kind == PrincipalKind::Agent && p.display_name == AGENT_NAME)
    {
        return Ok(p.id);
    }
    Ok(store.create_principal(PrincipalKind::Agent, AGENT_NAME, None)?.id)
}

/// One ask, end to end. Runs in the background; progress via `hot.agent`.
pub async fn ask(
    hot: HotState,
    store: Arc<Mutex<SqliteStore>>,
    doc_id: Uuid,
    instruction: String,
) -> Result<usize, String> {
    let blocks = numbered_blocks(&hot, doc_id).ok_or("doc is not in a live session")?;
    let who = {
        let hot = hot.clone();
        with_store(&store, move |s| -> Result<String, String> {
            agent_principal(s).map_err(|e| e.to_string())?;
            Ok(participants_line(&hot, doc_id, s))
        })
        .await?
    };
    let prompt = compose(&blocks, instruction.trim(), &who);
    presence(&hot, doc_id, true);
    let result = crate::garden::invoke_claude_bounded(&prompt, WALL_CLOCK).await;
    let out = match result {
        Ok((text, _tokens)) => {
            let reply: Reply = crate::garden::parse_json_result(&text)?;
            let n = apply(&hot, doc_id, &reply)?;
            hot.set_agent_note(doc_id, reply.note.filter(|n| !n.trim().is_empty()));
            Ok(n)
        }
        Err(e) => Err(e),
    };
    presence(&hot, doc_id, false);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_split_headings_code_and_paragraphs() {
        let ps = pieces("## Title\n\nfirst para\nstill first\n\n```rust\nfn x() {}\n```\n\nlast");
        let tags: Vec<&str> = ps.iter().map(|p| p.0.as_str()).collect();
        assert_eq!(tags, vec!["heading", "paragraph", "codeBlock", "paragraph"]);
        assert_eq!(ps[0].1, vec![("level".to_string(), "2".to_string())]);
        assert_eq!(ps[1].2, "first para\nstill first");
        assert_eq!(ps[2].1, vec![("language".to_string(), "rust".to_string())]);
        assert_eq!(ps[2].2, "fn x() {}");
    }

    #[test]
    fn compose_numbers_blocks_and_carries_the_ask() {
        let p = compose(&["one".into(), "two".into()], "tighten this", "the owner, alice");
        assert!(p.contains("[1] one"));
        assert!(p.contains("[2] two"));
        assert!(p.contains("\"tighten this\""));
        assert!(p.contains("the owner, alice"));
        assert!(p.contains("JSON ONLY"));
    }

    /// Suggestions land as marked nodes at the right place, and the numbered
    /// view the NEXT ask sees skips them (so anchors stay stable); accepted
    /// text is what the flatten renders.
    #[test]
    fn suggestions_land_marked_and_are_invisible_to_render_until_accepted() {
        let dir = std::env::temp_dir().join(format!("grimoire-room-{}", Uuid::now_v7()));
        let hot = HotState::new(dir);
        let doc = Uuid::now_v7();
        hot.start(doc, 0).unwrap();
        {
            let sessions = hot.sessions.lock().unwrap();
            let s = sessions.get(&doc).unwrap();
            let frag = s.awareness.doc().get_or_insert_xml_fragment("default");
            let mut txn = s.awareness.doc().transact_mut();
            for (i, text) in ["alpha", "beta", "gamma"].iter().enumerate() {
                let el = frag.insert(&mut txn, i as u32, yrs::XmlElementPrelim::empty("paragraph"));
                let t = el.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
                t.insert(&mut txn, 0, text);
            }
        }
        let reply = Reply {
            suggestions: vec![
                SuggestionOut { anchor: 1, mode: "after".into(), markdown: "after alpha".into() },
                SuggestionOut { anchor: 3, mode: "replace".into(), markdown: "GAMMA".into() },
            ],
            note: None,
        };
        assert_eq!(apply(&hot, doc, &reply).unwrap(), 2);
        // the model's view next time: the same three, unchanged
        assert_eq!(numbered_blocks(&hot, doc).unwrap(), vec!["alpha", "beta", "gamma"]);
        // the flatten view: identical — nothing accepted yet
        {
            let sessions = hot.sessions.lock().unwrap();
            let s = sessions.get(&doc).unwrap();
            let frag = s.awareness.doc().get_or_insert_xml_fragment("default");
            let txn = s.awareness.doc().transact();
            assert_eq!(crate::yrender::fragment_to_markdown(&txn, &frag), "alpha\n\nbeta\n\ngamma");
            // raw fragment: 5 nodes, suggestions marked, gamma marked replaced
            assert_eq!(frag.len(&txn), 5);
            let attr = |i: u32, k: &str| match frag.get(&txn, i) {
                Some(XmlOut::Element(el)) => el.get_attribute(&txn, k).map(|v| v.to_string(&txn)),
                _ => None,
            };
            assert_eq!(attr(1, "suggestion").as_deref(), Some("insert"));
            assert_eq!(attr(3, "suggestion").as_deref(), Some("replaced"));
            assert_eq!(attr(4, "suggestion").as_deref(), Some("replace"));
            assert_eq!(attr(4, "suggestionBy").as_deref(), Some(AGENT_NAME));
        }
        // accept the replace the way the editor does: strip attrs on the new,
        // delete the original → render shows GAMMA
        {
            let sessions = hot.sessions.lock().unwrap();
            let s = sessions.get(&doc).unwrap();
            let frag = s.awareness.doc().get_or_insert_xml_fragment("default");
            let mut txn = s.awareness.doc().transact_mut();
            if let Some(XmlOut::Element(el)) = frag.get(&txn, 4) {
                el.remove_attribute(&mut txn, &"suggestion");
            }
            frag.remove_range(&mut txn, 3, 1);
            drop(txn);
            let txn = s.awareness.doc().transact();
            assert_eq!(crate::yrender::fragment_to_markdown(&txn, &frag), "alpha\n\nbeta\n\nGAMMA");
        }
    }
}
