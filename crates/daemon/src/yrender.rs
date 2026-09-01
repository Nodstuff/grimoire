//! Render a y-prosemirror XmlFragment (our Tiptap schema) back to GFM
//! markdown (#67, ADR 0003 §5) — the daemon-side half of the flatten. The
//! output feeds `mddiff::markdown_to_ops`, which LCS-matches against the
//! existing blocks so unchanged content keeps its ids (and comment anchors).
//!
//! Node coverage mirrors ui/src/editor/markdown.ts: paragraph, heading,
//! codeBlock, blockquote, bulletList/orderedList/listItem, horizontalRule,
//! table (GFM), hardBreak; marks bold/italic/strike/code/link. Unknown nodes
//! degrade to their inline text rather than being dropped.

use yrs::types::xml::{XmlFragment, XmlOut};
use yrs::{Any, GetString, Out, ReadTxn, Text, XmlElementRef, XmlTextRef};

pub fn fragment_to_markdown<T: ReadTxn>(txn: &T, frag: &yrs::XmlFragmentRef) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for child in frag.children(txn) {
        if let Some(md) = render_node(txn, &child) {
            if !md.trim().is_empty() {
                blocks.push(md);
            }
        }
    }
    blocks.join("\n\n")
}

fn render_node<T: ReadTxn>(txn: &T, node: &XmlOut) -> Option<String> {
    match node {
        XmlOut::Element(el) => Some(render_element(txn, el)),
        XmlOut::Text(t) => Some(render_text(txn, t)),
        XmlOut::Fragment(_) => None,
    }
}

fn attr_str<T: ReadTxn>(txn: &T, el: &XmlElementRef, name: &str) -> Option<String> {
    use yrs::Xml;
    match el.get_attribute(txn, name)? {
        Out::Any(Any::String(s)) => Some(s.to_string()),
        Out::Any(Any::Number(n)) => Some(n.to_string()),
        Out::Any(Any::BigInt(n)) => Some(n.to_string()),
        other => Some(format!("{other:?}")),
    }
}

fn render_element<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> String {
    let tag = el.tag().to_string();
    match tag.as_str() {
        "paragraph" => inline_content(txn, el),
        "heading" => {
            let level: usize = attr_str(txn, el, "level")
                .and_then(|l| l.parse().ok())
                .unwrap_or(1);
            format!("{} {}", "#".repeat(level.clamp(1, 6)), inline_content(txn, el))
        }
        "codeBlock" => {
            let lang = attr_str(txn, el, "language").unwrap_or_default();
            let body = raw_text(txn, el);
            format!("```{lang}\n{body}\n```")
        }
        "blockquote" => children_blocks(txn, el)
            .join("\n\n")
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        "bulletList" => render_list(txn, el, None),
        "orderedList" => render_list(txn, el, Some(1)),
        "horizontalRule" => "---".into(),
        "table" => render_table(txn, el),
        "hardBreak" => "\\\n".into(),
        // unknown block-ish node: degrade to its content
        _ => inline_content(txn, el),
    }
}

fn children_blocks<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> Vec<String> {
    el.children(txn)
        .filter_map(|c| render_node(txn, &c))
        .filter(|s| !s.trim().is_empty())
        .collect()
}

fn render_list<T: ReadTxn>(txn: &T, el: &XmlElementRef, ordered_from: Option<usize>) -> String {
    let mut out = Vec::new();
    let mut n = ordered_from.unwrap_or(0);
    for item in el.children(txn) {
        let XmlOut::Element(item) = item else { continue };
        if item.tag().as_ref() != "listItem" {
            continue;
        }
        let inner = children_blocks(txn, &item).join("\n\n");
        let marker = match ordered_from {
            Some(_) => {
                let m = format!("{n}. ");
                n += 1;
                m
            }
            None => "- ".into(),
        };
        let indent = " ".repeat(marker.len());
        let mut lines = inner.lines();
        let first = lines.next().unwrap_or_default();
        let mut rendered = format!("{marker}{first}");
        for l in lines {
            rendered.push('\n');
            if l.is_empty() {
                continue; // blank separators inside items flatten out
            }
            rendered.push_str(&indent);
            rendered.push_str(l);
        }
        out.push(rendered);
    }
    out.join("\n")
}

fn render_table<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for row in el.children(txn) {
        let XmlOut::Element(row) = row else { continue };
        if row.tag().as_ref() != "tableRow" {
            continue;
        }
        let mut cells = Vec::new();
        for cell in row.children(txn) {
            let XmlOut::Element(cell) = cell else { continue };
            // a cell holds block content; tables in markdown are single-line
            let text = children_blocks(txn, &cell).join(" ").replace('\n', " ");
            cells.push(text.replace('|', "\\|"));
        }
        rows.push(cells);
    }
    if rows.is_empty() {
        return String::new();
    }
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(1);
    let line = |cells: &[String]| {
        let mut padded: Vec<String> = cells.to_vec();
        padded.resize(width, String::new());
        format!("| {} |", padded.join(" | "))
    };
    let mut out = vec![line(&rows[0])];
    out.push(format!("|{}|", vec![" --- "; width].join("|")));
    for r in &rows[1..] {
        out.push(line(r));
    }
    out.join("\n")
}

/// Inline content of a node: its XmlText children rendered with marks, plus
/// nested inline elements (hardBreak).
fn inline_content<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> String {
    let mut out = String::new();
    for child in el.children(txn) {
        match child {
            XmlOut::Text(t) => out.push_str(&render_text(txn, &t)),
            XmlOut::Element(e) if e.tag().as_ref() == "hardBreak" => out.push_str("\\\n"),
            XmlOut::Element(e) => out.push_str(&inline_content(txn, &e)),
            XmlOut::Fragment(_) => {}
        }
    }
    out
}

/// Unformatted text (code blocks).
fn raw_text<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> String {
    let mut out = String::new();
    for child in el.children(txn) {
        if let XmlOut::Text(t) = child {
            out.push_str(&t.get_string(txn));
        }
    }
    out
}

/// Marked text runs → markdown inline syntax.
fn render_text<T: ReadTxn>(txn: &T, t: &XmlTextRef) -> String {
    let mut out = String::new();
    for chunk in t.diff(txn, yrs::types::text::YChange::identity) {
        let Out::Any(Any::String(text)) = chunk.insert else {
            continue;
        };
        let mut piece = text.to_string();
        let mut href: Option<String> = None;
        let mut bold = false;
        let mut italic = false;
        let mut strike = false;
        let mut code = false;
        if let Some(attrs) = &chunk.attributes {
            for (k, v) in attrs.iter() {
                match k.as_ref() {
                    "bold" | "strong" => bold = true,
                    "italic" | "em" => italic = true,
                    "strike" => strike = true,
                    "code" => code = true,
                    "link" => {
                        href = match v {
                            Any::Map(m) => m.get("href").map(|h| match h {
                                Any::String(s) => s.to_string(),
                                other => format!("{other:?}"),
                            }),
                            Any::String(s) => Some(s.to_string()),
                            _ => None,
                        }
                    }
                    _ => {}
                }
            }
        }
        if code {
            piece = format!("`{piece}`");
        } else {
            if bold {
                piece = format!("**{piece}**");
            }
            if italic {
                piece = format!("*{piece}*");
            }
            if strike {
                piece = format!("~~{piece}~~");
            }
        }
        if let Some(href) = href {
            piece = format!("[{piece}]({href})");
        }
        out.push_str(&piece);
    }
    out
}
