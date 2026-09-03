//! Tuning harness for ask-the-vault retrieval, ignored by default. Run it
//! against a real vault to see what each question actually retrieves:
//!   PROBE_DB=/path/to/ks.db cargo test -p grimoire probe -- --ignored --nocapture
//! Keep the question list honest: things you know the vault answers.
#[cfg(test)]
mod probe {
    #[test]
    #[ignore]
    fn score_distribution() {
        let db = std::env::var("PROBE_DB").unwrap();
        let s = grimoire_store::SqliteStore::open(&db).unwrap();
        let e = crate::embed::Embedder::load().unwrap();
        e.load_index(&s).unwrap();
        // where does the block we KNOW is right sit, for Q1?
        {
            use grimoire_store::BlockStore;
            let q = "what rules do I follow about pinning dependency versions?";
            let qv = e.encode_one(q);
            let words = crate::ask::keywords(q);
            for d in s.list_docs().unwrap().iter().filter(|d| d.title.contains("Verify Latest") || d.title == "Review Gate") {
                let t = s.read_doc(d.id).unwrap();
                fn walk(ns: &[grimoire_store::BlockNode], out: &mut Vec<grimoire_store::Block>) { for n in ns { out.push(n.block.clone()); walk(&n.children, out); } }
                let mut bs = Vec::new(); walk(&t.roots, &mut bs);
                for b in bs {
                    let c = e.score(&qv, b.id);
                    let lc = format!("{} {}", d.title, b.content).to_lowercase();
                    let cov = words.iter().filter(|w| lc.contains(crate::ask::stem(w).as_str())).count();
                    println!("  [{}] cos={:?} cov={cov}/{} :: {}", d.title, c.map(|x| (x*1000.0).round()/1000.0), words.len(), b.content.replace('\n'," ").chars().take(70).collect::<String>());
                }
            }
            let all = e.search(q, 400);
            println!("  dense rank of blocks scoring >= 0.40: {}", all.iter().filter(|(_, sc)| *sc >= 0.40).count());
        }
        for q in [
            "what rules do I follow about pinning dependency versions?",
            "why do we not hit a write bottleneck with a single process?",
            "how does the review gate score a stale base?",
            "what did we decide about the CNQ grant flow?",
        ] {
            let chosen = crate::ask::retrieve(&s, Some(&e), q);
            println!("\nQ: {q}\n  chosen: {} blocks", chosen.len());
            let qv = e.encode_one(q);
            let words = crate::ask::keywords(q);
            for (i, h) in chosen.iter().enumerate() {
                let lc = format!("{} {}", h.doc_title, h.block.content).to_lowercase();
                let cov = words.iter().filter(|w| lc.contains(crate::ask::stem(w).as_str())).count();
                println!("  {i:2} cos={:.2} cov={cov}/{} [{}] {}", e.score(&qv, h.block.id).unwrap_or(0.0), words.len(), h.doc_title, h.block.content.replace('\n', " ").chars().take(70).collect::<String>());
            }
        }
    }
}
