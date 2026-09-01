//! Markdown vault import (ticket 2.8): Octarine vault → block trees.
//!
//! Line-based, lossless: every block stores the raw markdown slice it came
//! from, so export (2.9) is concatenation. Headings give structure (a
//! heading's following blocks nest under it, by level); fences become
//! code/diagram blocks; frontmatter rides along as the first block.

use crate::{BlockStore, BlockType, OpInput, OpKind, Result, order_key};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Default)]
pub struct ImportReport {
    pub docs: usize,
    pub blocks: usize,
    pub skipped: Vec<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub struct Segment {
    pub block_type: BlockType,
    /// Heading level for headings, 0 otherwise.
    pub level: u8,
    pub content: String,
}

/// True when `content` is a frontmatter block: a `---` first line closed by
/// a later `---` line. A lone `---` (horizontal rule) is not frontmatter.
pub fn is_frontmatter(content: &str) -> bool {
    let mut lines = content.lines();
    lines.next() == Some("---") && lines.any(|l| l == "---")
}

/// Heading level (1..=6) if `content` is an ATX heading (`#`{1,6} + space).
pub fn heading_level(content: &str) -> Option<u8> {
    let first = content.lines().next().unwrap_or("");
    let level = first.chars().take_while(|c| *c == '#').count();
    ((1..=6).contains(&level) && first[level..].starts_with(' ')).then_some(level as u8)
}

/// The single source of truth for typing a content block from its markdown.
/// `segment` and the store's Replace projection both use it, so a block's
/// type can never drift from what its content says it is.
///
/// - frontmatter (`---` first line, later closing `---`) → Code
/// - fenced ```` ```mermaid ```` → DiagramMermaid, ```` ```d2 ```` → DiagramD2,
///   any other ``` / ~~~ fence → Code
/// - `#`{1..6} + space → Heading
/// - starts with `DECISION:` → Decision
/// - else Paragraph
pub fn infer_block_type(content: &str) -> BlockType {
    if is_frontmatter(content) {
        return BlockType::Code;
    }
    let first = content.lines().next().unwrap_or("").trim_start();
    if let Some(fence) = ["```", "~~~"].iter().find(|f| first.starts_with(**f)) {
        let lang = first.trim_start_matches(*fence).trim().to_lowercase();
        return match lang.as_str() {
            "mermaid" => BlockType::DiagramMermaid,
            "d2" => BlockType::DiagramD2,
            _ => BlockType::Code,
        };
    }
    if heading_level(content).is_some() {
        return BlockType::Heading;
    }
    if content.starts_with("DECISION:") {
        return BlockType::Decision;
    }
    BlockType::Paragraph
}

/// Split raw markdown into flat segments (headings carry their level).
pub fn segment(md: &str) -> Vec<Segment> {
    let lines: Vec<&str> = md.lines().collect();
    let mut segs = Vec::new();
    let mut i = 0;

    // frontmatter
    if lines.first() == Some(&"---")
        && let Some(end) = lines.iter().skip(1).position(|l| *l == "---")
    {
        let content = lines[..=end + 1].join("\n");
        segs.push(Segment {
            block_type: infer_block_type(&content),
            level: 0,
            content,
        });
        i = end + 2;
    }

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // fenced block
        if let Some(fence) = ["```", "~~~"].iter().find(|f| trimmed.starts_with(**f)) {
            let start = i;
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(*fence) {
                i += 1;
            }
            let end = i.min(lines.len() - 1);
            i = (i + 1).min(lines.len());
            let content = lines[start..=end].join("\n");
            segs.push(Segment {
                block_type: infer_block_type(&content),
                level: 0,
                content,
            });
            continue;
        }

        // heading
        if let Some(level) = heading_level(line) {
            segs.push(Segment {
                block_type: BlockType::Heading,
                level,
                content: line.to_string(),
            });
            i += 1;
            continue;
        }

        // paragraph: run until blank line, fence, or heading
        let start = i;
        while i < lines.len() {
            let l = lines[i];
            let t = l.trim_start();
            if t.is_empty() || t.starts_with("```") || t.starts_with("~~~") {
                break;
            }
            if l.starts_with('#') && i > start {
                break;
            }
            i += 1;
        }
        let content = lines[start..i].join("\n");
        segs.push(Segment {
            // decision blocks (5.6): a paragraph declaring itself a decision
            block_type: infer_block_type(&content),
            level: 0,
            content,
        });
    }
    segs
}

/// Segments → insert ops with heading-level nesting and fractional order keys.
/// Public so the daemon's scribe can route new-doc content through the gate
/// (`propose`) instead of `apply`.
pub fn to_ops(segs: Vec<Segment>) -> Vec<OpInput> {
    // stack of (heading level, block id) — parents for what follows
    let mut stack: Vec<(u8, Uuid)> = Vec::new();
    let mut last_key: HashMap<Option<Uuid>, String> = HashMap::new();
    let mut ops = Vec::with_capacity(segs.len());

    for seg in segs {
        if seg.block_type == BlockType::Heading {
            while stack.last().is_some_and(|(l, _)| *l >= seg.level) {
                stack.pop();
            }
        }
        let parent = stack.last().map(|(_, id)| *id);
        let key = order_key::between(last_key.get(&parent).map(String::as_str), None);
        last_key.insert(parent, key.clone());

        let block_id = Uuid::now_v7();
        if seg.block_type == BlockType::Heading {
            stack.push((seg.level, block_id));
        }
        ops.push(OpInput {
            kind: OpKind::Insert {
                block_id,
                parent_id: parent,
                order_key: key,
                block_type: seg.block_type,
                content: seg.content,
                refers_to: None,
            },
            source_refs: vec![],
        });
    }
    ops
}

/// Import one markdown string as a new doc. One apply → one epoch.
pub fn import_markdown(
    store: &mut impl BlockStore,
    title: &str,
    parent_doc: Option<Uuid>,
    principal: Uuid,
    md: &str,
) -> Result<(Uuid, usize)> {
    let doc = store.create_doc(title, parent_doc, principal)?;
    let ops = to_ops(segment(md));
    let n = ops.len();
    if n > 0 {
        store.apply(doc.id, 0, principal, ops)?;
    }
    Ok((doc.id, n))
}

/// Walk a vault directory: folders become parent docs, every .md a doc.
/// Hidden entries and `.octarine` are skipped.
pub fn import_vault(
    store: &mut impl BlockStore,
    root: &Path,
    principal: Uuid,
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    walk(store, root, None, principal, &mut report)?;
    Ok(report)
}

fn walk(
    store: &mut impl BlockStore,
    dir: &Path,
    parent_doc: Option<Uuid>,
    principal: Uuid,
    report: &mut ImportReport,
) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| crate::StoreError::InvalidOp(format!("read_dir {}: {e}", dir.display())))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.starts_with('.'))
        })
        .collect();
    entries.sort();

    for path in entries {
        if path.is_dir() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let folder = store.create_doc(&name, parent_doc, principal)?;
            report.docs += 1;
            walk(store, &path, Some(folder.id), principal, report)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let title = path.file_stem().unwrap().to_string_lossy().to_string();
            match std::fs::read_to_string(&path) {
                Ok(md) => {
                    let (_, blocks) = import_markdown(store, &title, parent_doc, principal, &md)?;
                    report.docs += 1;
                    report.blocks += blocks;
                }
                Err(_) => report.skipped.push(path),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_frontmatter_headings_fences_paragraphs() {
        let md = "---\ntags:\n  - daily\n---\n\n# Title\n\nintro para\nsecond line\n\n## Sub\n\n```rust\nfn x() {}\n```\n\n```mermaid\ngraph TD\n```\n\ntail";
        let segs = segment(md);
        let types: Vec<_> = segs.iter().map(|s| s.block_type).collect();
        assert_eq!(
            types,
            vec![
                BlockType::Code, // frontmatter
                BlockType::Heading,
                BlockType::Paragraph,
                BlockType::Heading,
                BlockType::Code,
                BlockType::DiagramMermaid,
                BlockType::Paragraph,
            ]
        );
        assert_eq!(segs[2].content, "intro para\nsecond line");
        assert!(segs[4].content.starts_with("```rust"));
        assert!(segs[4].content.ends_with("```"));
    }

    #[test]
    fn heading_levels_nest() {
        let md = "# A\n\npara a\n\n## B\n\npara b\n\n# C\n\npara c";
        let ops = to_ops(segment(md));
        // A(root) > para a, B; B > para b; C(root) > para c
        let ids: Vec<Uuid> = ops
            .iter()
            .map(|o| match &o.kind {
                OpKind::Insert { block_id, .. } => *block_id,
                _ => unreachable!(),
            })
            .collect();
        let parents: Vec<Option<Uuid>> = ops
            .iter()
            .map(|o| match &o.kind {
                OpKind::Insert { parent_id, .. } => *parent_id,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(parents[0], None); // A
        assert_eq!(parents[1], Some(ids[0])); // para a under A
        assert_eq!(parents[2], Some(ids[0])); // B under A
        assert_eq!(parents[3], Some(ids[2])); // para b under B
        assert_eq!(parents[4], None); // C at root
        assert_eq!(parents[5], Some(ids[4])); // para c under C
    }

    #[test]
    fn infer_block_type_matrix() {
        let cases = [
            ("---\ntags:\n  - x\n---", BlockType::Code),
            ("---", BlockType::Paragraph), // horizontal rule, not frontmatter
            ("```mermaid\ngraph TD\n```", BlockType::DiagramMermaid),
            ("```MERMAID\ngraph TD\n```", BlockType::DiagramMermaid),
            ("```d2\na -> b\n```", BlockType::DiagramD2),
            ("```rust\nfn x() {}\n```", BlockType::Code),
            ("```\nplain\n```", BlockType::Code),
            ("~~~\ntilde\n~~~", BlockType::Code),
            ("# Title", BlockType::Heading),
            ("###### Six", BlockType::Heading),
            ("####### Seven", BlockType::Paragraph),
            ("#hashtag", BlockType::Paragraph),
            ("DECISION: ship it", BlockType::Decision),
            ("plain para\nsecond line", BlockType::Paragraph),
            ("", BlockType::Paragraph),
        ];
        for (content, want) in cases {
            assert_eq!(infer_block_type(content), want, "content {content:?}");
        }
    }

    #[test]
    fn segment_types_agree_with_infer_block_type() {
        let md = "---\ntags:\n  - daily\n---\n\n# Title\n\nDECISION: yes\n\n```d2\na\n```\n\n~~~\nx\n~~~\n\ntail";
        for seg in segment(md) {
            assert_eq!(
                seg.block_type,
                infer_block_type(&seg.content),
                "{:?}",
                seg.content
            );
        }
    }

    #[test]
    fn unterminated_fence_swallows_to_eof_without_panic() {
        let md = "```rust\nfn x() {}";
        let segs = segment(md);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].block_type, BlockType::Code);
    }
}
