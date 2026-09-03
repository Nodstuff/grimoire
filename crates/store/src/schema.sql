-- knowledge-system substrate schema (PROJECT.md §3.1–3.2).
-- Ledger (ops) is the primary write record; blocks is the projection,
-- written in the same transaction and authoritative for reads.
-- All IDs are UUIDs stored as TEXT — never autoincrement (federation tax §6).

CREATE TABLE IF NOT EXISTS principals (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL CHECK (kind IN ('human', 'agent', 'remote')),
    display_name TEXT NOT NULL,
    pubkey       TEXT
);

CREATE TABLE IF NOT EXISTS docs (
    id            TEXT PRIMARY KEY,
    parent_id     TEXT REFERENCES docs (id),
    title         TEXT NOT NULL,
    -- null inherits parent's policy via one recursive lookup (ticket 2.10)
    review_policy TEXT CHECK (review_policy IN ('human-review', 'agent-review', 'auto')),
    -- doc lifecycle (ticket 5.6); null = plain doc, no status
    status        TEXT CHECK (status IN ('draft', 'in-review', 'decided', 'superseded')),
    -- per-document, never global (federation tax §6); one epoch = one committed transaction
    current_epoch INTEGER NOT NULL DEFAULT 0,
    created_by    TEXT NOT NULL REFERENCES principals (id),
    -- manual tree ordering (fractional, like blocks); null sorts after keyed, by title
    sort_key      TEXT,
    deleted       INTEGER NOT NULL DEFAULT 0,
    -- when the tombstone was set; one value for every doc of a single delete,
    -- so restore can revive exactly that subtree (Trash)
    deleted_at    TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS blocks (
    id         TEXT PRIMARY KEY,
    doc_id     TEXT NOT NULL REFERENCES docs (id),
    parent_id  TEXT REFERENCES blocks (id),
    order_key  TEXT NOT NULL,
    block_type TEXT NOT NULL CHECK (block_type IN
        ('paragraph', 'heading', 'code', 'diagram_d2', 'diagram_mermaid',
         'canvas_scene', 'comment', 'decision')),
    content    TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES principals (id),
    -- epoch of last modification
    epoch      INTEGER NOT NULL,
    deleted    INTEGER NOT NULL DEFAULT 0,
    -- comment blocks: the content block this comment thread anchors to
    refers_to  TEXT
);

CREATE INDEX IF NOT EXISTS blocks_by_doc ON blocks (doc_id, parent_id, order_key);

CREATE TABLE IF NOT EXISTS ops (
    id            TEXT PRIMARY KEY,
    doc_id        TEXT NOT NULL REFERENCES docs (id),
    op_type       TEXT NOT NULL CHECK (op_type IN ('insert', 'replace', 'delete', 'move')),
    target_block  TEXT,
    -- full OpKind as JSON; op_type/target_block are denormalised for querying
    payload       TEXT NOT NULL,
    principal     TEXT NOT NULL REFERENCES principals (id),
    base_epoch    INTEGER NOT NULL,
    -- NULL = parked (red) / pending; set when the projection applied it
    epoch_applied INTEGER,
    verdict       TEXT CHECK (verdict IN ('green', 'yellow', 'red')),
    confidence    REAL,
    -- pre-image of the affected block as JSON (NULL for inserts):
    -- powers decline-revert, red parking with verbatim originals, and history
    prior         TEXT,
    source_refs   TEXT NOT NULL DEFAULT '[]',
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS ops_by_doc_epoch ON ops (doc_id, epoch_applied);

-- Review state lives as annotations referencing ops — never baked into content
-- (PROJECT.md §2). Accepting a yellow is clearing an annotation, not an edit.
CREATE TABLE IF NOT EXISTS annotations (
    id          TEXT PRIMARY KEY,
    doc_id      TEXT NOT NULL REFERENCES docs (id),
    op_id       TEXT NOT NULL REFERENCES ops (id),
    -- review = applied yellow awaiting review; parked = red, not applied
    kind        TEXT NOT NULL CHECK (kind IN ('review', 'parked')),
    status      TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'accepted', 'declined')),
    resolved_by TEXT REFERENCES principals (id),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    resolved_at TEXT
);

CREATE INDEX IF NOT EXISTS annotations_open ON annotations (doc_id, status);

-- [[wikilink]] edges (ticket 2.11): to_target is the raw link text, resolved
-- to docs at query time (Octarine links are workspace paths; match by title).
CREATE TABLE IF NOT EXISTS edges (
    from_block TEXT NOT NULL REFERENCES blocks (id),
    to_target  TEXT NOT NULL,
    PRIMARY KEY (from_block, to_target)
);

CREATE INDEX IF NOT EXISTS edges_by_target ON edges (to_target);

-- FTS5 trigram index over live block content (ticket 3.4), trigger-synced.
CREATE VIRTUAL TABLE IF NOT EXISTS blocks_fts USING fts5(
    content,
    content='blocks',
    content_rowid='rowid',
    tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS blocks_fts_ai AFTER INSERT ON blocks BEGIN
    INSERT INTO blocks_fts (rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS blocks_fts_ad AFTER DELETE ON blocks BEGIN
    INSERT INTO blocks_fts (blocks_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
END;

CREATE TRIGGER IF NOT EXISTS blocks_fts_au AFTER UPDATE OF content ON blocks BEGIN
    INSERT INTO blocks_fts (blocks_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
    INSERT INTO blocks_fts (rowid, content) VALUES (new.rowid, new.content);
END;

-- Gardener registry (ticket 4.1): a gardener is config, not construction.
CREATE TABLE IF NOT EXISTS gardeners (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    kind          TEXT NOT NULL DEFAULT 'tagging'
        CHECK (kind IN ('tagging', 'reviewer', 'auditor', 'scribe', 'keeper')),
    principal     TEXT NOT NULL REFERENCES principals (id),
    -- null scope = whole corpus; else this doc's subtree
    scope_doc     TEXT REFERENCES docs (id),
    task_prompt   TEXT NOT NULL,
    -- e.g. [{"kind":"github","repo":"o/r","cursor_sha":null}] (ticket 4.7)
    bindings      TEXT NOT NULL DEFAULT '[]',
    creds_ref     TEXT,
    schedule      TEXT NOT NULL DEFAULT 'daily',
    -- 'review' = all proposals land as reviewable yellows; 'gate' = normal verdicts
    confidence_policy TEXT NOT NULL DEFAULT 'review' CHECK (confidence_policy IN ('review', 'gate')),
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Run log (ticket 4.5): epoch cut provenance + budget accounting.
CREATE TABLE IF NOT EXISTS gardener_runs (
    id          TEXT PRIMARY KEY,
    gardener    TEXT NOT NULL REFERENCES gardeners (id),
    started_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    finished_at TEXT,
    status      TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'ok', 'failed', 'budget-killed')),
    summary     TEXT,
    tokens_used INTEGER,
    tool_calls  INTEGER
);

-- Tags (ticket 2.12): extracted from frontmatter blocks, per block like edges.
CREATE TABLE IF NOT EXISTS doc_tags (
    doc_id   TEXT NOT NULL REFERENCES docs (id),
    block_id TEXT NOT NULL REFERENCES blocks (id),
    tag      TEXT NOT NULL,
    PRIMARY KEY (block_id, tag)
);

CREATE INDEX IF NOT EXISTS doc_tags_by_tag ON doc_tags (tag);
CREATE INDEX IF NOT EXISTS doc_tags_by_doc ON doc_tags (doc_id);

-- Veracity sweep bookkeeping: which auditor covered which doc, when.
CREATE TABLE IF NOT EXISTS audits (
    doc_id     TEXT NOT NULL REFERENCES docs (id),
    principal  TEXT NOT NULL REFERENCES principals (id),
    audited_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (doc_id, principal)
);

-- Federation (ADR 0002): pair-once contacts, subtree shares, one-time invites,
-- grantee-side mirror cursors. Owner-authoritative; a remote peer is
-- gardener-shaped — its writes arrive through the propose gate.
CREATE TABLE IF NOT EXISTS contacts (
    id        TEXT PRIMARY KEY,
    -- iroh node id = ed25519 public key, hex — keys identify actors, UUIDs data
    pubkey    TEXT NOT NULL UNIQUE,
    petname   TEXT NOT NULL,
    principal TEXT NOT NULL REFERENCES principals (id),
    verified  INTEGER NOT NULL DEFAULT 0,
    revoked   INTEGER NOT NULL DEFAULT 0,
    paired_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS shares (
    id         TEXT PRIMARY KEY,
    root_doc   TEXT NOT NULL REFERENCES docs (id),
    -- NULL until an invite is redeemed and binds a contact
    contact    TEXT REFERENCES contacts (id),
    permission TEXT NOT NULL DEFAULT 'view' CHECK (permission IN ('view', 'propose')),
    -- offered = minted/awaiting grantee accept; active = syncing; revoked = refused on dial
    state      TEXT NOT NULL DEFAULT 'offered' CHECK (state IN ('offered', 'active', 'revoked')),
    -- overrides the doc's review policy for proposals arriving via this share
    policy_override TEXT CHECK (policy_override IN ('human-review', 'agent-review', 'auto')),
    -- trust tier (#62): review = remote proposals park red (default);
    -- yellow = they apply immediately as flagged yellows (reds still park);
    -- green = maintainer: clean edits land green, no review, owner notified
    trust      TEXT NOT NULL DEFAULT 'review' CHECK (trust IN ('review', 'yellow', 'green')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS shares_by_contact ON shares (contact);

-- One-time invite secrets (hash only, never the secret), burned on redeem.
CREATE TABLE IF NOT EXISTS share_invites (
    id          TEXT PRIMARY KEY,
    share_id    TEXT NOT NULL REFERENCES shares (id),
    secret_hash TEXT NOT NULL UNIQUE,
    expires_at  TEXT NOT NULL,
    redeemed_by TEXT REFERENCES contacts (id),
    redeemed_at TEXT,
    -- invites v2: the contact this invite was OFFERED to over the wire (no
    -- link); NULL for a minted link. The shares page reads "waiting for X".
    offered_to  TEXT REFERENCES contacts (id),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Invites v2: share OFFERS received from contacts (recipient side). Durable —
-- a request to join lives here until accepted/declined/expired, never only
-- in a toast. The secret is the invite secret; the recipient needs it to
-- redeem, so it is stored (this row IS the invite, from the other side).
CREATE TABLE IF NOT EXISTS share_offers (
    id           TEXT PRIMARY KEY,
    from_contact TEXT NOT NULL REFERENCES contacts (id),
    owner_node   TEXT NOT NULL,
    share_id     TEXT NOT NULL,
    root_title   TEXT NOT NULL,
    permission   TEXT NOT NULL CHECK (permission IN ('view', 'propose')),
    secret       TEXT NOT NULL,
    state        TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'accepted', 'declined', 'expired')),
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at   TEXT NOT NULL,
    UNIQUE (owner_node, share_id)
);

-- Grantee-side: a mirror doc's origin + pull cursor. Mirrors keep their origin
-- UUIDs (federation tax), so doc_id is the same id the owner holds. share_id
-- is the owner-side share id — a foreign instance's id, deliberately no FK.
CREATE TABLE IF NOT EXISTS mirrors (
    doc_id       TEXT PRIMARY KEY REFERENCES docs (id),
    owner        TEXT NOT NULL REFERENCES contacts (id),
    share_id     TEXT NOT NULL,
    synced_epoch INTEGER NOT NULL DEFAULT 0,
    -- what the owner granted us: drives the editor mode (read-only vs
    -- propose-upstream); the owner enforces regardless
    permission   TEXT NOT NULL DEFAULT 'view' CHECK (permission IN ('view', 'propose')),
    -- the owner tends this doc (a gardener over it or an ancestor). Shipped in
    -- the pull meta so the grantee can show "tended by owner" and refuse to
    -- tend it locally — one side's agents own a shared doc, never both.
    owner_tended INTEGER NOT NULL DEFAULT 0,
    -- sync health, per mirror doc: when the last pull that touched it
    -- succeeded, and the last pull error (cleared on success). The shares
    -- page shows these — a mirror that is "titles but no content" MUST read
    -- as a red row saying why, never as a silent doc.
    last_pulled_at TEXT,
    last_error     TEXT,
    -- the owner's epoch from the last pull meta; > synced_epoch = "behind"
    owner_epoch  INTEGER NOT NULL DEFAULT 0
);

-- Instance-level key/value settings (profile confirmation etc.). Tiny and
-- deliberately schemaless: anything bigger deserves its own table.
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Grantee-side: joins that could not complete because the owner was offline.
-- A background loop retries; success removes the row (async redeem, ADR 0002
-- decision 6). The ticket contains the secret, so this table is as sensitive
-- as the link itself — local-only, like everything here.
CREATE TABLE IF NOT EXISTS pending_joins (
    id         TEXT PRIMARY KEY,
    ticket     TEXT NOT NULL UNIQUE,
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Grantee-side: proposals shipped upstream through a propose share (#60).
-- op_ids are OWNER-side op ids (JSON array) — the handle for status checks.
-- Pessimistic mirror: the local doc never changes until the owner accepts
-- and the next pull lands it.
CREATE TABLE IF NOT EXISTS outbound_proposals (
    id         TEXT PRIMARY KEY,
    doc_id     TEXT NOT NULL REFERENCES docs (id),
    share_id   TEXT NOT NULL,
    owner      TEXT NOT NULL REFERENCES contacts (id),
    op_ids     TEXT NOT NULL,
    note       TEXT NOT NULL DEFAULT '',
    state      TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'accepted', 'declined', 'mixed')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Block embeddings (ask the vault, 2026-09-03): one static-model vector per
-- live content block, f32 little-endian BLOB. `epoch` is the block epoch the
-- vector was computed at; a newer block epoch = stale = re-embed that block.
CREATE TABLE IF NOT EXISTS block_vec (
    block_id TEXT PRIMARY KEY REFERENCES blocks (id),
    epoch    INTEGER NOT NULL,
    dim      INTEGER NOT NULL,
    vec      BLOB NOT NULL
);
