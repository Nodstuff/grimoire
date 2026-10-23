//! EPUB 3 rendering of Taisce docs (`Taisce › EPUB proposal`, Phase 2): a
//! [`BookSpec`] (chapters of block markdown, already gathered in the
//! reader's scope by `sentdocs.rs`) → the bytes of one `.epub`.
//!
//! The package:
//!
//! - `mimetype` first, STORED, no extra field (the OCF rule readers sniff);
//!   `META-INF/container.xml` → `OEBPS/content.opf`
//! - the OPF: `dc:identifier` = `urn:uuid:<doc id>` (stable for the doc's
//!   life), `dc:title`, `dc:creator` (the workspace, else "Taisce"),
//!   `dc:language`, `dcterms:modified` from the doc's last applied change
//! - `nav.xhtml` (EPUB 3 nav, nested by depth) and `toc.ncx` (EPUB 2
//!   readers, KOReader's older paths), one `chNNN.xhtml` per chapter, a
//!   small e-ink stylesheet, and the diagram PNGs the web app uploaded
//!
//! Content rules: raw HTML is shown as text, never rendered; links keep
//! http/https/mailto only (anything else is its text); a `[[wikilink]]` to
//! a doc inside the book is an internal link to its chapter, any other is
//! plain text (its alias, else its title); external images are never
//! fetched (the alt text stands in); a diagram fence (mermaid, d2,
//! reladraw, vega, vega-lite) is its stored picture when one matches the
//! fence's current source ([`source_hash`]), else its source in a code box.
//! Comments, canvases and frontmatter never reach the spec.
//!
//! DETERMINISTIC: the same spec gives the same bytes — a fixed file order,
//! a fixed zip timestamp (1980-01-01), fixed permissions, sorted maps, no
//! randomness — so an unchanged doc never looks updated to a reader.

use crate::share_render::{esc, safe_href};
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
use sha2::Digest as _;
use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use uuid::Uuid;

/// Fence languages drawn as diagrams (the editor's VISUAL_LANGS plus vega).
pub const DIAGRAM_LANGS: &[&str] = &["mermaid", "d2", "reladraw", "vega", "vega-lite"];

pub const EPUB_CONTENT_TYPE: &str = "application/epub+zip";

/// One block of a chapter: its id (diagram pictures key on it) and its
/// markdown exactly as stored.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpecBlock {
    pub id: Uuid,
    pub markdown: String,
}

/// One chapter: a doc, at its depth in the book's table of contents (1 =
/// top level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub doc_id: Uuid,
    pub title: String,
    pub depth: usize,
    pub blocks: Vec<SpecBlock>,
}

/// A stored diagram picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub png: Vec<u8>,
    pub width: i64,
    pub height: i64,
}

/// Everything a render reads. `pictures` keys on (block id, source hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookSpec {
    /// the book's identity: the tagged doc's id
    pub id: Uuid,
    pub title: String,
    pub creator: String,
    pub language: String,
    /// `YYYY-MM-DDTHH:MM:SSZ`
    pub modified: String,
    pub chapters: Vec<Chapter>,
    pub pictures: BTreeMap<(Uuid, String), Picture>,
}

/// The code a fence is compared by: lines without trailing spaces, outer
/// blank lines dropped (the web app's `codeKey`).
fn normalize_code(code: &str) -> String {
    code.split('\n').map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// The hash a diagram picture is keyed by: SHA-256 hex of `lang + "\n" +`
/// the normalized code. An edited fence hashes differently, so its old
/// picture is never used.
pub fn source_hash(lang: &str, code: &str) -> String {
    hex::encode(sha2::Sha256::digest(format!("{}\n{}", lang.to_ascii_lowercase(), normalize_code(code)).as_bytes()))
}

fn opts() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_WIKILINKS
}

/// The fence language a diagram is drawn from (`None` = not a diagram).
fn diagram_lang(info: &str) -> Option<String> {
    let lang = info.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    DIAGRAM_LANGS.contains(&lang.as_str()).then_some(lang)
}

/// Every diagram fence in a block's markdown: (lang, code).
pub fn diagram_fences(markdown: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for ev in Parser::new_ext(markdown, opts()) {
        match ev {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                cur = diagram_lang(&info).map(|l| (l, String::new()));
            }
            Event::Text(t) => {
                if let Some((_, code)) = cur.as_mut() {
                    code.push_str(&t);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
            }
            _ => {}
        }
    }
    out
}

/// The chapter file of chapter `i` (0-based).
fn chapter_file(i: usize) -> String {
    format!("ch{:03}.xhtml", i + 1)
}

fn picture_file(block: Uuid, hash: &str) -> String {
    format!("images/{}-{}.png", block.simple(), &hash[..hash.len().min(12)])
}

/// A wikilink target's doc title: the last path segment, without an
/// anchor or a `.md` suffix.
fn wikilink_title(dest: &str) -> String {
    let d = dest.split('#').next().unwrap_or("");
    let d = d.rsplit('/').next().unwrap_or(d);
    d.strip_suffix(".md").unwrap_or(d).trim().to_string()
}

/// What the renderer resolves against: chapter titles → files, pictures.
struct Ctx<'a> {
    titles: HashMap<String, usize>,
    pictures: &'a BTreeMap<(Uuid, String), Picture>,
}

impl Ctx<'_> {
    fn chapter_of(&self, title: &str) -> Option<usize> {
        self.titles.get(title).or_else(|| self.titles.get(&title.to_lowercase())).copied()
    }
}

/// One block's markdown → XHTML (well-formed XML: pulldown-cmark closes
/// every element and self-closes void ones).
fn render_block(block: &SpecBlock, ctx: &Ctx) -> String {
    let mut events: Vec<Event> = Vec::new();
    // what each open link became: true = an <a> was emitted
    let mut links: Vec<bool> = Vec::new();
    let mut parser = Parser::new_ext(&block.markdown, opts());
    while let Some(ev) = parser.next() {
        match ev {
            Event::Start(Tag::HtmlBlock) => events.push(Event::Start(Tag::CodeBlock(CodeBlockKind::Indented))),
            Event::End(TagEnd::HtmlBlock) => events.push(Event::End(TagEnd::CodeBlock)),
            Event::Html(s) | Event::InlineHtml(s) => events.push(Event::Text(s)),
            Event::TaskListMarker(done) => events.push(Event::Text(if done { "\u{2611} ".into() } else { "\u{2610} ".into() })),
            Event::Start(Tag::Link { link_type: LinkType::WikiLink { .. }, dest_url, .. }) => {
                match ctx.chapter_of(&wikilink_title(&dest_url)) {
                    Some(i) => {
                        events.push(Event::Html(format!("<a href=\"{}\">", chapter_file(i)).into()));
                        links.push(true);
                    }
                    None => links.push(false),
                }
            }
            Event::Start(Tag::Link { link_type, dest_url, .. }) => {
                let dest = match link_type {
                    LinkType::Email if !dest_url.contains(':') => format!("mailto:{dest_url}"),
                    _ => dest_url.to_string(),
                };
                match safe_href(&dest).filter(|h| !h.starts_with('#')) {
                    Some(h) => {
                        events.push(Event::Html(format!("<a href=\"{}\">", esc(&h)).into()));
                        links.push(true);
                    }
                    None => links.push(false),
                }
            }
            Event::End(TagEnd::Link) => {
                if links.pop().unwrap_or(false) {
                    events.push(Event::Html("</a>".into()));
                }
            }
            Event::Start(Tag::Image { .. }) => {
                // never fetched: the alt text stands in
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
                        Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                        Event::SoftBreak | Event::HardBreak => alt.push(' '),
                        _ => {}
                    }
                }
                let label = if alt.trim().is_empty() { "image".to_string() } else { alt };
                events.push(Event::Html(format!("<span class=\"image-alt\">[{}]</span>", esc(&label)).into()));
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) if diagram_lang(&info).is_some() => {
                let lang = diagram_lang(&info).unwrap_or_default();
                let mut code = String::new();
                for inner in parser.by_ref() {
                    match inner {
                        Event::End(TagEnd::CodeBlock) => break,
                        Event::Text(t) => code.push_str(&t),
                        _ => {}
                    }
                }
                let hash = source_hash(&lang, &code);
                let html = match ctx.pictures.get(&(block.id, hash.clone())) {
                    Some(p) => {
                        let dim = |k: &str, v: i64| if v > 0 { format!(" {k}=\"{v}\"") } else { String::new() };
                        format!(
                            "<figure class=\"diagram\"><img src=\"{}\" alt=\"{} diagram\"{}{} /></figure>\n",
                            picture_file(block.id, &hash),
                            esc(&lang),
                            dim("width", p.width),
                            dim("height", p.height)
                        )
                    }
                    None => format!(
                        "<figure class=\"diagram-source\"><figcaption>{} diagram (source)</figcaption><pre><code>{}</code></pre></figure>\n",
                        esc(&lang),
                        esc(&code)
                    ),
                };
                events.push(Event::Html(html.into()));
            }
            ev => events.push(ev),
        }
    }
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events.into_iter());
    out
}

const CSS: &str = "body { font-family: serif; line-height: 1.45; margin: 0 0.4em; }
h1, h2, h3, h4, h5, h6 { font-family: sans-serif; line-height: 1.2; margin: 1.1em 0 0.4em; page-break-after: avoid; }
h1 { font-size: 1.6em; } h2 { font-size: 1.3em; } h3 { font-size: 1.15em; } h4, h5, h6 { font-size: 1em; }
p { margin: 0.5em 0; text-align: left; }
a { color: inherit; text-decoration: underline; }
blockquote { margin: 0.6em 0 0.6em 0.4em; padding-left: 0.8em; border-left: 3px solid #000; }
code { font-family: monospace; font-size: 0.9em; }
pre { font-family: monospace; font-size: 0.8em; white-space: pre-wrap; word-wrap: break-word; border: 1px solid #000; padding: 0.5em; margin: 0.6em 0; }
pre code { font-size: 1em; }
table { border-collapse: collapse; margin: 0.6em 0; font-size: 0.9em; }
th, td { border: 1px solid #000; padding: 0.2em 0.4em; vertical-align: top; }
th { font-weight: bold; }
ul, ol { margin: 0.4em 0; padding-left: 1.4em; }
hr { border: 0; border-top: 1px solid #000; margin: 1em 0; }
figure { margin: 0.8em 0; text-align: center; page-break-inside: avoid; }
figure img { max-width: 100%; height: auto; }
figcaption { font-family: sans-serif; font-size: 0.8em; text-align: left; }
.image-alt { font-style: italic; }
nav ol { list-style: none; padding-left: 1em; }
";

fn xhtml_page(title: &str, body: &str, nav: bool) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"en\" lang=\"en\">\n\
<head>\n<meta charset=\"UTF-8\" />\n<title>{}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\" />\n</head>\n\
<body{}>\n{body}</body>\n</html>\n",
        esc(title),
        if nav { " epub:type=\"frontmatter\"" } else { "" }
    )
}

/// The first content block repeats the chapter's title as a `# Title`
/// heading (most docs open that way): then it is the heading.
fn opens_with_title(blocks: &[SpecBlock], title: &str) -> bool {
    let Some(first) = blocks.first() else { return false };
    let line = first.markdown.trim();
    line.strip_prefix("# ").is_some_and(|h| h.trim() == title.trim()) && !line.contains('\n')
}

fn chapter_xhtml(ch: &Chapter, ctx: &Ctx) -> String {
    let mut body = String::from("<section epub:type=\"chapter\">\n");
    if !opens_with_title(&ch.blocks, &ch.title) {
        body.push_str(&format!("<h1>{}</h1>\n", esc(&ch.title)));
    }
    for b in &ch.blocks {
        body.push_str(&render_block(b, ctx));
    }
    body.push_str("</section>\n");
    xhtml_page(&ch.title, &body, false)
}

/// The chapters as a tree, from the depth-first list: the top-level
/// chapters and each chapter's children (a depth that jumps by more than one
/// nests under the nearest shallower chapter).
fn toc_tree(chapters: &[Chapter]) -> (Vec<usize>, HashMap<usize, Vec<usize>>) {
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (depth, index)
    let (mut top, mut kids) = (Vec::new(), HashMap::<usize, Vec<usize>>::new());
    for (i, ch) in chapters.iter().enumerate() {
        let d = ch.depth.max(1);
        while stack.last().is_some_and(|(sd, _)| *sd >= d) {
            stack.pop();
        }
        match stack.last() {
            Some((_, p)) => kids.entry(*p).or_default().push(i),
            None => top.push(i),
        }
        stack.push((d, i));
    }
    (top, kids)
}

fn nav_ol(out: &mut String, items: &[usize], chapters: &[Chapter], kids: &dyn Fn(usize) -> Vec<usize>) {
    out.push_str("<ol>\n");
    for &i in items {
        out.push_str(&format!("<li><a href=\"{}\">{}</a>", chapter_file(i), esc(&chapters[i].title)));
        let k = kids(i);
        if !k.is_empty() {
            out.push('\n');
            nav_ol(out, &k, chapters, kids);
        }
        out.push_str("</li>\n");
    }
    out.push_str("</ol>\n");
}

fn ncx_points(out: &mut String, items: &[usize], chapters: &[Chapter], kids: &dyn Fn(usize) -> Vec<usize>) {
    for &i in items {
        out.push_str(&format!(
            "<navPoint id=\"np{n}\" playOrder=\"{n}\"><navLabel><text>{}</text></navLabel><content src=\"{}\"/>\n",
            esc(&chapters[i].title),
            chapter_file(i),
            n = i + 1
        ));
        ncx_points(out, &kids(i), chapters, kids);
        out.push_str("</navPoint>\n");
    }
}

/// Every file of the package, in zip order (mimetype handled separately).
fn package_files(spec: &BookSpec) -> Vec<(String, Vec<u8>, bool)> {
    let mut titles = HashMap::new();
    for (i, ch) in spec.chapters.iter().enumerate() {
        titles.entry(ch.title.clone()).or_insert(i);
        titles.entry(ch.title.to_lowercase()).or_insert(i);
    }
    let ctx = Ctx { titles, pictures: &spec.pictures };
    let chapters: Vec<String> = spec.chapters.iter().map(|c| chapter_xhtml(c, &ctx)).collect();
    // only the pictures a chapter actually uses go in the package
    let used: Vec<(&(Uuid, String), &Picture)> =
        spec.pictures.iter().filter(|((b, h), _)| chapters.iter().any(|c| c.contains(&picture_file(*b, h)))).collect();

    let (top, all) = toc_tree(&spec.chapters);
    let kids = |i: usize| all.get(&i).cloned().unwrap_or_default();

    let mut nav = String::from("<nav epub:type=\"toc\" id=\"toc\">\n<h1>Contents</h1>\n");
    nav_ol(&mut nav, &top, &spec.chapters, &kids);
    nav.push_str("</nav>\n");
    let nav = xhtml_page("Contents", &nav, true);

    let depth = spec.chapters.iter().map(|c| c.depth.max(1)).max().unwrap_or(1);
    let mut points = String::new();
    ncx_points(&mut points, &top, &spec.chapters, &kids);
    let ncx = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n\
<head>\n<meta name=\"dtb:uid\" content=\"urn:uuid:{id}\"/>\n<meta name=\"dtb:depth\" content=\"{depth}\"/>\n\
<meta name=\"dtb:totalPageCount\" content=\"0\"/>\n<meta name=\"dtb:maxPageNumber\" content=\"0\"/>\n</head>\n\
<docTitle><text>{title}</text></docTitle>\n<navMap>\n{points}</navMap>\n</ncx>\n",
        id = spec.id,
        title = esc(&spec.title),
    );

    let mut manifest = String::from(
        "<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n\
<item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n\
<item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n",
    );
    let mut spine = String::new();
    for i in 0..spec.chapters.len() {
        manifest.push_str(&format!("<item id=\"c{n}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>\n", chapter_file(i), n = i + 1));
        spine.push_str(&format!("<itemref idref=\"c{}\"/>\n", i + 1));
    }
    for (n, ((b, h), _)) in used.iter().enumerate() {
        manifest.push_str(&format!("<item id=\"img{}\" href=\"{}\" media-type=\"image/png\"/>\n", n + 1, picture_file(*b, h)));
    }
    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bookid\" xml:lang=\"{lang}\">\n\
<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
<dc:identifier id=\"bookid\">urn:uuid:{id}</dc:identifier>\n<dc:title>{title}</dc:title>\n<dc:creator>{creator}</dc:creator>\n\
<dc:language>{lang}</dc:language>\n<meta property=\"dcterms:modified\">{modified}</meta>\n</metadata>\n\
<manifest>\n{manifest}</manifest>\n<spine toc=\"ncx\">\n{spine}</spine>\n</package>\n",
        id = spec.id,
        title = esc(&spec.title),
        creator = esc(&spec.creator),
        lang = esc(&spec.language),
        modified = esc(&spec.modified),
    );

    let container = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n\
<rootfiles>\n<rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/>\n</rootfiles>\n</container>\n";

    // (path, bytes, compress)
    let mut files = vec![
        ("META-INF/container.xml".to_string(), container.as_bytes().to_vec(), true),
        ("OEBPS/content.opf".to_string(), opf.into_bytes(), true),
        ("OEBPS/nav.xhtml".to_string(), nav.into_bytes(), true),
        ("OEBPS/toc.ncx".to_string(), ncx.into_bytes(), true),
        ("OEBPS/style.css".to_string(), CSS.as_bytes().to_vec(), true),
    ];
    for (i, c) in chapters.into_iter().enumerate() {
        files.push((format!("OEBPS/{}", chapter_file(i)), c.into_bytes(), true));
    }
    for ((b, h), p) in used {
        // PNG is compressed already
        files.push((format!("OEBPS/{}", picture_file(*b, h)), p.png.clone(), false));
    }
    files
}

/// Render the book. Infallible in practice (an in-memory zip); an error
/// here is a bug, reported as such.
pub fn render(spec: &BookSpec) -> anyhow::Result<Vec<u8>> {
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, DateTime};
    let when = DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).map_err(|e| anyhow::anyhow!("zip time: {e}"))?;
    let base = SimpleFileOptions::default().last_modified_time(when).unix_permissions(0o644);
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    w.start_file("mimetype", base.compression_method(CompressionMethod::Stored))?;
    w.write_all(EPUB_CONTENT_TYPE.as_bytes())?;
    for (path, bytes, compress) in package_files(spec) {
        let o = if compress {
            base.compression_method(CompressionMethod::Deflated).compression_level(Some(6))
        } else {
            base.compression_method(CompressionMethod::Stored)
        };
        w.start_file(path, o)?;
        w.write_all(&bytes)?;
    }
    Ok(w.finish()?.into_inner())
}

/// A key for the render cache: SHA-256 over every input, in order.
pub fn spec_key(spec: &BookSpec) -> String {
    let mut h = sha2::Sha256::new();
    let mut put = |s: &[u8]| {
        h.update((s.len() as u64).to_le_bytes());
        h.update(s);
    };
    put(spec.id.as_bytes());
    put(spec.title.as_bytes());
    put(spec.creator.as_bytes());
    put(spec.language.as_bytes());
    put(spec.modified.as_bytes());
    for c in &spec.chapters {
        put(c.doc_id.as_bytes());
        put(c.title.as_bytes());
        put(&(c.depth as u64).to_le_bytes());
        for b in &c.blocks {
            put(b.id.as_bytes());
            put(b.markdown.as_bytes());
        }
    }
    for ((b, hash), p) in &spec.pictures {
        put(b.as_bytes());
        put(hash.as_bytes());
        put(&sha2::Sha256::digest(&p.png));
        put(&p.width.to_le_bytes());
        put(&p.height.to_le_bytes());
    }
    hex::encode(h.finalize())
}

/// A small in-memory cache of rendered books by [`spec_key`]: a reader's
/// HEAD-then-GET, and the feed's sizes, render once.
pub struct RenderCache {
    entries: std::sync::Mutex<std::collections::VecDeque<(String, std::sync::Arc<Vec<u8>>)>>,
}

const CACHE_ENTRIES: usize = 16;
const CACHE_BYTES: usize = 64 * 1024 * 1024;

impl RenderCache {
    pub const fn new() -> Self {
        Self { entries: std::sync::Mutex::new(std::collections::VecDeque::new()) }
    }

    /// The rendered bytes of `spec`, from the cache or freshly made.
    pub fn get_or_render(&self, spec: &BookSpec) -> anyhow::Result<std::sync::Arc<Vec<u8>>> {
        let key = spec_key(spec);
        {
            let mut e = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(pos) = e.iter().position(|(k, _)| *k == key) {
                let hit = e.remove(pos).expect("present");
                let bytes = hit.1.clone();
                e.push_front(hit);
                return Ok(bytes);
            }
        }
        let bytes = std::sync::Arc::new(render(spec)?);
        let mut e = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        e.push_front((key, bytes.clone()));
        while e.len() > CACHE_ENTRIES || (e.len() > 1 && e.iter().map(|(_, b)| b.len()).sum::<usize>() > CACHE_BYTES) {
            e.pop_back();
        }
        Ok(bytes)
    }
}

/// The process's render cache.
pub static CACHE: RenderCache = RenderCache::new();

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Read as _;

    /// A block with an id derived from its text (a sample is the same
    /// spec every time it is built).
    fn block(md: &str) -> SpecBlock {
        let h = sha2::Sha256::digest(md.as_bytes());
        SpecBlock { id: Uuid::from_bytes(h[..16].try_into().unwrap()), markdown: md.into() }
    }

    pub(crate) fn sample() -> BookSpec {
        let root = Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0001);
        let diagram = SpecBlock { id: Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_00d1), markdown: "```mermaid\ngraph TD\n  A --> B\n```".into() };
        let pictured = SpecBlock { id: Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_00d2), markdown: "```d2\nx -> y\n```".into() };
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
        let pictures = BTreeMap::from([((pictured.id, source_hash("d2", "x -> y")), Picture { png, width: 1200, height: 600 })]);
        BookSpec {
            id: root,
            title: "Field notes".into(),
            creator: "Work".into(),
            language: "en".into(),
            modified: "2026-10-23T09:30:00Z".into(),
            chapters: vec![
                Chapter {
                    doc_id: Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0002),
                    title: "Getting started".into(),
                    depth: 1,
                    blocks: vec![
                        block("# Getting started"),
                        block("Some *prose* with a [[Reference]] link, an [outside](https://example.com/a?b=1&c=2) one, and [[Elsewhere|a doc not in this book]]."),
                        block("```rust\nfn main() { println!(\"<hi>\"); }\n```"),
                        block("- [x] done\n- [ ] todo"),
                        block("<script>alert(1)</script>"),
                    ],
                },
                Chapter {
                    doc_id: Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0003),
                    title: "Reference".into(),
                    depth: 1,
                    blocks: vec![
                        block("| Name | Value |\n| --- | ---: |\n| a & b | 1 |\n| <c> | 2 |"),
                        block("> a quote\n> on two lines"),
                        block("![a picture](https://example.com/p.png)"),
                    ],
                },
                Chapter {
                    doc_id: Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0004),
                    title: "Diagrams".into(),
                    depth: 2,
                    blocks: vec![diagram, pictured, block("Back to [[getting started]].")],
                },
            ],
            pictures,
        }
    }

    pub(crate) fn unzip(bytes: &[u8]) -> Vec<(String, Vec<u8>, zip::CompressionMethod)> {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        (0..z.len())
            .map(|i| {
                let mut f = z.by_index(i).unwrap();
                let mut b = Vec::new();
                f.read_to_end(&mut b).unwrap();
                (f.name().to_string(), b, f.compression())
            })
            .collect()
    }

    /// Parse every byte as XML; panics on the first error.
    pub(crate) fn well_formed(name: &str, xml: &[u8]) {
        let mut r = quick_xml::Reader::from_reader(xml);
        r.config_mut().check_end_names = true;
        let mut buf = Vec::new();
        let mut depth = 0i64;
        loop {
            match r.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(quick_xml::events::Event::Start(_)) => depth += 1,
                Ok(quick_xml::events::Event::End(_)) => depth -= 1,
                Ok(_) => {}
                Err(e) => panic!("{name} is not well-formed XML at {}: {e}\n{}", r.buffer_position(), String::from_utf8_lossy(xml)),
            }
            buf.clear();
        }
        assert_eq!(depth, 0, "{name}: unbalanced");
    }

    #[test]
    fn the_package_is_a_valid_ocf_container() {
        let bytes = render(&sample()).unwrap();
        // mimetype is the first local header, stored, no extra field
        assert_eq!(&bytes[0..4], b"PK\x03\x04");
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 0, "mimetype is stored");
        let name_len = u16::from_le_bytes([bytes[26], bytes[27]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[28], bytes[29]]) as usize;
        assert_eq!((name_len, extra_len), (8, 0));
        assert_eq!(&bytes[30..38], b"mimetype");
        assert_eq!(&bytes[38..58], b"application/epub+zip");
        let files = unzip(&bytes);
        let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(
            names[..9],
            [
                "mimetype",
                "META-INF/container.xml",
                "OEBPS/content.opf",
                "OEBPS/nav.xhtml",
                "OEBPS/toc.ncx",
                "OEBPS/style.css",
                "OEBPS/ch001.xhtml",
                "OEBPS/ch002.xhtml",
                "OEBPS/ch003.xhtml",
            ]
        );
        assert_eq!(files[0].2, zip::CompressionMethod::Stored);
        for (name, body, _) in &files {
            if name.ends_with(".xml") || name.ends_with(".opf") || name.ends_with(".xhtml") || name.ends_with(".ncx") {
                well_formed(name, body);
            }
        }
        let text = |n: &str| String::from_utf8(files.iter().find(|f| f.0 == n).unwrap().1.clone()).unwrap();
        let opf = text("OEBPS/content.opf");
        assert!(opf.contains("<dc:identifier id=\"bookid\">urn:uuid:01900000-0000-7000-8000-000000000001</dc:identifier>"), "{opf}");
        assert!(opf.contains("<dc:creator>Work</dc:creator>") && opf.contains("<dc:language>en</dc:language>"));
        assert!(opf.contains("<meta property=\"dcterms:modified\">2026-10-23T09:30:00Z</meta>"));
        assert!(opf.contains("properties=\"nav\"") && opf.contains("<spine toc=\"ncx\">"));
        // exactly the one picture that matched its fence is packaged
        assert_eq!(files.iter().filter(|f| f.0.ends_with(".png")).count(), 1);
        assert!(opf.contains("media-type=\"image/png\""));
        // the nested contents: Diagrams (depth 2) sits under Reference
        let nav = text("OEBPS/nav.xhtml");
        let reference = nav.find("ch002.xhtml").unwrap();
        let diagrams = nav.find("ch003.xhtml").unwrap();
        assert!(reference < diagrams && nav[reference..diagrams].contains("<ol>"), "{nav}");
        let ncx = text("OEBPS/toc.ncx");
        assert!(ncx.contains("dtb:depth\" content=\"2\"") && ncx.contains("playOrder=\"3\""), "{ncx}");
    }

    #[test]
    fn content_rules_links_code_tables_and_diagrams() {
        let files = unzip(&render(&sample()).unwrap());
        let text = |n: &str| String::from_utf8(files.iter().find(|f| f.0 == n).unwrap().1.clone()).unwrap();
        let c1 = text("OEBPS/ch001.xhtml");
        // the doc's own `# Title` is the heading (not doubled)
        assert_eq!(c1.matches("Getting started</h1>").count(), 1, "{c1}");
        // a wikilink inside the book is an internal link; outside, plain text
        assert!(c1.contains("<a href=\"ch002.xhtml\">Reference</a>"), "{c1}");
        assert!(c1.contains("a doc not in this book") && !c1.contains("Elsewhere"), "{c1}");
        assert!(c1.contains("<a href=\"https://example.com/a?b=1&amp;c=2\">outside</a>"), "{c1}");
        assert!(c1.contains("<pre><code class=\"language-rust\">") && c1.contains("&lt;hi&gt;"), "{c1}");
        assert!(c1.contains("\u{2611} done") && c1.contains("\u{2610} todo"), "{c1}");
        assert!(!c1.contains("<script") && c1.contains("&lt;script&gt;"), "{c1}");
        let c2 = text("OEBPS/ch002.xhtml");
        assert!(c2.contains("<table>") && c2.contains("a &amp; b") && c2.contains("&lt;c&gt;"), "{c2}");
        assert!(c2.contains("<blockquote>"), "{c2}");
        assert!(c2.contains("[a picture]") && !c2.contains("<img"), "{c2}");
        let c3 = text("OEBPS/ch003.xhtml");
        // no stored picture: the source in a code box; a matching one: the image
        assert!(c3.contains("mermaid diagram (source)") && c3.contains("A --&gt; B"), "{c3}");
        assert!(c3.contains("<img src=\"images/") && c3.contains("alt=\"d2 diagram\" width=\"1200\" height=\"600\""), "{c3}");
        // case-insensitive wikilink back to chapter 1
        assert!(c3.contains("<a href=\"ch001.xhtml\">getting started</a>"), "{c3}");
    }

    #[test]
    fn a_stale_picture_is_ignored() {
        let mut s = sample();
        let d2 = s.chapters[2].blocks[1].id;
        s.pictures = BTreeMap::from([((d2, source_hash("d2", "x -> z")), Picture { png: vec![1], width: 1, height: 1 })]);
        let files = unzip(&render(&s).unwrap());
        assert!(!files.iter().any(|f| f.0.ends_with(".png")));
        let c3 = String::from_utf8(files.iter().find(|f| f.0 == "OEBPS/ch003.xhtml").unwrap().1.clone()).unwrap();
        assert!(c3.contains("d2 diagram (source)"), "{c3}");
    }

    #[test]
    fn the_same_content_gives_identical_bytes() {
        let a = render(&sample()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let b = render(&sample()).unwrap();
        assert_eq!(a, b, "a re-render must be byte-identical");
        assert_eq!(spec_key(&sample()), spec_key(&sample()));
        let mut changed = sample();
        changed.chapters[1].blocks[1].markdown.push('!');
        assert_ne!(render(&changed).unwrap(), a);
        assert_ne!(spec_key(&changed), spec_key(&sample()));
        // the cache hands back the same bytes
        let c = CACHE.get_or_render(&sample()).unwrap();
        assert_eq!(*c, a);
    }

    #[test]
    fn source_hashes_ignore_trailing_space_and_outer_blank_lines() {
        assert_eq!(source_hash("Mermaid", "\ngraph TD  \n  A-->B\n\n"), source_hash("mermaid", "graph TD\n  A-->B"));
        assert_ne!(source_hash("mermaid", "graph TD"), source_hash("d2", "graph TD"));
        assert_eq!(diagram_fences("x\n\n```vega-lite\n{}\n```\n\n```js\n1\n```"), vec![("vega-lite".to_string(), "{}\n".to_string())]);
    }

    #[test]
    fn the_toc_nests_by_depth() {
        let mut s = sample();
        s.chapters[0].depth = 1;
        s.chapters[1].depth = 2;
        s.chapters[2].depth = 3;
        let (top, kids) = toc_tree(&s.chapters);
        assert_eq!(top, vec![0]);
        assert_eq!((kids[&0].clone(), kids[&1].clone()), (vec![1], vec![2]));
    }
}
