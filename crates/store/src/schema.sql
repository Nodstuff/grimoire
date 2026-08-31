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
    kind          TEXT NOT NULL DEFAULT 'tagging' CHECK (kind IN ('tagging', 'reviewer', 'auditor')),
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
