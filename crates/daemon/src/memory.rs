//! Claude memory → Grimoire. Claude Code keeps a curated, per-project memory
//! under `~/.claude/projects/<slug>/memory/*.md` (frontmatter + prose). That
//! corpus is exactly what ask-the-vault should know, so the daemon mirrors
//! it into a `Claude Memory` folder: one doc per memory file, filed under
//! its project, tagged from the memory's `type`, with the origin session in
//! `source_refs`.
//!
//! The sync is idempotent and gate-shaped. A file seen for the first time is
//! imported; a file that changed later is DIFFED against its doc and the
//! delta proposed under the agent principal (`markdown_to_ops`, the same
//! path an agent's `propose_markdown` takes) — so a memory the model rewrote
//! shows up as a reviewable change, never a silent overwrite. Unchanged
//! files cost one hash compare. Files that vanish leave their doc in place
//! (memory is history too; the user can trash it).
//!
//! Runs at start and every 10 minutes; `POST /api/memory/sync` for now.

use grimoire_store::{BlockStore, SqliteStore};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub const ROOT_TITLE: &str = "Claude Memory";
const SLUG_PREFIX: &str = "-Users-";
const INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SyncReport {
    pub files: usize,
    pub imported: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub projects: usize,
}

/// Where Claude Code keeps project memories on this machine.
pub fn memory_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".claude/projects")
}

/// `-Users-tmeaney-qumulo-qompass` → `qumulo-qompass`; scratchpad/worktree
/// slugs under /private/tmp are noise and yield None.
pub fn project_name(slug: &str) -> Option<String> {
    if slug.starts_with("-private-tmp") || slug.starts_with("-tmp") {
        return None;
    }
    let s = slug.strip_prefix(SLUG_PREFIX).unwrap_or(slug);
    // drop the account segment: `tmeaney-qumulo-qompass` → `qumulo-qompass`;
    // a bare account (`-Users-tmeaney`, the home dir itself) is not a project
    let (_, s) = s.split_once('-')?;
    let s = s.trim_matches('-');
    (!s.is_empty()).then(|| s.to_string())
}

#[derive(Debug, Default)]
struct Front {
    name: Option<String>,
    description: Option<String>,
    kind: Option<String>,
    origin: Option<String>,
    body: String,
}

/// Tolerant frontmatter reader for the two shapes Claude writes: flat keys
/// and a nested `metadata:` block. Anything else is left as body text.
fn parse(md: &str) -> Front {
    let mut f = Front::default();
    let Some(rest) = md.strip_prefix("---\n") else {
        f.body = md.to_string();
        return f;
    };
    let Some(end) = rest.find("\n---") else {
        f.body = md.to_string();
        return f;
    };
    let (front, body) = rest.split_at(end);
    f.body = body.trim_start_matches("\n---").trim_start_matches('\n').to_string();
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let k = k.trim();
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        if v.is_empty() {
            continue;
        }
        match k {
            "name" => f.name = Some(v),
            "description" => f.description = Some(v),
            "type" => {
                f.kind.get_or_insert(v);
            }
            "originSessionId" => f.origin = Some(v),
            _ => {}
        }
    }
    f
}

fn title_from(name: Option<&str>, path: &Path) -> String {
    let raw = name
        .map(str::to_string)
        .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().to_string());
    // memory names are kebab slugs; read as words
    let words = raw.replace(['-', '_'], " ");
    let mut t = String::with_capacity(words.len());
    let mut cap = true;
    for c in words.chars() {
        if cap && c.is_alphabetic() {
            t.extend(c.to_uppercase());
            cap = false;
        } else {
            t.push(c);
            cap = c == ' ';
        }
    }
    t
}

/// The doc's markdown for a memory file: frontmatter tags, title, the
/// description as a lead line, the body verbatim.
pub fn to_doc_markdown(project: &str, path: &Path, md: &str) -> (String, String, Vec<String>) {
    let f = parse(md);
    let title = title_from(f.name.as_deref(), path);
    let kind = f.kind.clone().unwrap_or_else(|| "note".into());
    let mut out = format!("---\ntags:\n  - claude-memory\n  - memory-{kind}\n---\n\n# {title}\n\n");
    if let Some(d) = &f.description {
        out.push_str(&format!("*{d}*\n\n"));
    }
    out.push_str(f.body.trim());
    out.push('\n');
    let mut refs = vec![format!("claude-memory: {project}/{}", path.file_name().unwrap_or_default().to_string_lossy())];
    if let Some(o) = f.origin {
        refs.push(format!("session: {o}"));
    }
    (title, out, refs)
}

fn hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn find_child(store: &SqliteStore, parent: Option<Uuid>, title: &str) -> Option<Uuid> {
    store
        .list_docs()
        .ok()?
        .into_iter()
        .find(|d| d.parent_id == parent && d.title == title)
        .map(|d| d.id)
}

fn ensure_folder(store: &mut SqliteStore, parent: Option<Uuid>, title: &str, principal: Uuid) -> grimoire_store::Result<Uuid> {
    if let Some(id) = find_child(store, parent, title) {
        return Ok(id);
    }
    Ok(store.create_doc(title, parent, principal)?.id)
}

/// Every memory file, grouped by project name.
pub fn scan(root: &Path) -> HashMap<String, Vec<PathBuf>> {
    let mut out: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let Ok(projects) = std::fs::read_dir(root) else { return out };
    for p in projects.flatten() {
        let slug = p.file_name().to_string_lossy().to_string();
        let Some(project) = project_name(&slug) else { continue };
        let mem = p.path().join("memory");
        let Ok(files) = std::fs::read_dir(&mem) else { continue };
        for f in files.flatten() {
            let path = f.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name == "MEMORY.md" || !name.ends_with(".md") || name.starts_with('.') {
                continue;
            }
            out.entry(project.clone()).or_default().push(path);
        }
    }
    out
}

/// One sync pass. `human` owns the folders; the agent principal authors the
/// docs and the later diffs (so they are reviewable like any agent write).
pub fn sync(store: &Arc<Mutex<SqliteStore>>, root: &Path, human: Uuid) -> grimoire_store::Result<SyncReport> {
    let files = scan(root);
    let mut report = SyncReport { projects: files.len(), ..Default::default() };
    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
    let agent = crate::room::agent_principal(&mut s)?;
    let root_doc = ensure_folder(&mut s, None, ROOT_TITLE, human)?;
    let mut projects: Vec<_> = files.into_iter().collect();
    projects.sort_by(|a, b| a.0.cmp(&b.0));
    for (project, mut paths) in projects {
        paths.sort();
        let folder = ensure_folder(&mut s, Some(root_doc), &project, human)?;
        for path in paths {
            report.files += 1;
            let Ok(md) = std::fs::read_to_string(&path) else {
                report.skipped += 1;
                continue;
            };
            if md.trim().is_empty() {
                report.skipped += 1;
                continue;
            }
            let key = format!("memory.hash.{project}/{}", path.file_name().unwrap_or_default().to_string_lossy());
            let h = hash(&md);
            if s.get_setting(&key)?.as_deref() == Some(h.as_str()) {
                report.unchanged += 1;
                continue;
            }
            let (title, doc_md, refs) = to_doc_markdown(&project, &path, &md);
            match find_child(&s, Some(folder), &title) {
                None => {
                    let (doc_id, _) = grimoire_store::import::import_markdown(&mut *s, &title, Some(folder), agent, &doc_md)?;
                    // provenance on the import's ops: they were created green by
                    // import; record the origin on the doc's first op via a
                    // no-op-safe setting alongside the hash
                    s.set_setting(&format!("memory.origin.{doc_id}"), &refs.join(" · "))?;
                    report.imported += 1;
                }
                Some(doc_id) => {
                    let tree = s.read_doc(doc_id)?;
                    let mut ops = grimoire_store::mddiff::markdown_to_ops(&tree.roots, &doc_md);
                    if ops.is_empty() {
                        report.unchanged += 1;
                    } else {
                        for op in &mut ops {
                            op.source_refs = refs.clone();
                        }
                        // a memory the model rewrote: reviewable, never silent
                        s.propose_reviewed(doc_id, tree.doc.current_epoch, agent, ops)?;
                        report.updated += 1;
                    }
                }
            }
            s.set_setting(&key, &h)?;
        }
    }
    Ok(report)
}

/// Start-up sync, then every 10 minutes. A missing `~/.claude/projects` is
/// simply "nothing to do".
pub async fn memory_loop(store: Arc<Mutex<SqliteStore>>, human: Uuid) {
    let root = memory_root();
    loop {
        if root.is_dir() {
            let s = store.clone();
            let r = root.clone();
            match tokio::task::spawn_blocking(move || sync(&s, &r, human)).await {
                Ok(Ok(rep)) if rep.imported + rep.updated > 0 => {
                    tracing::info!(imported = rep.imported, updated = rep.updated, files = rep.files, "claude memory synced")
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("claude memory sync failed: {e}"),
                Err(e) => tracing::warn!("claude memory sync panicked: {e}"),
            }
        }
        tokio::time::sleep(INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::PrincipalKind;

    #[test]
    fn project_names_drop_the_account_prefix_and_scratch_dirs() {
        assert_eq!(project_name("-Users-tmeaney-qumulo-qompass").as_deref(), Some("qumulo-qompass"));
        assert_eq!(project_name("-Users-tmeaney-personal-sembler").as_deref(), Some("personal-sembler"));
        assert_eq!(project_name("-Users-tmeaney").as_deref(), None);
        assert_eq!(project_name("-private-tmp-claude-504-x-scratchpad").as_deref(), None);
    }

    #[test]
    fn frontmatter_both_shapes_become_title_tags_and_refs() {
        let flat = "---\nname: always-verify-latest-deps\ndescription: never pin from memory\ntype: feedback\noriginSessionId: abc\n---\n\nBody here.\n";
        let (title, md, refs) = to_doc_markdown("proj", Path::new("/x/always-verify-latest-deps.md"), flat);
        assert_eq!(title, "Always Verify Latest Deps");
        assert!(md.starts_with("---\ntags:\n  - claude-memory\n  - memory-feedback\n---\n\n# Always Verify Latest Deps\n\n*never pin from memory*\n\nBody here.\n"));
        assert_eq!(refs, vec!["claude-memory: proj/always-verify-latest-deps.md", "session: abc"]);
        let nested = "---\nname: codeshare project context\ndescription: d\nmetadata:\n  type: project\n  originSessionId: s1\n---\nBody\n";
        let (title, md, refs) = to_doc_markdown("p", Path::new("/x/codeshare.md"), nested);
        assert_eq!(title, "Codeshare Project Context");
        assert!(md.contains("memory-project"));
        assert!(refs.contains(&"session: s1".to_string()));
    }

    #[test]
    fn sync_imports_once_then_proposes_changes_as_reviewable() {
        let dir = tempfile::tempdir().unwrap();
        let mem = dir.path().join("-Users-me-personal-foo/memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(mem.join("MEMORY.md"), "- index\n").unwrap();
        std::fs::write(mem.join("thing.md"), "---\nname: thing\ntype: project\n---\n\nFirst version.\n").unwrap();
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let store = Arc::new(Mutex::new(s));

        let r = sync(&store, dir.path(), tom.id).unwrap();
        assert_eq!((r.files, r.imported, r.updated, r.unchanged), (1, 1, 0, 0));
        let r = sync(&store, dir.path(), tom.id).unwrap();
        assert_eq!((r.imported, r.updated, r.unchanged), (0, 0, 1));
        {
            let s = store.lock().unwrap();
            let docs = s.list_docs().unwrap();
            let root = docs.iter().find(|d| d.title == ROOT_TITLE && d.parent_id.is_none()).unwrap();
            let proj = docs.iter().find(|d| d.title == "personal-foo" && d.parent_id == Some(root.id)).unwrap();
            let doc = docs.iter().find(|d| d.title == "Thing" && d.parent_id == Some(proj.id)).unwrap();
            let tree = s.read_doc(doc.id).unwrap();
            // the heading parents the body (heading-stack rule): search the whole tree
            fn texts(ns: &[grimoire_store::BlockNode], out: &mut Vec<String>) {
                for n in ns {
                    out.push(n.block.content.clone());
                    texts(&n.children, out);
                }
            }
            let mut all = Vec::new();
            texts(&tree.roots, &mut all);
            assert!(all.iter().any(|c| c.contains("First version")), "{all:?}");
            assert!(tree.roots[0].block.content.contains("memory-project"));
            assert!(s.review_queue(None).unwrap().is_empty());
        }
        // the model rewrites the memory → a reviewable yellow, not a silent overwrite
        std::fs::write(mem.join("thing.md"), "---\nname: thing\ntype: project\n---\n\nSecond version.\n").unwrap();
        let r = sync(&store, dir.path(), tom.id).unwrap();
        assert_eq!((r.imported, r.updated), (0, 1));
        let s = store.lock().unwrap();
        let q = s.review_queue(None).unwrap();
        assert_eq!(q.len(), 1, "one reviewable change");
        assert!(q[0].op.source_refs.iter().any(|r| r.starts_with("claude-memory: personal-foo/thing.md")));
    }
}
