#!/usr/bin/env bash
# create-backlog.sh — personal knowledge system backlog → GitHub issues.
# Run inside the target repo clone, or: REPO=you/repo ./create-backlog.sh
# Requires gh CLI, authenticated. Run ONCE against a fresh repo (issues don't dedupe).
set -euo pipefail

REPO_FLAG=()
if [[ -n "${REPO:-}" ]]; then REPO_FLAG=(--repo "$REPO"); fi
API_REPO="${REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner)}"
echo "Target repo: $API_REPO"

label() { gh label create "$1" --color "$2" --description "$3" "${REPO_FLAG[@]}" --force >/dev/null && echo "label: $1"; }
label "epic:crdt-lab"  "1D76DB" "Rust CRDT curriculum"
label "epic:substrate" "0E8A16" "Block store, epochs, propose gate, links, tags"
label "epic:mcp"       "5319E7" "MCP surface for agents"
label "epic:gardeners" "D93F0B" "Gardener + reviewer agent runtime"
label "epic:ui"        "FBCA04" "Editor, review queue, search, graph"
label "type:learning"  "C2E0C6" "Curriculum"
label "type:infra"     "BFD4F2" "Plumbing"
label "type:feature"   "84B6EB" "Visible capability"
label "type:spike"     "E99695" "Timeboxed decision / ADR"
label "phase-2"        "CCCCCC" "Parked — unlocked by federation"

milestone() {
  gh api "repos/$API_REPO/milestones" -f title="$1" -f description="$2" >/dev/null 2>&1 \
    && echo "milestone: $1" || echo "milestone exists: $1"
}
milestone "M1 crdt-lab"    "Rust curriculum: counters, LWW, OR-Set, property tests"
milestone "M2 block-store" "Block tree, epochs, propose gate, links, tags, import/export"
milestone "M3 mcp-surface" "Agents read, propose, search"
milestone "M4 gardeners"   "Tagging gardener, GitHub gardeners, reviewer agent, tripwire"
milestone "M5 human-ui"    "Editor, review queue, quick-switcher, graph, diagrams"

issue() { gh issue create --title "$1" --milestone "$2" --label "$3" --body "$4" "${REPO_FLAG[@]}" >/dev/null && echo "issue: $1"; }
parked() { gh issue create --title "$1" --label "phase-2" --body "$2" "${REPO_FLAG[@]}" >/dev/null && echo "parked: $1"; }

# ===== M1 =====
issue "1.1 crdt-lab: project setup" "M1 crdt-lab" "epic:crdt-lab,type:infra" \
"cargo new crdt-lab --lib. proptest as dev-dep. Local cargo test + clippy — no CI until a second contributor exists.

- [ ] Empty test passes."

issue "1.2 G-Counter" "M1 crdt-lab" "epic:crdt-lab,type:learning" \
"HashMap<ReplicaId, u64> + own replica identity. increment, value, merge (pointwise max).

- [ ] Two replicas increment independently, merge both directions, converge.
- [ ] Merge idempotent."

issue "1.3 Extract Merge trait" "M1 crdt-lab" "epic:crdt-lab,type:learning" \
"trait Merge { fn merge(&mut self, other: &Self); } — retrofit G-Counter.

- [ ] Trait implemented; tests unchanged."

issue "1.4 PN-Counter" "M1 crdt-lab" "epic:crdt-lab,type:learning" \
"Two G-Counters. Composition + Default.

- [ ] Concurrent inc/dec converge to correct net value."

issue "1.5 LWW-Register" "M1 crdt-lab" "epic:crdt-lab,type:learning" \
"Generic over T: Clone. (value, lamport_ts, replica_id), deterministic tiebreak, receive bumps clock.

- [ ] Concurrent writes converge to same winner regardless of merge order.
- [ ] Causally-later write always wins."

issue "1.6 OR-Set (add-wins)" "M1 crdt-lab" "epic:crdt-lab,type:learning" \
"Unique tags per add; removes carry observed tags.

- [ ] Concurrent add+remove → present.
- [ ] Remove only kills observed adds."

issue "1.7 Property-test semilattice laws" "M1 crdt-lab" "epic:crdt-lab,type:learning" \
"Generic proptest: commutativity, associativity, idempotence — all types.

- [ ] Laws pass with generated op sequences."

issue "1.8 (Stretch) Baby RGA" "M1 crdt-lab" "epic:crdt-lab,type:learning" \
"Character sequence, tombstones, linear scan, no GC. Pure curriculum — any future live text is Yjs (phase 2).

- [ ] Replicas converge; concurrent same-anchor inserts order deterministically."

# ===== M2 =====
issue "2.1 ADR: SQLite behind a BlockStore trait" "M2 block-store" "epic:substrate,type:spike" \
"SQLite (WAL, busy_timeout) behind a trait. Epochs + propose gate serialise writes above storage — single-writer is fine for one server process. Postgres = reimplement trait if LISTEN/NOTIFY, pgvector, or multi-process ever matter.

- [ ] ADR written: decision, alternatives, consequences.
- [ ] BlockStore trait defined; no SQL leaks above it."

issue "2.2 Block model" "M2 block-store" "epic:substrate,type:feature" \
"{id, doc_id, parent_id, order_key, block_type, content, created_by, epoch, deleted}. Types: paragraph, heading, code, diagram_d2, diagram_mermaid, canvas_scene, comment, decision. Decide ordering (fractional index vs sibling list) in-ticket.

Federation-proofing, cheap, now: ALL IDs are UUIDs. Never autoincrement.

- [ ] CRUD a doc tree; round-trips.
- [ ] Zero integer IDs anywhere."

issue "2.3 Principals & provenance" "M2 block-store" "epic:substrate,type:feature" \
"Principal {id, kind: human|agent|remote, display_name, pubkey?}. remote + pubkey unused in v1 — the migration you'll never regret. Every block version records (principal, epoch, source_refs[]).

- [ ] For any block: who wrote it, when, citing what."

issue "2.4 Epochs (per-document)" "M2 block-store" "epic:substrate,type:feature" \
"Monotonic per-document — never global (federation-critical). Reads return epoch; writes declare base.

- [ ] Stale base routes to propose gate."

issue "2.5 Propose gate" "M2 block-store" "epic:substrate,type:feature" \
"propose(doc, ops[], base_epoch) → per-op {green|yellow|red, confidence, placement_context}. Green applies; yellow applies with pending annotation; red parks, original content verbatim.

- [ ] Current epoch + intact anchor → green.
- [ ] Anchor edited by another → yellow + confidence.
- [ ] Anchor deleted → red, text retrievable.
- [ ] Deletes biased red unless trivially safe.
- [ ] proposer_id != approver_id enforced."

issue "2.6 Confidence scoring" "M2 block-store" "epic:substrate,type:feature" \
"Context matching: exact-once → high; fuzzy/multiple → medium; gone/overlap → low. One threshold constant — you are the config file.

- [ ] Unit tests per band."

issue "2.7 Annotations layer" "M2 block-store" "epic:substrate,type:feature" \
"Review state as annotations referencing op/block IDs — never baked into content. Accept = clear annotation.

- [ ] Accepting a yellow adds no content change to history."

issue "2.8 Markdown import (Octarine)" "M2 block-store" "epic:substrate,type:feature" \
".md directory → block trees. Headings → structure, code fences → code, mermaid fences → diagram blocks, [[wikilinks]] → link edges.

- [ ] Vault imports without loss; spot-check 10 docs."

issue "2.9 Markdown export" "M2 block-store" "epic:substrate,type:feature" \
"Escape-hatch honesty.

- [ ] Import→export round-trip semantically stable."

issue "2.10 Review policy column" "M2 block-store" "epic:substrate,type:feature" \
"Nullable review_policy on docs: human-review | agent-review | auto; null = parent's (one recursive lookup, not an inheritance system). Auto self-applies greens + high-confidence yellows; reds always park.

- [ ] Child inherits unless set; gate consults it."

issue "2.11 Links & backlinks" "M2 block-store" "epic:substrate,type:feature" \
"[[wikilink]] resolution against doc names/aliases. Edges {from_block, to_target} as rows; backlinks = index query.

- [ ] Rename a doc, links follow.
- [ ] Backlinks correct."

issue "2.12 Tags model" "M2 block-store" "epic:substrate,type:feature" \
"Tags as metadata + a taxonomy doc (just a doc, maintained by the tagging gardener).

- [ ] Tag/untag; query by tag; taxonomy doc lists live tags."

# ===== M3 =====
issue "3.1 MCP server skeleton" "M3 mcp-surface" "epic:mcp,type:spike" \
"Rust vs thin TS shim — mini-ADR in ticket. Tools: list_docs, read_doc (returns epoch), read_block.

- [ ] Claude Desktop/Code browses the imported vault."

issue "3.2 propose tool" "M3 mcp-surface" "epic:mcp,type:feature" \
"Exposes the gate: ops in, verdicts + confidence + placement context out.

- [ ] Agent acts on structured verdicts, no prose parsing."

issue "3.3 Stale-epoch protocol" "M3 mcp-surface" "epic:mcp,type:feature" \
"Old epoch → {current_epoch, resync_required} + context to re-read and re-propose.

- [ ] Scripted stale-agent scenario recovers unaided."

issue "3.4 Search: FTS5 + trigram fuzzy" "M3 mcp-surface" "epic:mcp,type:feature" \
"FTS5 + trigram index for typo-tolerant fragments. One tool serves agents and the quick-switcher.

- [ ] 'gardnr bugdet' finds the budgets doc.
- [ ] Agents find runbooks by content.

Stretch (separate ticket if wanted): embeddings via sqlite-vec."

issue "3.5 Comment tools" "M3 mcp-surface" "epic:mcp,type:feature" \
"add_comment(block_id, body), list_comments(doc). Comments are blocks.

- [ ] Agent comment threads with correct provenance."

# ===== M4 =====
issue "4.1 Gardener registry" "M4 gardeners" "epic:gardeners,type:feature" \
"Row: {id, scope, prompt, bindings[], creds_ref, schedule, budget, confidence_policy}. Builder = CLI/form over the row.

- [ ] Create/edit/disable without code changes."

issue "4.2 System/user prompt split" "M4 gardeners" "epic:gardeners,type:feature" \
"Fixed preamble (propose, cite, stay in scope) + per-gardener task prompt.

- [ ] Adversarial task prompt cannot override propose discipline."

issue "4.3 Creds storage" "M4 gardeners" "epic:gardeners,type:infra" \
"OS keychain or chmod-600 file — no vault product. Fine-grained scoped PATs, injected as tool auth, never prompt text.

- [ ] Injection canary in a PR body cannot exfiltrate the PAT (tested in 4.9)."

issue "4.4 Tagging gardener (first gardener)" "M4 gardeners" "epic:gardeners,type:feature" \
"No external creds needed — exercises the whole loop first. Nightly sweep of untagged/changed docs; proposes tags against the taxonomy doc; converges on your vocabulary.

- [ ] Seeded untagged docs get sensible proposals.
- [ ] Declined batch leaves no trace.
- [ ] Taxonomy updates via its own proposals."

issue "4.5 Daily runner" "M4 gardeners" "epic:gardeners,type:feature" \
"Cron at 16:00. Per gardener: cursor → GitHub compare → compose → invoke → propose gate → advance cursor ONLY on success.

- [ ] Failed run leaves cursor; next run covers the gap.
- [ ] Run log: epoch, PRs cited, verdict counts."

issue "4.6 Budgets (two constants)" "M4 gardeners" "epic:gardeners,type:feature" \
"Hardcoded token + tool-call caps. Overrun → kill + red 'couldn't complete' note with partial context.

- [ ] Tiny budget produces the red note, not a hang."

issue "4.7 GitHub binding" "M4 gardeners" "epic:gardeners,type:feature" \
"{repo, paths[], cursor_sha}. Compare API, no webhooks.

- [ ] Gardener bound to deploy.yml wakes only on that path."

issue "4.8 Reviewer agent" "M4 gardeners" "epic:gardeners,type:feature" \
"Gardener variant reading the review queue. Verifies placement, accepts/rejects/re-places yellows and reds on agent-review docs. Distinct principal, skeptic prompt.

- [ ] Clears a seeded queue correctly.
- [ ] Agent-resolved reds carry distinct provenance.
- [ ] Cannot approve own proposals."

issue "4.9 Escalation tripwire + injection canary" "M4 gardeners" "epic:gardeners,type:infra" \
"One hardcoded threshold: >N reds on one doc in one run → batch escalates to human regardless of policy. Plus one canary test: adversarial PR body attempting injection/scope-escape.

- [ ] Mass-red scenario escalates.
- [ ] Canary stays green."

# ===== M5 =====
issue "5.1 Spike: editor foundation" "M5 human-ui" "epic:ui,type:spike" \
"Tiptap/ProseMirror wired to block store. Timebox, decide, commit. Yjs constraint dropped — solo; decision now cheap to reverse.

- [ ] Typing feels right for 90 seconds."

issue "5.2 Doc tree + navigation" "M5 human-ui" "epic:ui,type:feature" \
"Folders, doc list, breadcrumbs.

- [ ] Imported vault browsable."

issue "5.3 Review queue" "M5 human-ui" "epic:ui,type:feature" \
"Yellows/reds with provenance, diff view, accept/decline. Red shows original verbatim. Sorted by date — this IS the daily digest; no email pipeline.

- [ ] Agent-resolved items browsable with provenance marks."

issue "5.4 Provenance rendering" "M5 human-ui" "epic:ui,type:feature" \
"Per-block authorship: you vs gardener vs reviewer, epoch, citations on hover.

- [ ] 'What gardeners changed this week' is one filter."

issue "5.5 Comments UI" "M5 human-ui" "epic:ui,type:feature" \
"Threads on blocks, resolve state, agent comments visually distinct.

- [ ] Thread survives block edits."

issue "5.6 Decision blocks & doc status" "M5 human-ui" "epic:ui,type:feature" \
"draft/in-review/decided/superseded, decision block with who/when, supersedes links. Dogfood your own ADRs.

- [ ] 'All decided docs touching X' via search."

issue "5.7 D2/Mermaid rendering" "M5 human-ui" "epic:ui,type:feature" \
"Render on view, source on edit. Diagram diff = text diff + before/after preview.

- [ ] Gardener diagram change reviewable rendered."

issue "5.8 Canvas block (embed)" "M5 human-ui" "epic:ui,type:feature" \
"tldraw/Excalidraw embed, scene JSON as block content through the gate. No live sync (phase 2).

- [ ] Sketch persists, versioned."

issue "5.9 Quick-switcher" "M5 human-ui" "epic:ui,type:feature" \
"Cmd-K over the fuzzy search tool: docs, headings, tags.

- [ ] Typo'd fragment lands on the right doc, <100ms feel."

issue "5.10 Graph view" "M5 human-ui" "epic:ui,type:feature" \
"Read-only. 2D force layout first (legibility), 3D toggle (joy) via 3d-force-graph. Nodes = docs, edges = links, clusters = tags, tint = tending principal.

- [ ] Click node → open doc.
- [ ] Never writes."

# ===== Phase 2: parked =====
parked "P2.1 Live text mode (Yjs hot documents)" \
"Yjs + editor collab bindings + websocket relay. Doc goes hot explicitly or on second concurrent editor; epoch frozen for session. Unlocked by federation. Full design in founding conversation."

parked "P2.2 Cool-down flatten" \
"Session end → Yjs state flattens to one propose at one epoch. Provenance: live session, principals, duration. Anchor remap via confidence-scoring machinery."

parked "P2.3 Gardener hot-doc rule" \
"Gardeners/reviewers defer proposals against hot docs; retry after cool-down."

parked "P2.4 Presence & awareness" \
"Block-level presence cold, cursor awareness hot. Ephemeral websocket state, never persisted."

parked "P2.5 Live canvas sync" \
"Shape-LWW + add-wins OR-Set from crdt-lab, in production. Passion exception: allowed early if the itch wins — it's the one place your own Rust CRDTs ship."

parked "P2.6 Intra-block diff3" \
"diff3(base, theirs, mine) for two humans in one paragraph. Confidence scoring covers the solo case."

parked "P2.7 Federation sync" \
"Pull-based subtree sharing between instances. Remote principals through the propose gate; trust tiers via review policy; cursors per peer. v1 tax already paid: UUIDs, remote principal kind + pubkey, per-doc epochs."

echo ""
echo "Done. Check: gh issue list --limit 60"
