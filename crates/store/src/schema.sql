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
    -- per-document, never global (federation tax §6); one epoch = one committed transaction
    current_epoch INTEGER NOT NULL DEFAULT 0,
    created_by    TEXT NOT NULL REFERENCES principals (id),
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
    deleted    INTEGER NOT NULL DEFAULT 0
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
