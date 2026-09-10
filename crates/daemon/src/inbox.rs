//! Quick capture: `POST /api/inbox {text}` files a note under the root doc
//! titled **Inbox** as the HUMAN principal (this is the human typing — the
//! ⌘⇧I palette, the ⌥⌘G global hotkey, or a curl). The doc is titled from
//! the first line, carries the whole text as its body and is tagged `inbox`
//! in frontmatter so the filing gardener can find it later.

use crate::api::ApiState;
use crate::store_ext::with_store;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use grimoire_store::{BlockStore, Doc, SqliteStore};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub const INBOX_TITLE: &str = "Inbox";
const MAX_TITLE_CHARS: usize = 80;

/// The doc title for a captured note: its first non-blank line with list
/// bullets, heading hashes and checkbox markers stripped, cut to 80 chars on
/// a char boundary (never mid-grapheme). Empty text → "Untitled".
pub fn inbox_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut s = line;
    // repeated so "- # heading" and "# - item" both reduce to the words
    loop {
        let before = s;
        s = s.trim_start_matches(['#', '-', '*', '>']).trim_start();
        for marker in ["[ ]", "[x]", "[X]"] {
            if let Some(rest) = s.strip_prefix(marker) {
                s = rest.trim_start();
            }
        }
        if s == before {
            break;
        }
    }
    let s = s.trim_end();
    if s.is_empty() {
        return "Untitled".into();
    }
    if s.chars().count() <= MAX_TITLE_CHARS {
        return s.to_string();
    }
    let cut: String = s.chars().take(MAX_TITLE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The body every captured doc gets: `inbox` frontmatter tag + the text as
/// written.
pub fn inbox_markdown(text: &str) -> String {
    format!("---\ntags:\n  - inbox\n---\n\n{}\n", text.trim_end())
}

/// The root doc titled Inbox, created (by `principal`) when absent. Never a
/// duplicate: an existing root with that title is reused, whichever
/// principal made it.
pub fn find_or_create_inbox(s: &mut SqliteStore, principal: Uuid) -> grimoire_store::Result<Doc> {
    if let Some(d) = s
        .list_docs()?
        .into_iter()
        .find(|d| d.parent_id.is_none() && d.title == INBOX_TITLE)
    {
        return Ok(d);
    }
    s.create_doc(INBOX_TITLE, None, principal)
}

#[derive(Deserialize)]
struct CaptureReq {
    text: String,
}

async fn capture(State(st): State<ApiState>, Json(req): Json<CaptureReq>) -> Json<Value> {
    if req.text.trim().is_empty() {
        return Json(json!({"error": "nothing to capture"}));
    }
    let human = st.human;
    with_store(&st.store, move |s| {
        let inbox = match find_or_create_inbox(s, human) {
            Ok(d) => d,
            Err(e) => return Json(json!({"error": e.to_string()})),
        };
        let title = inbox_title(&req.text);
        let md = inbox_markdown(&req.text);
        let ops = grimoire_store::mddiff::markdown_to_ops(&[], &md);
        match s.create_doc_with_ops(&title, Some(inbox.id), human, ops) {
            Ok((doc, _)) => Json(json!({"doc_id": doc.id, "title": doc.title, "inbox_id": inbox.id})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

pub fn router(state: ApiState) -> Router {
    Router::new().route("/api/inbox", post(capture)).with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_the_first_line_without_markers() {
        assert_eq!(inbox_title("- call the bank\nmore"), "call the bank");
        assert_eq!(inbox_title("## Idea: ship it"), "Idea: ship it");
        assert_eq!(inbox_title("\n\n  - [ ] todo item  \n"), "todo item");
        assert_eq!(inbox_title("plain text"), "plain text");
        assert_eq!(inbox_title("   \n\n"), "Untitled");
        assert_eq!(inbox_title("###"), "Untitled");
    }

    #[test]
    fn title_is_capped_at_80_chars_on_a_char_boundary() {
        let long = "é".repeat(200);
        let t = inbox_title(&long);
        assert_eq!(t.chars().count(), 80);
        assert!(t.ends_with('…'));
        let exact = "x".repeat(80);
        assert_eq!(inbox_title(&exact), exact);
    }

    #[tokio::test]
    async fn capture_creates_inbox_once_and_files_tagged_docs_under_it() {
        use crate::home::testing::{app, call};
        let (app, human) = app();
        let a = call(&app, "POST", "/api/inbox", Some(json!({"text": "- call the bank\nabout the mortgage"}))).await;
        assert_eq!(a["title"], "call the bank", "{a}");
        let inbox_id = a["inbox_id"].as_str().unwrap().to_string();
        let b = call(&app, "POST", "/api/inbox", Some(json!({"text": "second thought"}))).await;
        assert_eq!(b["inbox_id"], inbox_id, "Inbox must be reused, not duplicated");
        let docs = call(&app, "GET", "/api/docs", None).await;
        let docs = docs.as_array().unwrap();
        assert_eq!(docs.iter().filter(|d| d["title"] == INBOX_TITLE).count(), 1);
        let inbox = docs.iter().find(|d| d["title"] == INBOX_TITLE).unwrap();
        assert!(inbox["parent_id"].is_null());
        assert_eq!(inbox["created_by"], human.to_string());
        let note = docs.iter().find(|d| d["id"] == a["doc_id"]).unwrap();
        assert_eq!(note["parent_id"], inbox_id);
        assert_eq!(note["created_by"], human.to_string(), "captured as the human");
        // body = frontmatter + the text; the tag is queryable
        let tree = call(&app, "GET", &format!("/api/doc/{}", a["doc_id"].as_str().unwrap()), None).await;
        let roots = tree["roots"].as_array().unwrap();
        assert!(roots[0]["block"]["content"].as_str().unwrap().starts_with("---\ntags:\n  - inbox"));
        assert_eq!(tree["doc"]["current_epoch"], 1);
        let all: String = roots
            .iter()
            .map(|r| r["block"]["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("call the bank") && all.contains("about the mortgage"), "{all}");
        let tags = call(&app, "GET", "/api/tags", None).await;
        assert!(tags.as_array().unwrap().iter().any(|t| t[0] == "inbox"), "{tags}");
        // empty text is refused
        let e = call(&app, "POST", "/api/inbox", Some(json!({"text": "  \n"}))).await;
        assert!(e["error"].is_string());
    }

    #[test]
    fn markdown_carries_the_inbox_tag_and_the_text() {
        let md = inbox_markdown("- call the bank\nabout the thing\n\n");
        assert!(md.starts_with("---\ntags:\n  - inbox\n---\n\n"));
        assert!(md.ends_with("- call the bank\nabout the thing\n"));
    }
}
