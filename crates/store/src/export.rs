//! Markdown export (ticket 2.9): block trees → markdown, the escape hatch.
//!
//! Blocks store the raw markdown they were imported from (or were written
//! as), so a doc's markdown is its blocks in tree order joined by blank
//! lines. Docs with child docs become directories; their own blocks land in
//! `_index.md`.

use crate::{BlockNode, BlockStore, BlockType, Doc, Result, StoreError};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

fn collect(nodes: &[BlockNode], out: &mut Vec<String>) {
    for n in nodes {
        // comments live in the block tree but are not document content
        if n.block.block_type == BlockType::Comment {
            continue;
        }
        out.push(n.block.content.clone());
        collect(&n.children, out);
    }
}

/// A doc's markdown: blocks in tree order, blank-line separated.
pub fn export_doc(store: &impl BlockStore, doc_id: Uuid) -> Result<String> {
    let tree = store.read_doc(doc_id)?;
    let mut parts = Vec::new();
    collect(&tree.roots, &mut parts);
    let mut md = parts.join("\n\n");
    if !md.is_empty() {
        md.push('\n');
    }
    Ok(md)
}

#[derive(Debug, Default)]
pub struct ExportReport {
    pub files: usize,
}

/// Export every doc to a directory tree. Doc titles become file/dir names.
pub fn export_vault(store: &impl BlockStore, out: &Path) -> Result<ExportReport> {
    let docs = store.list_docs()?;
    let mut children: HashMap<Option<Uuid>, Vec<&Doc>> = HashMap::new();
    for d in &docs {
        children.entry(d.parent_id).or_default().push(d);
    }
    let mut report = ExportReport::default();
    write_level(store, &children, None, out, &mut report)?;
    Ok(report)
}

/// A doc title as a file name: path separators become dashes.
pub fn safe_name(title: &str) -> String {
    title.replace(['/', '\\'], "-")
}

fn write_level(
    store: &impl BlockStore,
    children: &HashMap<Option<Uuid>, Vec<&Doc>>,
    parent: Option<Uuid>,
    dir: &Path,
    report: &mut ExportReport,
) -> Result<()> {
    let Some(docs) = children.get(&parent) else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)
        .map_err(|e| StoreError::InvalidOp(format!("mkdir {}: {e}", dir.display())))?;
    for doc in docs {
        let name = safe_name(&doc.title);
        let has_children = children.contains_key(&Some(doc.id));
        let md = export_doc(store, doc.id)?;
        if has_children {
            let sub = dir.join(&name);
            write_level(store, children, Some(doc.id), &sub, report)?;
            if !md.is_empty() {
                write_md(&sub.join("_index.md"), &md)?;
                report.files += 1;
            }
        } else {
            write_md(&dir.join(format!("{name}.md")), &md)?;
            report.files += 1;
        }
    }
    Ok(())
}

fn write_md(path: &Path, md: &str) -> Result<()> {
    std::fs::write(path, md)
        .map_err(|e| StoreError::InvalidOp(format!("write {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::import_markdown;
    use crate::{PrincipalKind, SqliteStore};

    const MD: &str = "---\ntags:\n  - daily\n---\n\n# Title\n\nintro para\nsecond line\n\n## Sub\n\n```rust\nfn x() {}\n```\n\ntail\n";

    #[test]
    fn import_export_round_trips_bytes_for_single_blank_line_docs() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let (doc_id, _) = import_markdown(&mut s, "d", None, tom.id, MD).unwrap();
        assert_eq!(export_doc(&s, doc_id).unwrap(), MD);
    }

    #[test]
    fn semantic_round_trip_is_stable() {
        // messy spacing normalises once, then export∘import is identity
        let messy = "# A\n\n\n\npara\n\n\n## B\ntail\n";
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let (d1, _) = import_markdown(&mut s, "d1", None, tom.id, messy).unwrap();
        let once = export_doc(&s, d1).unwrap();
        let (d2, _) = import_markdown(&mut s, "d2", None, tom.id, &once).unwrap();
        assert_eq!(export_doc(&s, d2).unwrap(), once);
    }
}
