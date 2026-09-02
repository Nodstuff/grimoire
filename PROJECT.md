> **Status (2026-09-01):** this is the founding design record, kept as written. Since
> then everything in it has been built, including all of §6 and §7: v1 (M1–M5),
> federation over iroh with trust tiers (M6/M7, ADR 0002), hot docs with presence,
> auto-hot and comments-as-chat (M8, ADR 0003), a signed + notarized dmg, and the
> React Flow canvas with live collab (M9). Where this file says "parked" or "design
> constraints only", read the ADRs in `docs/adr/` and the `[[Grimoire]]` doc tree in
> Grimoire itself for what exists. The repo is `Nodstuff/grimoire` (renamed from
> `knowledge-system`).

# PROJECT.md — Personal Knowledge System That Maintains Itself

**Context document for Claude Code.** This captures the full design conversation (Aug 2026) that produced the backlog in this repo. Read this before touching any ticket. The backlog (GitHub issues — the source of truth) says *what*; this says *why* and *how*.

---

## 0. Ground rules

1. **Claude builds this.** This document is the build spec. Normal collaborative development throughout: Claude Code implements, Tom directs, reviews, and makes the calls on anything marked as a decision ticket (ADRs, the editor spike). The crdt-lab milestone (M1) is optional background material, not a gate — the substrate (M2) does not depend on it and can start immediately. If M1 is built, build it properly with the property tests; its types ship in live canvas sync later.
2. **Solo-first is a soft rule for sequencing, not a ceiling.** v1 is built for one human user (Tom) plus agents — that ordering stands, and the de-gilding decisions (constants over config, a column over a policy system) stand for v1. But multi-human collaboration — shared editing, whiteboarding, the distributed data-bank/federation idea — is the roadmap, not a hypothetical. Phase 2 (§7) contains real designs, not parked dreams. The practical consequence: every v1 implementation choice must respect the federation tax (§6) and must not foreclose the hot/cold document pattern (§7). When two v1 implementations are equal, pick the one that makes phase 2 cheaper.

## 1. Vision

A **personal knowledge system that maintains itself**: local-first, owned data (in the spirit of Obsidian/Octarine/ATLAS:Face), but where AI agents are not a chat sidebar bolted on — they are **first-class principals living inside the merge semantics**.

**The bar**: the best docs platform for humans *and* agents to coexist on — fast, efficient, and good-feeling for humans; token-cheap, structurally addressable, and safely writable for agents. Confluence is good for neither; every other tool picks one side. Blocks themselves are commodity (Notion/AnyType/ADF all have them) — the differentiator is treating *writes* as first-class objects: base epochs, confidence-scored merge, principal provenance. Documents bound to external sources (GitHub repos) update themselves daily via agents whose writes flow through a merge/review gate. The eternal wiki disease is rot; this is the first architecture where docs can plausibly stop being lies, because agent writes are cheap, safe, attributed, and reviewable.

Current state being replaced: Tom's Octarine markdown vault, which agents write to, but which is single-laptop, has no merge semantics, no provenance, no review, and no self-maintenance.

**Origin**: started as "learn Rust by building CRDTs from scratch," grew into this. The curriculum and the product share a spine deliberately.

**North star (planned, sequenced after v1)**: the distributed data bank — federated instances where everyone runs their own, and shares *sections* (subtrees) with teammates or their agents (e.g. docs for a tool you built), with real-time co-editing and whiteboarding inside shared sections. v1 pays the federation tax up front (§6) so this layers on without a rewrite.

**Non-goals for v1** (not forever): replacing Confluence, enterprise anything, building a canvas editor from scratch, competing with agent-orchestration platforms. Live collaborative editing and whiteboarding are **phase 2, on the roadmap** (§7) — designed now, built when the substrate is solid.

---

## 2. Conceptual foundations (the 5am CRDT conversation, distilled)

These concepts underpin every design decision. Tom understands them; Claude should use this vocabulary consistently.

**CRDTs**: data structures whose merge is commutative, associative, idempotent (semilattice), so replicas converge regardless of delivery order. Lamport clocks are an *ingredient* (ordering/tiebreaks), not the thing itself. A G-Counter is structurally a vector clock — same skeleton, different interpretation.

**LWW**: trivially simple because it's allowed to destroy data — concurrent writes resolved by timestamp + replica-id tiebreak; the loser is silently discarded. Causally-later writes always win (receive bumps clock); only truly concurrent writes are arbitrary. Fine for properties; unacceptable for text.

**Sequence CRDTs (RGA/Yjs/YATA)**: indices are poison; every element gets a permanent identity; inserts anchor to identities; deletes are tombstones (others may anchor to them); concurrent same-anchor inserts ordered deterministically by (timestamp, replica-id). Hard parts: tombstone accumulation, metadata size, interleaving pathologies. **Decision: we never build this for production.** Yjs exists and is battle-tested; from-scratch RGA is optional curriculum only (ticket 1.8). Yjs's tombstone trick worth knowing: delete content immediately, keep identity *ranges* in a run-length-encoded DeleteSet.

**Causal stability & GC**: a tombstone is collectable only when every replica has seen the delete — one permanently-offline replica blocks GC forever. You cannot distinguish "slow" from "gone" without someone making a call. Practical systems therefore add bounded coordination:

**Leases/epochs (the key insight of the whole design)**: convert the unbounded academic guarantee into a bounded engineering one. Replicas that sync within the window merge as peers; stragglers past it take a slow path — resync + best-effort re-application of their edits as *new* operations. The window is a product decision, not maths. Measure it in epochs (server/authority-side counters), never client wall clocks.

**The review gate**: best-effort re-applied edits are confidence-scored. Confidently placed → **yellow** (applied, flagged for review). Unplaceable or overlapping-with-others' edits → **red** (parked, original text preserved verbatim — a red pointing at nothing is data loss with extra steps). Deletes biased aggressively toward red (a wrong insert annoys; a wrong delete destroys). Clean → **green** (auto-applied). Review state lives as *annotations referencing ops/blocks*, never baked into content — accepting a yellow is clearing an annotation, not an edit.

**The architecture pattern**: a coordination-free CRDT core, wrapped in small explicit doses of coordination (epochs, leases, review gates) placed where the product can afford them. Everything exotic happens at the edges and re-enters through the gate; the kernel never depends on the heuristic path.

---

## 3. Architecture

### 3.1 Block store (the substrate — M2)

- **Block** = `{id, doc_id, parent_id, order_key, block_type, content, created_by, epoch, deleted}`. Types: paragraph, heading, code, diagram_d2, diagram_mermaid, canvas_scene, comment, decision. Docs are trees of blocks. Ordering scheme (fractional index vs sibling list) decided in ticket 2.2.
- **The ledger (primary write record)**: an append-only `ops` table — `{id, doc_id, op: insert|replace|delete|move, target_block, payload, principal, base_epoch, epoch_applied?, verdict, confidence, source_refs[]}`. The `blocks` table is the *projection* of applied ops, written in the same transaction, and stays authoritative for reads — no replay machinery, no event-sourcing framework. Everything the design needs falls out of this one structure: parked reds = unapplied ops (original text preserved by construction), yellows = annotations referencing ops, provenance = op metadata, history/time-travel = a query, federation sync (§6) = "ship ops since cursor" — the gardener cursor pattern reused. Op granularity is **exactly** the block-level operations above, never finer; if the ops table burned down, the blocks table still works as a plain wiki.
- **Why blocks, not characters**: agents edit in semantic units (replace paragraph, insert section). Block-level merge is dramatically simpler than sequence CRDTs, confidence scoring is easier because blocks carry meaning, and two humans rarely wordsmith one sentence simultaneously — and there's only one human anyway.
- **Principals**: `{id, kind: human|agent|remote, display_name, pubkey?}`. Every block version records `(principal, epoch, source_refs[])`. Provenance is a native property — "which principal tends this region" is queryable and renderable. `remote`/`pubkey` unused in v1 (federation tax, §6).
- **Epochs**: monotonic **per-document** (never global — federation-critical). Reads return the epoch; writes declare their base epoch. Stale base → routed through the propose gate. Epochs serialise writes above the storage layer. **One epoch = one committed transaction**, which may contain many ops: a gardener's daily batch lands as one epoch (hence the changelog line "epoch 214: updated deploy runbook"), a human edit session commits as one. This resolves per-write vs per-batch — there is no tension, the unit is the transaction.
- **Human write path**: a human editing at the *current* epoch writes directly — the gate is for agent principals and stale bases, not a toll booth on typing. (Their ops still land in the ledger for provenance/history.) `proposer ≠ approver` binds gate traffic only.
- **Propose gate**: `propose(doc, ops[], base_epoch) → per-op {green|yellow|red, confidence, placement_context}`. Semantics as in §2. **Invariant: proposer ≠ approver**, enforced at the gate.
- **Confidence scoring**: ops target block IDs, so the fast path is exact, not fuzzy — target block unchanged since `base_epoch` → green with no matching at all. The fuzzy machinery (diff-match-patch spirit) is the *rare* path, engaged only when the target was edited/split/deleted since base: surrounding context found exactly once → high; fuzzy/multiple matches → medium; gone or overlapping another's edit → low. One hardcoded threshold constant.
- **Review policy**: nullable column per doc — `human-review | agent-review | auto`; null inherits parent via one recursive lookup. Auto self-applies greens + high-confidence yellows; **reds always park** for some reviewer. **Expected steady state: most docs are `agent-review`**, with the reviewer agent (4.8) as the default queue consumer; `human-review` is the exception reserved for high-stakes docs. Queue rot is designed away by making an agent, not Tom, the primary reviewer.
- **Links**: `[[wikilink]]` → first-class edge rows `{from_block, to_target}`; backlinks are an index query. Feeds reviewer-agent context ("what links here").
- **Tags**: metadata + a taxonomy doc that is *itself just a doc*, maintained by the tagging gardener via its own proposals — so tagging converges on Tom's vocabulary.
- **Import/export**: Octarine markdown vault → block trees (headings→structure, fences→code/diagram blocks, wikilinks→edges) and back out. Escape-hatch honesty.

### 3.2 Storage (ADR, decided in principle — ticket 2.1)

**SQLite** (WAL mode, busy_timeout) behind a `BlockStore` **trait**. Ledger (`ops`) and projection (`blocks`) are both plain tables written in one transaction — no queue, no replay, no framework. Rationale: single-writer is a non-issue because (a) epochs + the gate already serialise writes architecturally, (b) one daemon process owns the file (§3.2a) — within one process the "single writer" is a mutex around a µs-scale commit, (c) write load is comical (a few humans' worth at most, gardeners once daily, live-session traffic never touches the store mid-flight). Postgres arguments (LISTEN/NOTIFY, pgvector, multi-process) are real but not v1 — the trait bounds the swap cost at "reimplement the trait." Do not start on Postgres out of concurrency fear the design already engineered away. Alternatives audited and rejected for v1: libSQL (server-mode SQLite — the credible upgrade path if multi-writer ever becomes real; the trait is the escape hatch), DuckDB (OLAP, wrong shape), embedded KV stores (redb/RocksDB — lose SQL, FTS5, and the plain-tables honesty). FTS5 for search; sqlite-vec if embeddings ever happen.

### 3.2a Process topology (the daemon)

**One Rust daemon process** owns the SQLite file — literally, not aspirationally — and serves every surface:

- **MCP over streamable HTTP** (latest spec, stateless) at `localhost:<port>` — every Claude session's MCP config points here. **Never stdio**: stdio spawns one server process per session, which multi-writes the file and deletes the storage rationale.
- **Web UI (M5)** — same process, same HTTP server, different routes.
- Kept alive by launchd; started on demand is fine for v1.

Consequences: the 3.1 mini-ADR (Rust vs TS shim) mostly dissolves — the MCP surface is routes on the daemon, so Rust wins by default. And the daemon is exactly the thing federation (§6) eventually exposes — the phase-2 shape falls out of the v1 topology for free.

### 3.3 MCP surface (M3)

Tools: `list_docs`, `read_doc` (returns epoch), `read_block`, `propose` (returns structured verdicts — agents act on them without prose parsing), `search`, `add_comment`/`list_comments`, `diff_since`. The read path is designed to the same bar as the write gate — token-cheap, structurally addressable:

- **`diff_since(doc, epoch)`**: the ops since that epoch — a single SELECT off the ledger. One primitive, three hats: stale-agent recovery, gardener "what changed since my cursor", federation sync (§6) later.
- **`read_doc` outline mode**: structure + block IDs + heading/first-line per block within a token budget; the agent fetches full blocks by ID only where it needs them. Never pay full-doc tokens to edit one paragraph.
- **`search` returns blocks**, not docs — each hit with doc + heading breadcrumb, so the agent lands on the editable unit directly.

**Stale-epoch protocol**: propose against an old epoch returns `{current_epoch, missed_ops}` (via `diff_since`) — not "resync required, re-read everything," but "here are the 3 ops you missed." A scripted stale agent must recover unaided. This kills the classic bug: agent clobbers edits made mid-task.

First demo target (end of M3): an agent reads the imported vault and proposes an edit Tom reviews as a diff.

### 3.4 Gardeners (M4)

A gardener is **config, not construction**: a registry row `{id, scope (doc/section subtree), prompt, bindings[], creds_ref, schedule, budget, confidence_policy}`. The "agent builder" is a form/CLI over this row. Deliberately not an orchestration platform.

- **Prompt split**: fixed platform preamble (you propose rather than write, cite sources in provenance, stay in scope) + per-gardener task prompt. An adversarial task prompt must not be able to override propose discipline.
- **Scheduling**: once daily, 16:00 cutoff. **Pull, not push** — no webhooks; GitHub compare API against a stored cursor (`cursor_sha` per binding), advanced **only on successful run** (failed Tuesday → Wednesday covers both days). Daily batching = the 4pm run *is* the epoch cut; provenance reads like a changelog ("epoch 214: updated deploy runbook, citing PRs #341, #344"); and review arrives as one digest, not a trickle humans learn to ignore. Debounce/settle-window machinery deliberately deleted by this design.
- **Budgets**: two hardcoded constants (tokens, tool calls). Overrun → kill + red "couldn't complete" note with partial context. Never a hang, never silent.
- **Security model**: the propose gate is the injection firewall. Threat = hostile content in sources (malicious PR descriptions/commit messages). Blast radius of a compromised gardener = weird yellow/red proposals with provenance, declined by a human. Creds: fine-grained scoped PATs in OS keychain or chmod-600 file, injected as tool auth, **never in prompt text**. One injection canary test in CI-spirit (ticket 4.9).
- **Reviewer agent** (4.8): a gardener variant reading the review queue instead of GitHub. Skeptic prompt, distinct principal, verifies placement by re-reading context, accepts/rejects/re-places yellows *and* reds on `agent-review` docs. Agent-resolved reds carry a distinct provenance mark. **Build order: immediately after the tagging gardener (4.4)** — it is the default queue consumer for the whole system (most docs are `agent-review`), not a late add-on. Ticket numbering unchanged; sequencing is.
- **Escalation tripwire** (4.9): volume-based, one hardcoded threshold — >N reds resolved on one doc in one run escalates the batch to human regardless of policy. An agent resolving one red is working; twenty on one doc means something upstream broke (mangled import, hostile diff).
- **First gardener = tagging gardener** (4.4): needs no external creds, exercises the entire propose→review loop before GitHub integration exists.
- **Trust invariants (hold these three and tiered autonomy stays auditable)**: proposer ≠ approver; distinct provenance for agent-resolved reds; volume escalation.

Killer demo: PR merges → architecture diagram updates itself (D2 text diff, rendered before/after) → Tom approves with coffee.

### 3.5 Human UI (M5)

- **Stack (decided 2026-08-31)**: React + TypeScript + Vite, built bundle served as static assets by the daemon (§3.2a) — no Node in production. Forced by the embeds, pleasantly: Tiptap has first-class React bindings (5.1), tldraw is React-only (5.8), react-force-graph wraps 5.10. Vite dev server proxies to the daemon during development.

- **Editor**: Tiptap/ProseMirror spike (5.1) — timeboxed; typing must feel right in 90 seconds (adoption-critical, the "unglamorous 85%"). Yjs-compatibility constraint **dropped** with live mode parked, so this decision is now cheap to reverse.
- **Review queue** (5.3): yellows/reds, provenance, diff view, accept/decline. Sorted by date it *is* the daily digest — no email/render pipeline.
- **Provenance rendering** (5.4): per-block authorship (Tom vs gardener vs reviewer), epoch, citations; "what gardeners changed this week" as one filter.
- **Comments** (5.5): comments are blocks with `refers_to`; threads are trees; agent comments visually distinct; threads survive block edits.
- **Decision blocks & status** (5.6): draft/in-review/decided/superseded; decision blocks with who/when; `supersedes` links. Dogfood the project's own ADRs. Makes the corpus queryable ("all decided docs touching retention") and gives gardeners triggers (superseded design doc → wake the runbook gardener).
- **Diagrams** (5.7): text-to-diagram only. D2 preferred (better layout for architecture), Mermaid also supported (training-set ubiquity). A diagram is a block whose content is source — versioned, merged, provenance'd, *agent-editable* unchanged. Diagram diff = text diff + rendered before/after. **No canvas editor built from scratch, ever** — that's tldraw's whole company.
- **Canvas** (5.8): tldraw/Excalidraw embed, scene JSON as opaque-ish block content through the gate. No live sync in v1.
- **Quick-switcher** (5.9): Cmd-K over FTS5 + trigram fuzzy ("gardnr bugdet" → budgets doc).
- **Graph view** (5.10): read-only, never writes. 2D force layout first (legibility), 3D toggle for joy (3d-force-graph over three.js — a week of wiring, not a renderer project). Nodes=docs, edges=links, clusters=tags, **tint = tending principal** — a visualization nothing else can draw because nothing else has principal-level provenance.

---

## 4. crdt-lab (M1 — optional, non-blocking)

Library crate, no persistence, no network. Arc: **G-Counter** → **Merge trait** → **PN-Counter** → **LWW-Register** (Lamport clocks) → **OR-Set add-wins** → **proptest suite encoding the semilattice laws** (commutativity, associativity, idempotence) against every type → optional **baby RGA** (background only; production live-text is Yjs).

Status: originally a learning milestone, now optional background. M2 does not depend on it — the substrate uses block-level merge + epochs, not these types directly. Build it if/when useful; the payoff is that the shape-level CRDTs (LWW map + OR-Set) are exactly what live canvas sync (P2.5) ships in production, so if built, build it properly with the property tests.

---

## 5. Inspirations & competitive framing

- **ATLAS:Face** (atlasface.resistancelabs.tech): inspiration for local-first ownership, links, graph view, and the *feel* of a personal knowledge universe. Also the cautionary tale — v0.0.50 with a 3D map, EPUB reader, handwriting, YouTube quizzes, SRS, publishing, and a marketplace is a genre's checklist shipped before finding what it's for. Adopted: links, tagging, fuzzy search, graph. Rejected: everything else, and the sprawl.
- **vs. every PKM "AI feature"**: they let AI write directly into your data or quarantine it in chat. Here agents *propose* through a gate with provenance and confidence — honest agent-write semantics are the differentiator. Auto-tagging that lands as reviewable yellows (declinable as a batch) beats auto-tagging that just happens to you.
- **vs. git-based workflows**: agents are good at PRs, but that's merge-as-ceremony. This is merge-on-write for living documents, with a review model native to prose blocks rather than diffs of lines.
- **Confluence**: structurally incapable on the axis that matters (HTML soup, whole-page PUTs, no agent principals) — but *not the target*. It was the imagined customer.

---

## 6. Federation (north star — design constraints only)

The eventual shape: everyone runs an instance; **sections (subtrees) are shared** with other people or their agents. Key insight: **a remote person/agent is architecturally identical to a gardener** — an external principal whose writes arrive through the propose gate with provenance, judged by confidence, routed by the section's review policy. Trust tiers fall out of existing machinery: stranger's instance → everything yellows; trusted peer → greens flow. Sync is **pull-based** on the existing cursor pattern ("fetch their epochs since my cursor") — no always-on relay. The 5am lease/epoch/rebase design for stale laptops *is* the federation protocol.

**The v1 federation tax — already paid in M2 tickets, keep it paid:**
1. All IDs are UUIDs, never autoincrement (2.2).
2. Principal `kind: remote` + optional `pubkey` field exist in the schema (2.3).
3. Epochs are strictly per-document, never global (2.4).

Everything else (identity, discovery, transport, protocol versioning — the Matrix/SSB graveyard) waits until someone else's instance actually wants Tom's sections. That demand is the real test of the whole idea.

---

## 7. Phase 2 — the collaboration roadmap (grey `phase-2` label, no milestone yet)

This is where the project is going: teammate collaboration — co-editing docs, live whiteboarding, drawing — and the distributed data bank (§6). Sequenced after the v1 substrate because every one of these builds on the gate/epoch machinery. Full designs exist in the founding conversation; summaries in the issues. When v1 is stable, these get milestones.

- **P2.1 Live text mode**: hot documents — Yjs + editor collab bindings + websocket relay; doc goes hot explicitly or on second concurrent editor; character-level collab with cursors inside the session; **epoch frozen** during. Buy-not-build: never write YATA for production.
- **P2.2 Cool-down flatten**: session end (explicit/idle) → Yjs state flattens to blocks → lands as **one propose at one epoch**; provenance = "live session, principals, duration"; comment anchors remapped via the 2.6 confidence machinery. The gate and gardeners never know character-level chaos happened.
- **P2.3 Gardener hot-doc rule**: defer/queue proposals against hot docs; retry after flatten.
- **P2.4 Presence & awareness**: block-level presence cold (avatar on active block — conflict *avoidance*), cursor awareness hot. Ephemeral websocket state, **never persisted** — presence is not document state.
- **P2.5 Live canvas sync / whiteboarding**: shape-level LWW + add-wins OR-Set (crdt-lab types in production; canvas collab is *much easier* than text — per-property LWW is acceptable UX for shapes; arrows reference stable shape IDs). The whiteboarding/drawing surface for teammate sessions; may be pulled forward if the itch wins.
- **P2.6 Intra-block diff3**: `diff3(base, theirs, mine)` upgrade for two humans in one paragraph; 2.6 covers the solo case.
- **P2.7 Federation sync**: §6.

**Hot/cold is the pattern that keeps the substrate honest**: cold = blocks, epochs, gate, gardeners; hot = exotic live collab at the edge; everything re-enters through the gate as one clean commit.

---

## 8. Development environment & conventions

- **Repo**: github.com/Nodstuff/grimoire. Personal account `Nodstuff`; a second gh account may also be in the keyring — **never mix**. Known footgun: `git push` over HTTPS follows the OS credential helper's cached creds, not gh's active account. Recommended: direnv `.envrc` in the project root with `GH_TOKEN` (personal fine-grained PAT) so any shell/Claude Code session in this directory is automatically the personal identity. Scoped PATs per purpose — the gardener creds story starts here.
- **Tickets**: GitHub Issues are the source of truth, labels `epic:*`/`type:*`/`phase-2`, milestones M1–M5. Close issues from commit messages — provenance-flavoured workflow, fittingly.
- **Dependencies flow** M1 → M2 → M3 → M4; M5 can start after M2 in parallel.
- **Cutover bar (defines "viable")**: full one-shot cutover from the Octarine vault as soon as these four hold — 2.8 import (vault in, links intact) · 5.1 editor spike passing (daily notes get written here or nowhere) · M3 MCP surface (the real cutover moment is `~/.claude/CLAUDE.md` pointing Claude sessions at this system instead of the vault) · 5.9 quick-switcher. **Gardeners (M4) are explicitly not on the cutover path** — they are the essential fast-follow: quality upgrades to a system already inhabited, not the reason to move in. Crossover period is short by design; nothing else in M5 blocks cutover.
- **ADRs**: written as docs, dogfooding the decision-block format by hand until 5.6 exists. Two decisions flagged expensive to reverse: storage (2.1, made) and editor (5.1, spike pending).
- **Tooling**: Claude Code as the primary build tool across all milestones. Zed with the agent panel as the alternative surface when Tom wants to drive (buffer + rust-analyzer diagnostics visible live).
- **De-gilding rule (v1)**: anywhere a spec says "configurable / per-X policy / encrypted vault / digest pipeline," the v1 answer is a constant, a column, a keychain entry, or a sorted list. Re-gild when phase 2 makes it real — but only then.

## 9. Ticket map (details in issues)

- **M1**: 1.1 setup · 1.2 G-Counter · 1.3 Merge trait · 1.4 PN-Counter · 1.5 LWW-Register · 1.6 OR-Set · 1.7 proptest laws · 1.8 (stretch) baby RGA
- **M2**: 2.1 storage ADR/trait · 2.2 block model (UUIDs) + ops ledger · 2.3 principals/provenance · 2.4 per-doc epochs · 2.5 propose gate · 2.6 confidence · 2.7 annotations · 2.8 md import · 2.9 md export · 2.10 review-policy column · 2.11 links/backlinks · 2.12 tags model
- **M3**: 3.1 daemon skeleton + MCP over streamable HTTP (mini-ADR resolved: Rust, routes on the daemon) · 3.2 propose tool · 3.3 stale-epoch protocol (`diff_since`-based) · 3.4 FTS5+trigram search, block-granular results (stretch: sqlite-vec) · 3.5 comment tools · 3.6 `diff_since` tool · 3.7 `read_doc` outline mode
- **M4**: 4.1 registry · 4.2 prompt split · 4.3 creds · 4.4 tagging gardener (first) · 4.5 daily runner+cursors · 4.6 budgets · 4.7 GitHub binding · 4.8 reviewer agent · 4.9 tripwire+canary
- **M5**: 5.1 editor spike · 5.2 nav · 5.3 review queue · 5.4 provenance UI · 5.5 comments UI · 5.6 decisions/status · 5.7 D2/Mermaid · 5.8 canvas embed · 5.9 quick-switcher · 5.10 graph view
- **Parked**: P2.1–P2.7 (§7)
