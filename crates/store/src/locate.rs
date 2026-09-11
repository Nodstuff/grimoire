//! Locating things inside a doc for edit-shaped writes (AX 2): heading
//! paths (`grimoire › Plans`), short block refs (`^abc123`), section spans,
//! and the splice helpers `edit_doc` / `append` use to turn an edit into a
//! whole-doc markdown string for `mddiff::markdown_to_ops`.
//!
//! Everything here works on the doc's *content export* — blocks in tree
//! order joined by blank lines, comments and canvases excluded — which is
//! what `read_doc` shows and what `markdown_to_ops` diffs against, so an
//! edit expressed as a string splice round-trips to the minimal ops.

use crate::import::heading_level;
use crate::{Block, BlockNode, BlockType};
use std::ops::Range;
use uuid::Uuid;

/// Segment separators in a heading path: `grimoire › Plans` or `grimoire / Plans`.
const PATH_SEPARATORS: [char; 2] = ['›', '/'];

/// `^` + the last 6 hex chars of a block id, lowercase — the short ref an
/// agent quotes in `edit_doc` errors, `read_doc(refs: true)` and comments.
pub fn short_ref(id: Uuid) -> String {
    let s = id.simple().to_string();
    format!("^{}", &s[s.len() - 6..])
}

/// Is this (trimmed) line exactly a short ref? `read_doc(refs: true)` puts one
/// above each block; `strip_block_markers` drops them so the read round-trips.
pub fn is_short_ref_line(line: &str) -> bool {
    let t = line.trim();
    t.len() == 7
        && t.starts_with('^')
        && t[1..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// A block reference as an agent writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockRef {
    /// A full UUID.
    Id(Uuid),
    /// `^abc123`: the last 6 hex chars of the id, lowercase.
    Suffix(String),
}

/// Parse `^abc123` or a full UUID. None when the string is neither — callers
/// then treat it as a heading path.
pub fn parse_block_ref(s: &str) -> Option<BlockRef> {
    let t = s.trim();
    if let Ok(id) = Uuid::parse_str(t) {
        return Some(BlockRef::Id(id));
    }
    let hex = t.strip_prefix('^')?;
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| BlockRef::Suffix(hex.to_ascii_lowercase()))
}

/// Does `id` end with this 6-char suffix?
pub fn id_has_suffix(id: Uuid, suffix: &str) -> bool {
    id.simple().to_string().ends_with(suffix)
}

/// Content blocks (comments and canvases excluded) in tree order.
pub fn content_blocks(roots: &[BlockNode]) -> Vec<&Block> {
    fn walk<'a>(nodes: &'a [BlockNode], out: &mut Vec<&'a Block>) {
        for n in nodes {
            if !is_non_content(&n.block) {
                out.push(&n.block);
                walk(&n.children, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(roots, &mut out);
    out
}

/// Comments and canvases are not content flow (the same rule as
/// `mddiff::markdown_to_ops`, so an export built from `content_blocks` diffs
/// to zero ops against itself).
pub fn is_non_content(b: &Block) -> bool {
    matches!(b.block_type, BlockType::Comment | BlockType::CanvasScene)
}

/// The content export with each block's byte range in it: blocks in tree
/// order joined by `\n\n`, a trailing `\n` when non-empty — byte-identical to
/// `export::markdown_of(roots, false)` for a doc without canvases.
pub fn export_with_offsets(roots: &[BlockNode]) -> (String, Vec<(Uuid, Range<usize>)>) {
    let mut md = String::new();
    let mut spans = Vec::new();
    for (i, b) in content_blocks(roots).iter().enumerate() {
        if i > 0 {
            md.push_str("\n\n");
        }
        let start = md.len();
        md.push_str(&b.content);
        spans.push((b.id, start..md.len()));
    }
    if !md.is_empty() {
        md.push('\n');
    }
    (md, spans)
}

/// The content export, blank-line separated (comments and canvases excluded).
pub fn content_markdown(roots: &[BlockNode]) -> String {
    export_with_offsets(roots).0
}

/// The same export with a `^abc123` line above every block — `read_doc(refs: true)`.
pub fn content_markdown_with_refs(roots: &[BlockNode]) -> String {
    let parts: Vec<String> = content_blocks(roots)
        .iter()
        .map(|b| format!("{}\n{}", short_ref(b.id), b.content))
        .collect();
    let mut md = parts.join("\n\n");
    if !md.is_empty() {
        md.push('\n');
    }
    md
}

/// A heading's text: `## Plans ` → `plans` (leading `#`s optional, trimmed,
/// lowercase) — the form path segments are compared in.
pub fn heading_key(s: &str) -> String {
    s.trim().trim_start_matches('#').trim().to_lowercase()
}

/// Split a heading path on `›` or `/`, dropping empty segments. Each segment
/// keeps its raw form so `create_missing` can read an explicit level
/// (`## x / ### y`); compare with `heading_key`.
pub fn split_path(path: &str) -> Vec<String> {
    path.split(PATH_SEPARATORS)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// A heading block and its ancestor chain, for path matching and messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingEntry {
    pub id: Uuid,
    pub level: u8,
    /// Ancestor heading texts root-first, then this heading's text (display form).
    pub path: Vec<String>,
}

impl HeadingEntry {
    /// `qompass › Plans`.
    pub fn display(&self) -> String {
        self.path.join(" › ")
    }
}

/// Every heading block in the doc with its ancestor path (tree order).
pub fn headings(roots: &[BlockNode]) -> Vec<HeadingEntry> {
    fn walk(nodes: &[BlockNode], chain: &mut Vec<String>, out: &mut Vec<HeadingEntry>) {
        for n in nodes {
            if is_non_content(&n.block) {
                continue;
            }
            let Some(level) = heading_level(&n.block.content) else {
                walk(&n.children, chain, out);
                continue;
            };
            let text = n.block.content.trim().trim_start_matches('#').trim().to_string();
            chain.push(text);
            out.push(HeadingEntry {
                id: n.block.id,
                level,
                path: chain.clone(),
            });
            walk(&n.children, chain, out);
            chain.pop();
        }
    }
    let mut out = Vec::new();
    walk(roots, &mut Vec::new(), &mut out);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateError {
    /// No heading matched; carries the headings that exist (display paths).
    NotFound { path: String, available: Vec<String> },
    /// Several headings match; carries their full display paths.
    Ambiguous { path: String, candidates: Vec<String> },
}

impl std::fmt::Display for LocateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocateError::NotFound { path, available } => {
                write!(f, "no heading matches {path:?}")?;
                if !available.is_empty() {
                    let shown: Vec<&str> = available.iter().take(12).map(String::as_str).collect();
                    write!(f, "; headings: {}", shown.join(" | "))?;
                    if available.len() > 12 {
                        write!(f, " | … +{} more", available.len() - 12)?;
                    }
                }
                Ok(())
            }
            LocateError::Ambiguous { path, candidates } => write!(
                f,
                "{path:?} is ambiguous — widen the path to one of: {}",
                candidates.join(" | ")
            ),
        }
    }
}

/// Does `entry`'s ancestor chain contain `segments` (heading keys) in order,
/// with the last segment being the heading itself?
fn path_matches(entry: &HeadingEntry, segments: &[String]) -> bool {
    let Some((last, prefix)) = segments.split_last() else {
        return false;
    };
    let keys: Vec<String> = entry.path.iter().map(|s| heading_key(s)).collect();
    let Some((own, ancestors)) = keys.split_last() else {
        return false;
    };
    if own != last {
        return false;
    }
    // the prefix must appear in order among the ancestors (not necessarily contiguous)
    let mut i = 0;
    for a in ancestors {
        if i < prefix.len() && *a == prefix[i] {
            i += 1;
        }
    }
    i == prefix.len()
}

/// Resolve a heading path to its heading block. Segments are separated by
/// `›` or `/` and compared case-insensitively with leading `#`s optional;
/// a longer path narrows. Exactly one match wins; several → `Ambiguous`
/// listing their full paths so the caller can widen; none → `NotFound`.
pub fn resolve_heading_path(roots: &[BlockNode], path: &str) -> Result<HeadingEntry, LocateError> {
    let all = headings(roots);
    let segments: Vec<String> = split_path(path).iter().map(|s| heading_key(s)).collect();
    if segments.is_empty() {
        return Err(LocateError::NotFound {
            path: path.to_string(),
            available: all.iter().map(HeadingEntry::display).collect(),
        });
    }
    let mut hits: Vec<&HeadingEntry> = all.iter().filter(|h| path_matches(h, &segments)).collect();
    match hits.len() {
        0 => Err(LocateError::NotFound {
            path: path.to_string(),
            available: all.iter().map(HeadingEntry::display).collect(),
        }),
        1 => Ok(hits.remove(0).clone()),
        _ => Err(LocateError::Ambiguous {
            path: path.to_string(),
            candidates: hits.iter().map(|h| h.display()).collect(),
        }),
    }
}

/// The node for `id` anywhere in the tree.
pub fn find_node(roots: &[BlockNode], id: Uuid) -> Option<&BlockNode> {
    for n in roots {
        if n.block.id == id {
            return Some(n);
        }
        if let Some(found) = find_node(&n.children, id) {
            return Some(found);
        }
    }
    None
}

/// The span of a section: the block itself plus every content descendant, in
/// tree order (a heading's section is everything nested under it; a
/// paragraph's span is just itself).
pub fn section_span(roots: &[BlockNode], id: Uuid) -> Vec<&Block> {
    match find_node(roots, id) {
        Some(n) => {
            let mut out = vec![&n.block];
            out.extend(content_blocks(&n.children));
            out
        }
        None => Vec::new(),
    }
}

/// Byte offsets in the content export where text goes when appended to the
/// section rooted at `id`: `start` = right after the heading block, `end` =
/// after the section's last descendant. None when `id` is not in the tree.
pub fn section_insert_offsets(roots: &[BlockNode], id: Uuid) -> Option<(usize, usize)> {
    let (_, spans) = export_with_offsets(roots);
    let span = section_span(roots, id);
    let first = span.first()?.id;
    let last = span.last()?.id;
    let end_of = |b: Uuid| spans.iter().find(|(i, _)| *i == b).map(|(_, r)| r.end);
    Some((end_of(first)?, end_of(last)?))
}

/// Insert `text` into the export at byte `at` (the end of some block), with
/// blank-line separation on both sides. `at == export.len()` with an empty
/// export creates the doc's first content.
pub fn splice_insert(export: &str, at: usize, text: &str) -> String {
    let text = text.trim_matches('\n').trim_end();
    if text.is_empty() {
        return export.to_string();
    }
    if export.trim().is_empty() {
        return format!("{text}\n");
    }
    let at = at.min(export.len());
    let (head, tail) = export.split_at(at);
    let head = head.trim_end_matches('\n');
    let tail = tail.trim_start_matches('\n');
    let mut out = String::with_capacity(export.len() + text.len() + 4);
    out.push_str(head);
    if !head.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(text);
    if !tail.is_empty() {
        out.push_str("\n\n");
        out.push_str(tail);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Replace the byte range with `new` — the edit twin of `splice_insert`.
pub fn splice_replace(export: &str, range: Range<usize>, new: &str) -> String {
    let mut out = String::with_capacity(export.len() + new.len());
    out.push_str(&export[..range.start]);
    out.push_str(new);
    out.push_str(&export[range.end..]);
    out
}

/// Every byte offset where `needle` occurs in `hay` (non-overlapping).
pub fn find_all(hay: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let mut from = 0;
    while let Some(pos) = hay[from..].find(needle) {
        out.push(from + pos);
        from += pos + needle.len();
    }
    out
}

/// Whitespace-normalised view of `s` (runs of whitespace → one space, ends
/// trimmed) with, for every byte of the normalised string, the byte offset it
/// came from in `s` — so a match in the normalised text maps back to an
/// original range.
pub fn normalise_ws(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut map = Vec::with_capacity(s.len());
    let mut pending_space: Option<usize> = None;
    for (i, ch) in s.char_indices() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space.get_or_insert(i);
            }
            continue;
        }
        if let Some(sp) = pending_space.take() {
            out.push(' ');
            map.push(sp);
        }
        let mut buf = [0u8; 4];
        let n = ch.encode_utf8(&mut buf).len();
        out.push(ch);
        for k in 0..n {
            map.push(i + k);
        }
    }
    map.push(s.len());
    (out, map)
}

/// Find `old` in `export` ignoring whitespace differences. Returns the
/// original byte ranges of every match.
pub fn find_all_normalised(export: &str, old: &str) -> Vec<Range<usize>> {
    let (nh, map) = normalise_ws(export);
    let (nn, _) = normalise_ws(old);
    if nn.is_empty() {
        return Vec::new();
    }
    find_all(&nh, &nn)
        .into_iter()
        .map(|start| {
            let end = start + nn.len();
            // the end maps to the byte after the last matched char
            let orig_end = if end < map.len() { map[end] } else { export.len() };
            let orig_end = export[..orig_end].trim_end().len().max(map[start]);
            map[start]..orig_end
        })
        .collect()
}

/// Which block's range contains byte `at`? (The first block whose range
/// starts at or before `at` and ends after it.)
pub fn block_at(spans: &[(Uuid, Range<usize>)], at: usize) -> Option<Uuid> {
    spans
        .iter()
        .find(|(_, r)| r.start <= at && at < r.end.max(r.start + 1))
        .map(|(id, _)| *id)
        .or_else(|| spans.iter().rev().find(|(_, r)| r.start <= at).map(|(id, _)| *id))
}

fn trigrams(s: &str) -> std::collections::HashSet<String> {
    let padded: Vec<char> = format!("  {} ", s.to_lowercase()).chars().collect();
    padded.windows(3).map(|w| w.iter().collect()).collect()
}

/// The content block most similar to `text` (trigram overlap), for the
/// "old not found, did you mean" hint. None on an empty doc.
pub fn closest_block<'a>(blocks: &[&'a Block], text: &str) -> Option<&'a Block> {
    let q = trigrams(text);
    if q.is_empty() {
        return None;
    }
    blocks
        .iter()
        .map(|b| {
            let c = trigrams(&b.content);
            let inter = q.intersection(&c).count() as f64;
            let score = inter / (q.len() as f64).max(1.0) + inter / (c.len() as f64).max(1.0) * 0.25;
            (score, *b)
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, b)| b)
}

/// First line of a block, capped at `max` chars, with an ellipsis if cut.
pub fn first_line(content: &str, max: usize) -> String {
    let first = content.lines().next().unwrap_or("");
    let mut s: String = first.chars().take(max).collect();
    if s.len() < content.len() {
        s.push('…');
    }
    s
}

/// The heading level new root-level sections get when `append` creates a
/// path that does not exist: the doc's existing root-level section level
/// (`##` when its sections are `##`). `#` is a doc's title in Grimoire, never
/// a section, so a doc with only an H1 — or no headings at all — gets `##`
/// (the first live `append(create_missing)` on an empty daily doc produced
/// `# grimoire` / `## Done`, 2026-09-11).
pub fn root_heading_level(roots: &[BlockNode]) -> u8 {
    roots
        .iter()
        .filter_map(|n| heading_level(&n.block.content))
        .min()
        .map(|l| l.max(2))
        .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockStore, PrincipalKind, SqliteStore, import::import_markdown};

    const DAILY: &str = "---\ntags:\n  - daily\n---\n\n## qompass\n\n### Done\n\n- shipped x\n\n### Plans\n\n- plan q\n\n## portus\n\n### Plans\n\n- plan p\n\n## grimoire\n\nintro line\n";

    fn tree(md: &str) -> (SqliteStore, Uuid, Vec<BlockNode>) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let (doc, _) = import_markdown(&mut s, "d", None, tom.id, md).unwrap();
        let roots = s.read_doc(doc).unwrap().roots;
        (s, doc, roots)
    }

    #[test]
    fn short_refs_are_the_last_six_hex_chars_lowercase() {
        let id = Uuid::parse_str("01a080ba-17b1-7a43-93d5-4d99f46f2ABC").unwrap();
        assert_eq!(short_ref(id), "^6f2abc");
        assert!(id_has_suffix(id, "6f2abc"));
        assert!(is_short_ref_line(" ^6f2abc "));
        assert!(!is_short_ref_line("^6F2ABC"), "uppercase is prose, not a ref");
        assert!(!is_short_ref_line("^abc"));
        assert!(!is_short_ref_line("^abc123 and more"));
        assert_eq!(parse_block_ref("^ABC123"), Some(BlockRef::Suffix("abc123".into())));
        assert_eq!(parse_block_ref(&id.to_string()), Some(BlockRef::Id(id)));
        assert_eq!(parse_block_ref("Plans"), None);
        assert_eq!(parse_block_ref("^zzzzzz"), None);
    }

    #[test]
    fn heading_paths_narrow_and_ambiguity_lists_candidates() {
        let (_, _, roots) = tree(DAILY);
        let q = resolve_heading_path(&roots, "qompass › Plans").unwrap();
        assert_eq!(q.display(), "qompass › Plans");
        assert_eq!(q.level, 3);
        assert_eq!(resolve_heading_path(&roots, "## Qompass / ### plans").unwrap().id, q.id);
        assert_eq!(resolve_heading_path(&roots, "grimoire").unwrap().display(), "grimoire");
        // unique last segment resolves alone
        assert_eq!(resolve_heading_path(&roots, "Done").unwrap().display(), "qompass › Done");
        match resolve_heading_path(&roots, "Plans").unwrap_err() {
            LocateError::Ambiguous { candidates, .. } => {
                assert_eq!(candidates, ["qompass › Plans", "portus › Plans"]);
            }
            e => panic!("{e:?}"),
        }
        let e = resolve_heading_path(&roots, "nope").unwrap_err();
        assert!(matches!(e, LocateError::NotFound { .. }));
        assert!(e.to_string().contains("qompass › Done"), "{e}");
        assert!(resolve_heading_path(&roots, "portus › Done").is_err(), "wrong parent");
    }

    #[test]
    fn section_span_and_offsets_cover_heading_plus_descendants() {
        let (_, _, roots) = tree(DAILY);
        let q = resolve_heading_path(&roots, "qompass").unwrap();
        let span = section_span(&roots, q.id);
        let texts: Vec<&str> = span.iter().map(|b| b.content.as_str()).collect();
        assert_eq!(texts, ["## qompass", "### Done", "- shipped x", "### Plans", "- plan q"]);
        let (md, _) = export_with_offsets(&roots);
        assert_eq!(md, DAILY);
        let (start, end) = section_insert_offsets(&roots, q.id).unwrap();
        assert_eq!(&md[..start].rsplit("\n\n").next().unwrap(), &"## qompass");
        assert!(md[..end].ends_with("- plan q"));
        // appending at the end of qompass lands before "## portus"
        let out = splice_insert(&md, end, "- new plan");
        assert!(out.contains("- plan q\n\n- new plan\n\n## portus"), "{out}");
        let out = splice_insert(&md, start, "- first");
        assert!(out.contains("## qompass\n\n- first\n\n### Done"), "{out}");
        // end of doc
        let out = splice_insert(&md, md.len(), "## new\n\ntail\n");
        assert!(out.ends_with("intro line\n\n## new\n\ntail\n"), "{out}");
        assert_eq!(splice_insert("", 0, "hello"), "hello\n");
        assert_eq!(splice_insert(&md, end, "   \n"), md, "nothing to insert");
    }

    #[test]
    fn whitespace_normalised_search_maps_back_to_original_spans() {
        let hay = "alpha   beta\n\ngamma\tdelta  \n";
        let hits = find_all_normalised(hay, "beta gamma");
        assert_eq!(hits.len(), 1);
        assert_eq!(&hay[hits[0].clone()], "beta\n\ngamma");
        let hits = find_all_normalised(hay, "delta");
        assert_eq!(&hay[hits[0].clone()], "delta");
        assert!(find_all_normalised(hay, "epsilon").is_empty());
        assert_eq!(find_all("a-a-a", "a"), [0, 2, 4]);
        assert_eq!(find_all("aaaa", "aa"), [0, 2], "non-overlapping");
        let (n, _) = normalise_ws("  x  y\n z ");
        assert_eq!(n, "x y z");
    }

    #[test]
    fn closest_block_and_root_level() {
        let (_, _, roots) = tree(DAILY);
        let blocks = content_blocks(&roots);
        let best = closest_block(&blocks, "shipped y").unwrap();
        assert_eq!(best.content, "- shipped x");
        assert_eq!(root_heading_level(&roots), 2);
        let (_, _, r2) = tree("# Title\n\npara\n");
        assert_eq!(root_heading_level(&r2), 2, "an H1 is the title, sections start at ##");
        let (_, _, r3) = tree("just a paragraph\n");
        assert_eq!(root_heading_level(&r3), 2, "no headings: sections start at ##");
        assert_eq!(first_line("line one\nline two", 100), "line one…");
        assert_eq!(first_line("short", 100), "short");
    }

    #[test]
    fn refs_export_strips_back_to_the_plain_export() {
        let (_, _, roots) = tree(DAILY);
        let with_refs = content_markdown_with_refs(&roots);
        assert!(with_refs.starts_with('^'));
        assert_eq!(crate::mddiff::strip_block_markers(&with_refs), DAILY);
        assert!(crate::mddiff::markdown_to_ops(&roots, &with_refs).is_empty());
    }

    #[test]
    fn block_at_maps_offsets_to_blocks() {
        let (_, _, roots) = tree("one\n\ntwo\n\nthree\n");
        let (md, spans) = export_with_offsets(&roots);
        let pos = md.find("two").unwrap();
        assert_eq!(block_at(&spans, pos), Some(spans[1].0));
        assert_eq!(block_at(&spans, pos + 3), Some(spans[1].0), "end of block still that block");
        assert_eq!(block_at(&spans, 0), Some(spans[0].0));
    }
}
