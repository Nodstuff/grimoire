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
//! - A link with sub-pages: `[[Title]]` / `[[Title|label]]` naming one of the
//!   link's pages links to it; any other wiki link stays the text it was. The
//!   page then carries the tree of pages (`Nav`) beside the doc.

use pulldown_cmark::{CodeBlockKind, CowStr, Event, LinkType, MetadataBlockKind, Options, Parser, Tag, TagEnd};
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

fn image_html(dest: &str, alt: &str, title: &str, assets: &HashMap<String, AssetInfo>, lazy: bool) -> String {
    let alt_e = esc(alt);
    let title_attr = if title.is_empty() { String::new() } else { format!(" title=\"{}\"", esc(title)) };
    if let Some(name) = dest.trim().strip_prefix(ASSET_SCHEME) {
        return match assets.get(name) {
            Some(a) => {
                let dim = |k: &str, v: Option<i64>| v.filter(|v| *v > 0).map(|v| format!(" {k}=\"{v}\"")).unwrap_or_default();
                format!(
                    "<img src=\"{}\" alt=\"{alt_e}\"{title_attr}{}{}{}>",
                    esc(&a.url),
                    dim("width", a.width),
                    dim("height", a.height),
                    if lazy { " loading=\"lazy\" decoding=\"async\"" } else { "" }
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

/// Render a snapshot's markdown to the page body (images load lazily).
#[cfg_attr(not(test), allow(dead_code))]
pub fn render_body(markdown: &str, assets: &HashMap<String, AssetInfo>) -> String {
    render_body_with(markdown, assets, true, &PageLinks::default())
}

/// The pages of a link a `[[wiki link]]` may name: title → that page's URL.
#[derive(Default)]
pub struct PageLinks(HashMap<String, String>);

impl PageLinks {
    /// Titles compare trimmed and case-insensitively; the first page of a
    /// title wins (tree order, the root first).
    pub fn new(pages: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut m = HashMap::new();
        for (title, url) in pages {
            m.entry(Self::key(&title)).or_insert(url);
        }
        Self(m)
    }

    fn key(title: &str) -> String {
        title.trim().to_lowercase()
    }

    /// `[[Title]]`, or `[[Title#Heading]]` (the heading is dropped: the
    /// page's headings carry no ids).
    fn get(&self, dest: &str) -> Option<&String> {
        self.0.get(&Self::key(dest)).or_else(|| dest.split_once('#').and_then(|(t, _)| self.0.get(&Self::key(t))))
    }
}

/// `lazy: false` for a page that is printed (the PDF preview): a printed
/// page never scrolls, so a lazy image would never load.
pub fn render_body_with(markdown: &str, assets: &HashMap<String, AssetInfo>, lazy: bool, pages: &PageLinks) -> String {
    let md = strip_frontmatter(markdown);
    let mut opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    // only a link with pages reads wiki links at all: a single doc's
    // `[[x]]` stays exactly the text it has always been
    if !pages.0.is_empty() {
        opts |= Options::ENABLE_WIKILINKS;
    }
    // each open link: does its end close an <a>, or a wiki link left as text?
    let mut links: Vec<bool> = Vec::new();
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
            Event::Start(Tag::Link { link_type: LinkType::WikiLink { has_pothole }, dest_url, .. }) => match pages.get(&dest_url) {
                Some(url) => {
                    links.push(true);
                    events.push(Event::Html(CowStr::from(format!("<a class=\"page-link\" href=\"{}\">", esc(url)))));
                }
                // not one of this link's pages: the text as it was written
                None => {
                    links.push(false);
                    events.push(Event::Text(CowStr::from(if has_pothole { format!("[[{dest_url}|") } else { "[[".to_string() })));
                }
            },
            Event::Start(Tag::Link { link_type, dest_url, title, .. }) => {
                links.push(true);
                // an email autolink (<a@b.c>) arrives without its scheme
                let dest = match link_type {
                    LinkType::Email if !dest_url.contains(':') => format!("mailto:{dest_url}"),
                    _ => dest_url.to_string(),
                };
                let href = safe_href(&dest).unwrap_or_else(|| "#".into());
                let title = if title.is_empty() { String::new() } else { format!(" title=\"{}\"", esc(&title)) };
                events.push(Event::Html(CowStr::from(format!(
                    "<a href=\"{}\"{title} rel=\"nofollow noopener noreferrer\">",
                    esc(&href)
                ))));
            }
            // the start became an Html event, so the end must too (block
            // numbering counts Start/End pairs)
            Event::End(TagEnd::Link) => events.push(match links.pop() {
                Some(false) => Event::Text(CowStr::Borrowed("]]")),
                _ => Event::Html(CowStr::Borrowed("</a>")),
            }),
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
                events.push(Event::Html(CowStr::from(image_html(&dest_url, &alt, &title, assets, lazy))));
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
    /// the link's tree of pages, when it has sub-pages
    pub nav: Option<&'a Nav>,
}

/// One page in a link's tree, as the navigation lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavItem {
    pub title: String,
    pub url: String,
    /// 0 = the root, 1 = directly under it, …
    pub depth: usize,
}

/// A link's pages in reading order (the root first), and which one this is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nav {
    pub items: Vec<NavItem>,
    pub current: usize,
}

impl Nav {
    /// Tree order from (slug, parent slug, title) rows in their stored
    /// order: each page under its parent, a page whose parent is not in
    /// the link directly under the root. `current`: a slug, None = the root.
    pub fn build(root_title: &str, root_url: &str, pages: &[(String, Option<String>, String)], current: Option<&str>) -> Self {
        let known: std::collections::HashSet<&str> = pages.iter().map(|p| p.0.as_str()).collect();
        let mut kids: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
        for (i, (_, parent, _)) in pages.iter().enumerate() {
            kids.entry(parent.as_deref().filter(|p| known.contains(p))).or_default().push(i);
        }
        let mut items = vec![NavItem { title: root_title.to_string(), url: root_url.to_string(), depth: 0 }];
        let mut cur = 0;
        // depth-first, without recursion (a cycle cannot arise: every page
        // is visited from one parent, and unvisited ones are appended below)
        let mut seen = vec![false; pages.len()];
        let mut stack: Vec<(usize, usize)> = kids.get(&None).map(|v| v.iter().rev().map(|i| (*i, 1)).collect()).unwrap_or_default();
        while let Some((i, depth)) = stack.pop() {
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            let (slug, _, title) = &pages[i];
            if current == Some(slug.as_str()) {
                cur = items.len();
            }
            items.push(NavItem { title: title.clone(), url: format!("{root_url}/p/{slug}"), depth: depth.min(6) });
            if let Some(v) = kids.get(&Some(slug.as_str())) {
                stack.extend(v.iter().rev().map(|k| (*k, depth + 1)));
            }
        }
        for (i, (slug, _, title)) in pages.iter().enumerate() {
            if !seen[i] {
                if current == Some(slug.as_str()) {
                    cur = items.len();
                }
                items.push(NavItem { title: title.clone(), url: format!("{root_url}/p/{slug}"), depth: 1 });
            }
        }
        Self { items, current: cur }
    }

    fn list(&self) -> String {
        let mut out = String::from("<ul>");
        for (i, it) in self.items.iter().enumerate() {
            let here = if i == self.current { " aria-current=\"page\"" } else { "" };
            out.push_str(&format!("<li class=\"d{}\"><a href=\"{}\"{here}>{}</a></li>", it.depth, esc(&it.url), esc(&it.title)));
        }
        out.push_str("</ul>");
        out
    }

    /// The tree twice: a sidebar on a wide screen, a closed "Pages" fold
    /// above the doc on a narrow one (CSS shows one of them).
    fn html(&self) -> (String, String) {
        let list = self.list();
        (
            format!("<nav class=\"pages pages-side\" aria-label=\"Pages\">{list}</nav>"),
            format!(
                "<details class=\"pages pages-fold\"><summary>Pages <span class=\"n\">{} of {}</span></summary><nav aria-label=\"Pages\">{list}</nav></details>",
                self.current + 1,
                self.items.len()
            ),
        )
    }

    /// Previous and next page, at the doc's foot.
    fn pager(&self) -> String {
        let link = |i: usize, rel: &str, label: &str| {
            let it = &self.items[i];
            format!("<a rel=\"{rel}\" class=\"{rel}\" href=\"{}\"><span>{label}</span>{}</a>", esc(&it.url), esc(&it.title))
        };
        let prev = self.current.checked_sub(1).map(|i| link(i, "prev", "Previous")).unwrap_or_else(|| "<span></span>".into());
        let next = if self.current + 1 < self.items.len() { link(self.current + 1, "next", "Next") } else { String::new() };
        format!("<nav class=\"pager\" aria-label=\"Previous and next page\">{prev}{next}</nav>")
    }
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
    // the toggle's bar above the page, and the drawer it slides out
    let (bar, drawer) = if comments {
        (
            "<div id=\"topbar\" class=\"topbar\" hidden><button id=\"comments-toggle\" class=\"comments-toggle\" type=\"button\" aria-controls=\"comments\" aria-expanded=\"false\">\
Comments<span id=\"comments-count\" class=\"count\"></span></button></div>",
            "<aside id=\"comments\" class=\"comments\" aria-label=\"Comments\"><div class=\"comments-head\"><h2>Comments</h2>\
<button id=\"comments-close\" class=\"close\" type=\"button\" aria-label=\"Close comments\">\u{00d7}</button></div>\
<div id=\"threads\"></div></aside>",
        )
    } else {
        ("", "")
    };
    let (side, fold, pager) = match p.nav {
        Some(n) if n.items.len() > 1 => {
            let (side, fold) = n.html();
            (side, fold, n.pager())
        }
        _ => Default::default(),
    };
    // a sub-page's tab reads "Page · Root"
    let tab = match p.nav.and_then(|n| n.items.first().filter(|_| n.current > 0)) {
        Some(root) => format!("{} \u{00b7} {}", esc(p.title), esc(&root.title)),
        None => esc(p.title),
    };
    format!(
        "<!doctype html>\n<html lang=\"en\" data-theme=\"{theme}\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<meta name=\"robots\" content=\"noindex, nofollow\"><meta name=\"referrer\" content=\"no-referrer\">\
<meta name=\"color-scheme\" content=\"{scheme}\">\
<title>{tab}</title><style>{PAGE_CSS}</style></head>\
<body class=\"{layout}{with_nav}\" data-comments=\"{on}\">{bar}<div class=\"wrap\">{side}<main class=\"doc\">{fold}<header><h1 class=\"doc-title\">{title}</h1></header>\
<article id=\"doc\">{body}</article>{pager}\
<footer class=\"foot\">Shared from Taisce · snapshot of {date}</footer></main>{drawer}</div>{script}</body></html>\n",
        scheme = if theme == "auto" { "light dark" } else { theme },
        title = esc(p.title),
        with_nav = if side.is_empty() { "" } else { " with-nav" },
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
    fn email_autolinks_become_mailto() {
        let out = render_body("write to <aoife@example.com> or [me](mailto:tom@example.com)", &assets());
        assert!(out.contains("<a href=\"mailto:aoife@example.com\""), "{out}");
        assert!(out.contains("<a href=\"mailto:tom@example.com\""), "{out}");
    }

    #[test]
    fn a_printed_page_loads_its_images_eagerly() {
        let lazy = render_body("![d](taisce-asset:d1.svg)", &assets());
        let eager = render_body_with("![d](taisce-asset:d1.svg)", &assets(), false, &PageLinks::default());
        assert!(lazy.contains("loading=\"lazy\""), "{lazy}");
        assert!(!eager.contains("loading=") && eager.contains("<img src=\"/s/T/a/d1.svg\""), "{eager}");
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
    fn wiki_links_reach_the_links_own_pages_and_nothing_else() {
        let pages = PageLinks::new([
            ("Root".to_string(), "/s/T".to_string()),
            ("Fleet Twin".to_string(), "/s/T/p/abc".to_string()),
            ("fleet twin".to_string(), "/s/T/p/second".to_string()),
        ]);
        let md = "see [[Fleet Twin]], [[ fleet TWIN |the twin]], [[Fleet Twin#Costs]], [[Root]] and [[Secret Doc]] or [[Secret Doc|that]]\n\n`[[Fleet Twin]]`";
        let out = render_body_with(md, &assets(), true, &pages);
        assert_eq!(out.matches("<a class=\"page-link\" href=\"/s/T/p/abc\">").count(), 3, "{out}");
        assert!(out.contains("<a class=\"page-link\" href=\"/s/T/p/abc\">the twin</a>"), "{out}");
        assert!(out.contains("<a class=\"page-link\" href=\"/s/T\">Root</a>"), "{out}");
        assert!(!out.contains("second"), "the first page of a title wins: {out}");
        // not a page of this link: the text as written, no link at all
        assert!(out.contains("[[Secret Doc]] or [[Secret Doc|that]]"), "{out}");
        assert!(out.contains("<code>[[Fleet Twin]]</code>"), "code is left alone: {out}");
        assert_eq!(out.matches("<a ").count(), 4, "{out}");
        // a single-doc link reads no wiki links: byte for byte what it was
        let plain = render_body("see [[Fleet Twin]] and [[a|b]]", &assets());
        assert_eq!(plain.trim_end(), "<p data-b=\"0\">see [[Fleet Twin]] and [[a|b]]</p>");
        // a page title cannot smuggle markup or a scheme into the href
        let evil = PageLinks::new([("x".to_string(), "/s/T/p/\"><script>".to_string())]);
        let out = render_body_with("[[x]]", &assets(), true, &evil);
        assert!(!out.contains("<script>"), "{out}");
    }

    #[test]
    fn the_nav_is_the_tree_in_reading_order() {
        let row = |s: &str, p: Option<&str>, t: &str| (s.to_string(), p.map(str::to_string), t.to_string());
        // stored order is not tree order; one parent is not in the link
        let pages = [row("b", None, "B"), row("a1", Some("a"), "A one"), row("a", None, "A <i>"), row("lost", Some("gone"), "Lost")];
        let nav = Nav::build("Root", "/s/T", &pages, Some("a1"));
        assert_eq!(
            nav.items.iter().map(|i| (i.title.as_str(), i.depth, i.url.as_str())).collect::<Vec<_>>(),
            [("Root", 0, "/s/T"), ("B", 1, "/s/T/p/b"), ("A <i>", 1, "/s/T/p/a"), ("A one", 2, "/s/T/p/a1"), ("Lost", 1, "/s/T/p/lost")]
        );
        assert_eq!(nav.current, 3);
        assert_eq!(Nav::build("Root", "/s/T", &pages, None).current, 0);
        // a parent loop cannot hang or drop a page
        let looped = [row("x", Some("y"), "X"), row("y", Some("x"), "Y")];
        assert_eq!(Nav::build("R", "/s/T", &looped, None).items.len(), 3);
        let p = page(&PageParts { title: "A one", body_html: "<p>x</p>", theme: "auto", snapshot_date: "d", nonce: None, comments_enabled: false, nav: Some(&nav) });
        assert!(p.contains("<title>A one \u{00b7} Root</title>"), "{p}");
        assert!(p.contains("<body class=\"plain with-nav\""), "{p}");
        assert!(p.contains("<li class=\"d2\"><a href=\"/s/T/p/a1\" aria-current=\"page\">A one</a></li>"), "{p}");
        assert!(p.contains("A &lt;i&gt;") && !p.contains("A <i>"), "titles are escaped: {p}");
        assert_eq!(p.matches(" aria-current=\"page\">A one</a>").count(), 2, "the sidebar and the fold");
        assert!(p.contains("<a rel=\"prev\" class=\"prev\" href=\"/s/T/p/a\">") && p.contains("<a rel=\"next\" class=\"next\" href=\"/s/T/p/lost\">"), "{p}");
        assert!(p.contains("Pages <span class=\"n\">4 of 5</span>"), "{p}");
        // a link with no sub-pages has no nav at all
        let solo = Nav::build("Root", "/s/T", &[], None);
        let p = page(&PageParts { title: "Root", body_html: "", theme: "auto", snapshot_date: "d", nonce: None, comments_enabled: false, nav: Some(&solo) });
        assert!(!p.contains("class=\"pages") && p.contains("<body class=\"plain\"") && !p.contains("<nav"), "{p}");
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
            nav: None,
        });
        assert!(p.contains("&lt;b&gt;T&lt;/b&gt;") && !p.contains("<b>T</b>"));
        assert!(p.contains("<script nonce=\"abc\">") && p.contains("data-theme=\"dark\""));
        assert!(p.contains("Shared from Taisce · snapshot of 2026-10-06"));
        // comments are a drawer behind a toggle in a bar above the page: the
        // bar comes before the doc, the drawer after it, both closed at first
        let (bar, drawer, doc) = (p.find("id=\"topbar\"").unwrap(), p.find("id=\"comments\"").unwrap(), p.find("id=\"doc\"").unwrap());
        assert!(bar < doc && doc < drawer, "{p}");
        assert!(p.contains("aria-expanded=\"false\"") && p.contains("<body class=\"with-comments\""));
        let p = page(&PageParts { title: "T", body_html: "", theme: "x", snapshot_date: "d", nonce: None, comments_enabled: true, nav: None });
        assert!(!p.contains("<script") && p.contains("data-theme=\"auto\"") && !p.contains("id=\"comments\"") && !p.contains("topbar\""));
    }
}
