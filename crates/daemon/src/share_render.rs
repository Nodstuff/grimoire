//! Share links: a snapshot's markdown → the public page (pulldown-cmark).
//!
//! - Raw HTML is never rendered: an HTML block shows as code, inline HTML as
//!   text (both escaped).
//! - Links keep http, https, mailto and `#fragment` destinations; anything
//!   else (`javascript:`, `data:`, relative paths, other schemes) becomes `#`.
//! - Images: `taisce-asset:<name>` → the share's asset URL (or a `data:` URL
//!   in a preview); a name the snapshot lacks → a "missing image" box. Any
//!   other image is not loaded (the page's CSP allows only its own images):
//!   it renders as a link to it.
//! - Frontmatter is stripped. Each top-level block carries `data-b="<n>"`.

use pulldown_cmark::{CodeBlockKind, CowStr, Event, MetadataBlockKind, Options, Parser, Tag, TagEnd};
use std::collections::HashMap;

/// What the renderer needs to know about one asset.
pub struct AssetInfo {
    /// the `src` to use
    pub url: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

/// HTML-escape text for an element body or a quoted attribute.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A link destination the page may carry, or None (rendered as `#`).
pub fn safe_href(dest: &str) -> Option<String> {
    let d = dest.trim();
    if d.starts_with('#') && !d.chars().any(char::is_control) {
        return Some(d.to_string());
    }
    // WHATWG parsing, as a browser would (it drops tabs and newlines, so
    // `java\tscript:` is still javascript)
    let u = url::Url::parse(d).ok()?;
    matches!(u.scheme(), "http" | "https" | "mailto").then(|| u.to_string())
}

/// Drop a leading `---` frontmatter block.
pub fn strip_frontmatter(md: &str) -> &str {
    let md = md.strip_prefix('\u{feff}').unwrap_or(md);
    let Some(rest) = md.strip_prefix("---\n").or_else(|| md.strip_prefix("---\r\n")) else { return md };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        at += line.len();
        if matches!(line.trim_end_matches(['\r', '\n']), "---" | "...") {
            return &rest[at..];
        }
    }
    md
}

const ASSET_SCHEME: &str = "taisce-asset:";

fn image_html(dest: &str, alt: &str, title: &str, assets: &HashMap<String, AssetInfo>) -> String {
    let alt_e = esc(alt);
    let title_attr = if title.is_empty() { String::new() } else { format!(" title=\"{}\"", esc(title)) };
    if let Some(name) = dest.trim().strip_prefix(ASSET_SCHEME) {
        return match assets.get(name) {
            Some(a) => {
                let dim = |k: &str, v: Option<i64>| v.filter(|v| *v > 0).map(|v| format!(" {k}=\"{v}\"")).unwrap_or_default();
                format!(
                    "<img src=\"{}\" alt=\"{alt_e}\"{title_attr}{}{} loading=\"lazy\" decoding=\"async\">",
                    esc(&a.url),
                    dim("width", a.width),
                    dim("height", a.height)
                )
            }
            None => format!(
                "<span class=\"missing-image\" role=\"img\" aria-label=\"missing image\">missing image{}</span>",
                if alt.is_empty() { String::new() } else { format!(": {alt_e}") }
            ),
        };
    }
    let label = if alt.is_empty() { "image".to_string() } else { alt_e };
    match safe_href(dest) {
        Some(h) => format!("<a class=\"ext-image\" href=\"{}\" rel=\"nofollow noopener noreferrer\">{label}</a>", esc(&h)),
        None => format!("<span class=\"ext-image\">{label}</span>"),
    }
}

/// Render a snapshot's markdown to the page body.
pub fn render_body(markdown: &str, assets: &HashMap<String, AssetInfo>) -> String {
    let md = strip_frontmatter(markdown);
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut events: Vec<Event> = Vec::new();
    let mut parser = Parser::new_ext(md, opts);
    let mut in_meta = false;
    while let Some(ev) = parser.next() {
        match ev {
            Event::Start(Tag::MetadataBlock(_)) => in_meta = true,
            Event::End(TagEnd::MetadataBlock(MetadataBlockKind::YamlStyle | MetadataBlockKind::PlusesStyle)) => in_meta = false,
            _ if in_meta => {}
            // raw HTML: shown, never rendered
            Event::Start(Tag::HtmlBlock) => events.push(Event::Start(Tag::CodeBlock(CodeBlockKind::Indented))),
            Event::End(TagEnd::HtmlBlock) => events.push(Event::End(TagEnd::CodeBlock)),
            Event::Html(s) | Event::InlineHtml(s) => events.push(Event::Text(s)),
            Event::Start(Tag::Link { dest_url, title, .. }) => {
                let href = safe_href(&dest_url).unwrap_or_else(|| "#".into());
                let title = if title.is_empty() { String::new() } else { format!(" title=\"{}\"", esc(&title)) };
                events.push(Event::Html(CowStr::from(format!(
                    "<a href=\"{}\"{title} rel=\"nofollow noopener noreferrer\">",
                    esc(&href)
                ))));
            }
            // the start became an Html event, so the end must too (block
            // numbering counts Start/End pairs)
            Event::End(TagEnd::Link) => events.push(Event::Html(CowStr::Borrowed("</a>"))),
            Event::Start(Tag::Image { dest_url, title, .. }) => {
                // the alt text is everything up to the matching end
                let (mut alt, mut depth) = (String::new(), 1);
                for inner in parser.by_ref() {
                    match inner {
                        Event::Start(_) => depth += 1,
                        Event::End(_) => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        Event::Text(t) | Event::Code(t) | Event::Html(t) | Event::InlineHtml(t) => alt.push_str(&t),
                        Event::SoftBreak | Event::HardBreak => alt.push(' '),
                        _ => {}
                    }
                }
                events.push(Event::Html(CowStr::from(image_html(&dest_url, &alt, &title, assets))));
            }
            ev => events.push(ev),
        }
    }
    // one top-level block at a time, so each can carry its index
    let mut out = String::with_capacity(md.len() * 2);
    let (mut depth, mut start, mut n) = (0usize, 0usize, 0usize);
    for (i, ev) in events.iter().enumerate() {
        let opens = matches!(ev, Event::Start(_));
        let closes = matches!(ev, Event::End(_));
        if opens {
            depth += 1;
        }
        if closes {
            depth = depth.saturating_sub(1);
        }
        if depth == 0 {
            let mut block = String::new();
            pulldown_cmark::html::push_html(&mut block, events[start..=i].iter().cloned());
            start = i + 1;
            let block = block.trim_start();
            if block.is_empty() {
                continue;
            }
            out.push_str(&tag_block(block, n));
            out.push('\n');
            n += 1;
        }
    }
    out
}

/// `data-b="<n>"` on a block's first element.
fn tag_block(html: &str, n: usize) -> String {
    let named = html.starts_with('<') && html[1..].starts_with(|c: char| c.is_ascii_alphabetic());
    if !named {
        return format!("<div data-b=\"{n}\">{html}</div>");
    }
    let end = html[1..].find(|c: char| !c.is_ascii_alphanumeric()).map_or(html.len(), |e| e + 1);
    format!("{} data-b=\"{n}\"{}", &html[..end], &html[end..])
}

/// The page around a body. `nonce` = the comment script runs (the public
/// page); None = no script at all (the preview and PDF export).
pub struct PageParts<'a> {
    pub title: &'a str,
    pub body_html: &'a str,
    pub theme: &'a str,
    /// the snapshot's date, `YYYY-MM-DD`
    pub snapshot_date: &'a str,
    pub nonce: Option<&'a str>,
    pub comments_enabled: bool,
}

pub const PAGE_CSS: &str = include_str!("share/page.css");
pub const PAGE_JS: &str = include_str!("share/page.js");

pub fn page(p: &PageParts) -> String {
    let theme = match p.theme {
        "light" | "dark" => p.theme,
        _ => "auto",
    };
    let script = match p.nonce {
        Some(n) => format!("<script nonce=\"{}\">{PAGE_JS}</script>", esc(n)),
        None => String::new(),
    };
    let comments = p.nonce.is_some() && p.comments_enabled;
    let panel = if comments {
        "<aside id=\"comments\" class=\"comments\" aria-label=\"Comments\"><h2>Comments</h2><div id=\"threads\"></div></aside>"
    } else {
        ""
    };
    format!(
        "<!doctype html>\n<html lang=\"en\" data-theme=\"{theme}\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<meta name=\"robots\" content=\"noindex, nofollow\"><meta name=\"referrer\" content=\"no-referrer\">\
<meta name=\"color-scheme\" content=\"{scheme}\">\
<title>{title}</title><style>{PAGE_CSS}</style></head>\
<body class=\"{layout}\" data-comments=\"{on}\"><div class=\"wrap\"><main class=\"doc\"><header><h1 class=\"doc-title\">{title}</h1></header>\
<article id=\"doc\">{body}</article>\
<footer class=\"foot\">Shared from Taisce · snapshot of {date}</footer></main>{panel}</div>{script}</body></html>\n",
        scheme = if theme == "auto" { "light dark" } else { theme },
        title = esc(p.title),
        layout = if comments { "with-comments" } else { "plain" },
        on = if comments { "on" } else { "off" },
        body = p.body_html,
        date = esc(p.snapshot_date),
    )
}

/// The page a dead link answers with (410), or an unknown one (404).
pub fn gone_page(status_text: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\" data-theme=\"auto\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta name=\"robots\" content=\"noindex, nofollow\">\
<title>Taisce</title><style>{PAGE_CSS}</style></head><body class=\"plain\"><div class=\"wrap\"><main class=\"doc gone\">\
<h1 class=\"doc-title\">{}</h1><p>Ask whoever sent it for a new link.</p></main></div></body></html>\n",
        esc(status_text)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> HashMap<String, AssetInfo> {
        HashMap::from([("d1.svg".to_string(), AssetInfo { url: "/s/T/a/d1.svg".into(), width: Some(640), height: Some(320) })])
    }

    #[test]
    fn raw_html_is_escaped_and_never_rendered() {
        let out = render_body("hi <script>alert(1)</script> there\n\n<div onclick=\"x()\">block</div>\n\n<img src=x onerror=alert(1)>", &assets());
        assert!(!out.contains("<script"), "{out}");
        assert!(!out.contains("<div onclick"), "{out}");
        assert!(!out.contains("<img src=x"), "{out}");
        assert!(out.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "{out}");
        assert!(out.contains("&lt;div onclick="), "{out}");
    }

    #[test]
    fn javascript_and_other_schemes_are_neutralised() {
        for bad in [
            "[x](javascript:alert(1))",
            "[x](JaVaScRiPt:alert(1))",
            "[x](java\tscript:alert(1))",
            "[x]( javascript:alert(1))",
            "[x](data:text/html;base64,PHNjcmlwdD4=)",
            "[x](vbscript:msgbox)",
            "<javascript:alert(1)>",
            "[x][r]\n\n[r]: javascript:alert(1)",
        ] {
            let out = render_body(bad, &assets());
            // the text may show it (or it may not parse as a link at all);
            // any href is the neutralised one
            assert_eq!(out.matches("href=").count(), out.matches("href=\"#\"").count(), "{bad} → {out}");
            assert!(!out.contains("<a href=\"j") && !out.contains("<a href=\"d") && !out.contains("<a href=\"v"), "{bad} → {out}");
        }
        assert_eq!(render_body("[x](javascript:alert(1))", &assets()).matches("href=\"#\"").count(), 1);
        for raw in ["java\tscript:alert(1)", "java\nscript:alert(1)", " JAVASCRIPT:alert(1)", "data:text/html,x", "/relative", "taisce:doc"] {
            assert_eq!(safe_href(raw), None, "{raw:?}");
        }
        let out = render_body("[ok](https://example.com/a?b=1&c=\"2\") [m](mailto:a@b.c) [f](#top)", &assets());
        assert!(out.contains("href=\"https://example.com/a?b=1&amp;c=%222%22\""), "{out}");
        assert!(out.contains("href=\"mailto:a@b.c\"") && out.contains("href=\"#top\""), "{out}");
        assert!(out.contains("rel=\"nofollow noopener noreferrer\""), "{out}");
    }

    #[test]
    fn assets_are_rewritten_and_missing_ones_boxed() {
        let out = render_body("![diagram](taisce-asset:d1.svg)\n\n![gone](taisce-asset:nope.svg)\n\n![ext](https://x.test/i.png)", &assets());
        assert!(out.contains("<img src=\"/s/T/a/d1.svg\" alt=\"diagram\" width=\"640\" height=\"320\""), "{out}");
        assert!(out.contains("missing image: gone"), "{out}");
        assert!(!out.contains("<img src=\"https://x.test"), "{out}");
        assert!(!out.contains("taisce-asset:"), "{out}");
    }

    #[test]
    fn frontmatter_is_stripped_and_blocks_are_numbered() {
        let out = render_body("---\ntags: [secret]\n---\n# Title\n\npara\n\n- a\n- b\n\n```rust\nfn x() {}\n```\n", &assets());
        assert!(!out.contains("secret"), "{out}");
        assert!(out.contains("<h1 data-b=\"0\">Title</h1>"), "{out}");
        assert!(out.contains("<p data-b=\"1\">para</p>"), "{out}");
        assert!(out.contains("<ul data-b=\"2\">"), "{out}");
        assert!(out.contains("<pre data-b=\"3\">"), "{out}");
    }

    #[test]
    fn the_page_escapes_its_title_and_carries_the_nonce() {
        let p = page(&PageParts {
            title: "<b>T</b>",
            body_html: "<p>x</p>",
            theme: "dark",
            snapshot_date: "2026-10-06",
            nonce: Some("abc"),
            comments_enabled: true,
        });
        assert!(p.contains("&lt;b&gt;T&lt;/b&gt;") && !p.contains("<b>T</b>"));
        assert!(p.contains("<script nonce=\"abc\">") && p.contains("data-theme=\"dark\""));
        assert!(p.contains("Shared from Taisce · snapshot of 2026-10-06"));
        let p = page(&PageParts { title: "T", body_html: "", theme: "x", snapshot_date: "d", nonce: None, comments_enabled: true });
        assert!(!p.contains("<script") && p.contains("data-theme=\"auto\"") && !p.contains("id=\"comments\""));
    }
}
