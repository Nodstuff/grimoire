//! Block embeddings for "ask the vault" (and anything else that wants
//! "what is this like?"): potion-base-8M, a Model2Vec STATIC model —
//! tokenize, look up, mean — so embedding is microseconds per block, runs on
//! the CPU, needs no service, and the model files ship INSIDE the binary
//! (rust-embed over `models/potion-base-8M`, fetched sha256-pinned at build
//! time by `build.rs`). Nothing is downloaded at runtime, ever.
//!
//! The unit is the block. `embed_loop` re-embeds exactly the blocks whose
//! vector is missing or older than the block's epoch (see
//! `BlockStore::stale_block_vectors`): editing one paragraph of a 200-block
//! doc re-embeds one paragraph; a tombstoned block's vector is purged. The
//! in-memory index mirrors the `block_vec` table for brute-force cosine
//! search — a few thousand 256-d vectors is a couple of MB and a sub-ms scan.

use grimoire_store::{BlockStore, SqliteStore};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

#[derive(rust_embed::RustEmbed)]
#[folder = "models/potion-base-8M"]
struct EmbeddedModel;

pub struct Embedder {
    model: model2vec_rs::model::StaticModel,
    pub dim: usize,
    /// block id → unit vector (the model normalises), live blocks only
    index: RwLock<HashMap<Uuid, Vec<f32>>>,
}

const BATCH: usize = 256;
/// Idle poll for stale blocks. Cheap: one indexed query returning nothing.
const TICK: std::time::Duration = std::time::Duration::from_secs(2);

impl Embedder {
    pub fn load() -> anyhow::Result<Self> {
        let file = |name: &str| {
            EmbeddedModel::get(name)
                .map(|f| f.data.into_owned())
                .ok_or_else(|| anyhow::anyhow!("embedding model file {name} not compiled in"))
        };
        let model = model2vec_rs::model::StaticModel::from_bytes(
            file("tokenizer.json")?,
            file("model.safetensors")?,
            file("config.json")?,
            Some(true),
        )?;
        let dim = model.encode_single("dimension probe").len();
        anyhow::ensure!(dim > 0, "embedding model produced a zero-width vector");
        Ok(Self {
            model,
            dim,
            index: RwLock::new(HashMap::new()),
        })
    }

    pub fn encode(&self, texts: &[String]) -> Vec<Vec<f32>> {
        self.model.encode(texts)
    }

    pub fn encode_one(&self, text: &str) -> Vec<f32> {
        self.model.encode_single(text)
    }

    pub fn indexed(&self) -> usize {
        self.index.read().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Nearest blocks to `query` by cosine (vectors are unit length, so a
    /// dot product), best first. Only live blocks are ever in the index.
    pub fn search(&self, query: &str, k: usize) -> Vec<(Uuid, f32)> {
        let q = self.encode_one(query);
        if q.iter().all(|x| *x == 0.0) {
            return Vec::new();
        }
        let index = self.index.read().unwrap_or_else(|p| p.into_inner());
        let mut scored: Vec<(Uuid, f32)> = index
            .iter()
            .map(|(id, v)| (*id, v.iter().zip(&q).map(|(a, b)| a * b).sum::<f32>()))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
    }

    /// Cosine of a query vector against one indexed block (None = not indexed).
    pub fn score(&self, q: &[f32], id: Uuid) -> Option<f32> {
        let index = self.index.read().unwrap_or_else(|p| p.into_inner());
        index.get(&id).map(|v| v.iter().zip(q).map(|(a, b)| a * b).sum())
    }

    /// Bring the in-memory index in line with the table (start-up).
    pub fn load_index(&self, store: &SqliteStore) -> grimoire_store::Result<usize> {
        let rows = store.block_vecs()?;
        let n = rows.len();
        let mut index = self.index.write().unwrap_or_else(|p| p.into_inner());
        index.clear();
        for (id, v) in rows {
            index.insert(id, v);
        }
        Ok(n)
    }

    /// One pass: embed up to BATCH stale blocks, store, update the index.
    /// Returns how many were (re)embedded. Frontmatter/`---` blocks get an
    /// empty vector so they count as done without polluting search.
    pub fn embed_stale(&self, store: &Arc<Mutex<SqliteStore>>) -> grimoire_store::Result<usize> {
        let stale = {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            s.stale_block_vectors(BATCH)?
        };
        if stale.is_empty() {
            return Ok(0);
        }
        let (skip, embed): (Vec<_>, Vec<_>) = stale
            .into_iter()
            .partition(|(_, _, c)| c.trim().is_empty() || grimoire_store::import::is_frontmatter(c));
        let texts: Vec<String> = embed.iter().map(|(_, _, c)| c.clone()).collect();
        let vecs = if texts.is_empty() { Vec::new() } else { self.encode(&texts) };
        let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
        let mut index = self.index.write().unwrap_or_else(|p| p.into_inner());
        for (id, epoch, _) in &skip {
            s.set_block_vec(*id, *epoch, &[])?;
            index.remove(id);
        }
        for ((id, epoch, _), v) in embed.iter().zip(vecs) {
            s.set_block_vec(*id, *epoch, &v)?;
            index.insert(*id, v);
        }
        Ok(skip.len() + embed.len())
    }

    pub fn purge(&self, store: &Arc<Mutex<SqliteStore>>) -> grimoire_store::Result<usize> {
        let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
        let n = s.purge_block_vecs()?;
        if n > 0 {
            let live: std::collections::HashSet<Uuid> =
                s.block_vecs()?.into_iter().map(|(id, _)| id).collect();
            let mut index = self.index.write().unwrap_or_else(|p| p.into_inner());
            index.retain(|id, _| live.contains(id));
        }
        Ok(n)
    }
}

/// Keeps `block_vec` current: a full catch-up at start (a few thousand
/// blocks take seconds), then a 2s poll that is one cheap query when idle.
/// Purges vectors of deleted blocks once a minute.
pub async fn embed_loop(embedder: Arc<Embedder>, store: Arc<Mutex<SqliteStore>>) {
    let started = std::time::Instant::now();
    let mut total = 0usize;
    let mut ticks = 0u64;
    loop {
        let e = embedder.clone();
        let s = store.clone();
        match tokio::task::spawn_blocking(move || e.embed_stale(&s)).await {
            Ok(Ok(n)) if n > 0 => {
                total += n;
                if n == BATCH {
                    continue; // catching up: keep going without sleeping
                }
                tracing::debug!(embedded = n, total, "block embeddings updated");
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!("embedding pass failed: {e}"),
            Err(e) => tracing::warn!("embedding task panicked: {e}"),
        }
        if total > 0 && started.elapsed() < std::time::Duration::from_secs(60) && ticks == 0 {
            tracing::info!(total, secs = started.elapsed().as_secs(), "embedding backfill complete");
        }
        ticks += 1;
        if ticks % 30 == 0
            && let Ok(n) = embedder.purge(&store)
            && n > 0
        {
            tracing::debug!(purged = n, "dropped embeddings of deleted blocks");
        }
        tokio::time::sleep(TICK).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::{PrincipalKind, import::import_markdown};

    #[test]
    fn embeds_stale_blocks_once_reembeds_on_edit_and_purges_deleted() {
        let embedder = Embedder::load().expect("model compiled in");
        assert_eq!(embedder.dim, 256);
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let (doc, _) = import_markdown(&mut s, "Notes", None, tom.id,
            "---\ntags:\n  - x\n---\n\nThe backup runs nightly with VACUUM INTO.\n\nSourdough wants a long cold proof.\n").unwrap();
        let store = Arc::new(Mutex::new(s));
        // first pass embeds everything (frontmatter gets an empty marker)
        let n = embedder.embed_stale(&store).unwrap();
        assert_eq!(n, 3);
        assert_eq!(embedder.indexed(), 2, "frontmatter is not searchable");
        assert_eq!(embedder.embed_stale(&store).unwrap(), 0, "nothing stale");
        // semantic: a paraphrase finds the right block
        let hits = embedder.search("how are database snapshots taken?", 1);
        let top = store.lock().unwrap().read_block(hits[0].0).unwrap();
        assert!(top.content.contains("backup"), "got {:?}", top.content);
        // edit one block → exactly one becomes stale
        let tree = store.lock().unwrap().read_doc(doc).unwrap();
        let target = tree.roots.iter().flat_map(|n| std::iter::once(&n.block).chain(n.children.iter().map(|c| &c.block)))
            .find(|b| b.content.contains("Sourdough")).unwrap().id;
        store.lock().unwrap().apply(doc, tree.doc.current_epoch, tom.id, vec![grimoire_store::OpInput {
            kind: grimoire_store::OpKind::Replace { target, content: "Bread needs a twelve hour rise.".into() },
            source_refs: vec![],
        }]).unwrap();
        assert_eq!(embedder.embed_stale(&store).unwrap(), 1);
        // delete the doc → vectors purged, index shrinks
        store.lock().unwrap().delete_doc(doc).unwrap();
        // tombstoning blocks is doc-level here; block rows stay but the doc is deleted —
        // block_vecs joins on block.deleted, so emulate a block delete
        let bid = target;
        store.lock().unwrap().apply(doc, tree.doc.current_epoch + 1, tom.id, vec![grimoire_store::OpInput {
            kind: grimoire_store::OpKind::Delete { target: bid },
            source_refs: vec![],
        }]).ok();
        let purged = embedder.purge(&store).unwrap();
        assert!(purged >= 1, "purged {purged}");
        // 0.7.2: blocks of a tombstoned doc leave block_vecs (never searchable)
        assert_eq!(embedder.indexed(), 0);
    }
}
