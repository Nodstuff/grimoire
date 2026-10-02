-- knowledge-system substrate schema (PROJECT.md §3.1–3.2).
-- Ledger (ops) is the primary write record; blocks is the projection,
-- written in the same transaction and authoritative for reads.
-- All IDs are UUIDs stored as TEXT — never autoincrement (PROJECT.md §6).

-- 'remote' principals were federation peers (ADR 0002, superseded); the
-- kind stays so the provenance of their past ops still reads.
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
    -- per-document, never global (PROJECT.md §6); one epoch = one committed transaction
    current_epoch INTEGER NOT NULL DEFAULT 0,
    created_by    TEXT NOT NULL REFERENCES principals (id),
    -- manual tree ordering (fractional, like blocks); null sorts after keyed, by title
    sort_key      TEXT,
    deleted       INTEGER NOT NULL DEFAULT 0,
    -- when the tombstone was set; one value for every doc of a single delete,
    -- so restore can revive exactly that subtree (Trash)
    deleted_at    TEXT,
    -- doc freshness: when an auditor/keeper last evaluated this doc and found
    -- nothing, or a human accepted one of its fixes. Never set by ordinary
    -- edits; NULL = never verified.
    verified_at   TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- ADR 0004: the auth_users id whose Unsorted this tree is in when it is
    -- the root (children carry their root's owner too). NULL = the
    -- instance owner (a LOCAL database that never had users).
    owner_id      TEXT
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
-- comment threads: list_comments looks blocks up by the block they anchor to
CREATE INDEX IF NOT EXISTS blocks_by_refers_to ON blocks (refers_to);
-- tree walks (delete/restore subtree, next sibling key) go parent → children
CREATE INDEX IF NOT EXISTS docs_by_parent ON docs (parent_id);

-- Monotone counter bumped by trigger on every docs write (insert, any
-- update, delete). change_stamp's signal for the mutations its aggregates
-- cannot see: a same-length rename, a status or review-policy flip.
CREATE TABLE IF NOT EXISTS doc_revs (
    id  INTEGER PRIMARY KEY CHECK (id = 1),
    rev INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO doc_revs (id, rev) VALUES (1, 0);

CREATE TRIGGER IF NOT EXISTS docs_rev_ai AFTER INSERT ON docs BEGIN
    UPDATE doc_revs SET rev = rev + 1 WHERE id = 1;
END;
CREATE TRIGGER IF NOT EXISTS docs_rev_au AFTER UPDATE ON docs BEGIN
    UPDATE doc_revs SET rev = rev + 1 WHERE id = 1;
END;
CREATE TRIGGER IF NOT EXISTS docs_rev_ad AFTER DELETE ON docs BEGIN
    UPDATE doc_revs SET rev = rev + 1 WHERE id = 1;
END;

CREATE TABLE IF NOT EXISTS ops (
    id            TEXT PRIMARY KEY,
    doc_id        TEXT NOT NULL REFERENCES docs (id),
    -- block ops, plus doc ops (AX slice B: rename/move/status/delete of the
    -- doc itself, proposed by agents through the gate)
    op_type       TEXT NOT NULL CHECK (op_type IN ('insert', 'replace', 'delete', 'move',
                                                   'rename_doc', 'move_doc', 'set_status', 'delete_doc')),
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
-- proposal_outcomes: "what happened to MY ops" scans by principal
CREATE INDEX IF NOT EXISTS ops_by_principal ON ops (principal);

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
-- the whole-vault review queue filters on status alone (doc_id IS NULL)
CREATE INDEX IF NOT EXISTS annotations_by_status ON annotations (status);
-- op → its annotation: op_statuses, proposal_outcomes, resolve
CREATE INDEX IF NOT EXISTS annotations_by_op ON annotations (op_id);

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
        CHECK (kind IN ('tagging', 'reviewer', 'auditor', 'scribe', 'keeper', 'filer')),
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
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- ADR 0004: the user this gardener works for (it runs in their scope);
    -- NULL = the instance owner
    owner_id      TEXT
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

-- Living answers: the blocks an ask-the-vault answer cited, at the epoch
-- they were read. A cited block whose epoch moved on (or that is gone)
-- makes the answer stale; the refresher re-runs the question and lands new
-- receipts through the gate, then rewrites these rows.
CREATE TABLE IF NOT EXISTS answer_sources (
    answer_doc_id   TEXT NOT NULL REFERENCES docs (id),
    block_id        TEXT NOT NULL,
    epoch_at_answer INTEGER NOT NULL,
    recorded_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (answer_doc_id, block_id)
);

-- Veracity sweep bookkeeping: which auditor covered which doc, when.
CREATE TABLE IF NOT EXISTS audits (
    doc_id     TEXT NOT NULL REFERENCES docs (id),
    principal  TEXT NOT NULL REFERENCES principals (id),
    audited_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (doc_id, principal)
);

-- Instance-level key/value settings (profile confirmation etc.). Tiny and
-- deliberately schemaless: anything bigger deserves its own table.
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Block embeddings (ask the vault, 2026-09-03): one static-model vector per
-- live content block, f32 little-endian BLOB. `epoch` is the block epoch the
-- vector was computed at; a newer block epoch = stale = re-embed that block.
-- CASCADE: a hard block delete takes the vector with it (the FK would
-- otherwise fail once the block is embedded).
CREATE TABLE IF NOT EXISTS block_vec (
    block_id TEXT PRIMARY KEY REFERENCES blocks (id) ON DELETE CASCADE,
    epoch    INTEGER NOT NULL,
    dim      INTEGER NOT NULL,
    vec      BLOB NOT NULL
);

-- Change journal: the sync cursor for offline clients (GET /api/changes,
-- /api/changes/stream). One row per observable doc change, written by the
-- triggers below so every write path (UI, MCP, gardeners, import) lands
-- here without opting in. `seq` is the cursor — the one integer id in the
-- schema, deliberately: it is local to this database. Rows are not coalesced; a client refetches the doc either way.
CREATE TABLE IF NOT EXISTS changes (
    seq    INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id TEXT NOT NULL,
    -- doc: content/title/status/tags/review state; tree: created, moved,
    -- reordered; deleted: tombstoned (or purged); restored: out of the Trash
    kind   TEXT NOT NULL CHECK (kind IN ('doc', 'tree', 'deleted', 'restored')),
    epoch  INTEGER,
    at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- ADR 0004: NULL = a global row (filtered by the reader's visibility);
    -- set = a row for that user only: access gained ('tree') or revoked
    -- ('deleted', the client drops the doc)
    user_id TEXT
);

CREATE TRIGGER IF NOT EXISTS changes_docs_ai AFTER INSERT ON docs BEGIN
    INSERT INTO changes (doc_id, kind, epoch) VALUES (new.id, 'tree', new.current_epoch);
END;
-- content edits all bump current_epoch (one epoch = one transaction), so the
-- epoch column, not ops, is the content signal: one row per commit
CREATE TRIGGER IF NOT EXISTS changes_docs_au AFTER UPDATE ON docs BEGIN
    INSERT INTO changes (doc_id, kind, epoch)
        SELECT new.id, 'deleted', new.current_epoch WHERE old.deleted = 0 AND new.deleted <> 0;
    INSERT INTO changes (doc_id, kind, epoch)
        SELECT new.id, 'restored', new.current_epoch WHERE old.deleted <> 0 AND new.deleted = 0;
    INSERT INTO changes (doc_id, kind, epoch)
        SELECT new.id, 'tree', new.current_epoch
        WHERE old.deleted = 0 AND new.deleted = 0
          AND (old.parent_id IS NOT new.parent_id OR old.sort_key IS NOT new.sort_key);
    INSERT INTO changes (doc_id, kind, epoch)
        SELECT new.id, 'doc', new.current_epoch
        WHERE old.deleted = 0 AND new.deleted = 0
          AND (old.current_epoch <> new.current_epoch OR old.title <> new.title
               OR old.status IS NOT new.status);
END;
CREATE TRIGGER IF NOT EXISTS changes_docs_ad AFTER DELETE ON docs BEGIN
    INSERT INTO changes (doc_id, kind, epoch) VALUES (old.id, 'deleted', old.current_epoch);
END;
-- review state (a yellow flagged or accepted, a red parked or resolved)
-- changes what a client shows without moving the epoch
CREATE TRIGGER IF NOT EXISTS changes_annotations_ai AFTER INSERT ON annotations BEGIN
    INSERT INTO changes (doc_id, kind, epoch)
        SELECT id, 'doc', current_epoch FROM docs WHERE id = new.doc_id AND deleted = 0;
END;
CREATE TRIGGER IF NOT EXISTS changes_annotations_au AFTER UPDATE OF status ON annotations
WHEN old.status IS NOT new.status BEGIN
    INSERT INTO changes (doc_id, kind, epoch)
        SELECT id, 'doc', current_epoch FROM docs WHERE id = new.doc_id AND deleted = 0;
END;

-- Auth (server mode: OAuth 2.1 + passkeys). Every secret is stored as a
-- SHA-256 hex hash, never in the clear; every expiry is unix seconds and
-- enforced by the query that reads the row, not only by the cleanup sweep.
-- Users are rows, not a singleton: one owner today, more later.
CREATE TABLE IF NOT EXISTS auth_users (
    id           TEXT PRIMARY KEY,
    -- the principal this user's /api writes are attributed to
    principal_id TEXT NOT NULL REFERENCES principals (id),
    name         TEXT NOT NULL,
    role         TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    created_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS auth_credentials (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    -- the WebAuthn credential id (base64url), unique across all users
    cred_id      TEXT NOT NULL UNIQUE,
    -- webauthn-rs `Passkey`, serialised (public key + counter; no secret)
    passkey      TEXT NOT NULL,
    label        TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER
);

-- One-time owner enrollment links minted by `taisce auth enroll`.
CREATE TABLE IF NOT EXISTS auth_enrollments (
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);

-- OAuth clients: 'dcr' (RFC 7591, client_id minted here) or 'cimd' (the
-- client_id is an https URL; the fetched document is cached here).
CREATE TABLE IF NOT EXISTS oauth_clients (
    client_id     TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('dcr', 'cimd')),
    client_name   TEXT NOT NULL,
    redirect_uris TEXT NOT NULL,
    metadata      TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    -- cimd: when the cached document must be fetched again
    refresh_at    INTEGER
);

CREATE TABLE IF NOT EXISTS oauth_codes (
    code_hash      TEXT PRIMARY KEY,
    client_id      TEXT NOT NULL,
    user_id        TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    redirect_uri   TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    resource       TEXT,
    scope          TEXT NOT NULL,
    expires_at     INTEGER NOT NULL,
    used_at        INTEGER,
    -- the grant this code was exchanged for: a replayed code revokes it
    grant_id       TEXT
);

-- A grant is one authorization (one client, one user): the family every
-- refresh/access token descends from. Revoking it kills all of them.
CREATE TABLE IF NOT EXISTS oauth_grants (
    id         TEXT PRIMARY KEY,
    client_id  TEXT NOT NULL,
    user_id    TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    resource   TEXT,
    scope      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER,
    revoke_why TEXT
);

CREATE TABLE IF NOT EXISTS oauth_refresh_tokens (
    token_hash TEXT PRIMARY KEY,
    grant_id   TEXT NOT NULL REFERENCES oauth_grants (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    -- set when rotated: presenting it again is reuse (revokes the grant),
    -- unless within the grace window while its successor is still unused
    used_at    INTEGER,
    -- the pair this rotation issued (hashes), for that grace check
    next_refresh TEXT,
    next_access  TEXT
);
CREATE INDEX IF NOT EXISTS oauth_refresh_grant ON oauth_refresh_tokens (grant_id);

CREATE TABLE IF NOT EXISTS oauth_access_tokens (
    token_hash TEXT PRIMARY KEY,
    grant_id   TEXT NOT NULL REFERENCES oauth_grants (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS oauth_access_grant ON oauth_access_tokens (grant_id);

-- Personal access tokens (`taisce auth token create`): one static bearer per
-- name, for MCP clients that cannot run the OAuth flow (one token shared by
-- every Claude Code account). Accepted on /mcp only; the user is the token's
-- identity exactly as an OAuth grant's. A name is unique among a user's live
-- tokens, so a revoked name can be minted again.
CREATE TABLE IF NOT EXISTS auth_api_tokens (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    created_at   INTEGER NOT NULL,
    -- touched at most once a minute (`auth_touch_api_token`)
    last_used_at INTEGER,
    revoked_at   INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS auth_api_tokens_name ON auth_api_tokens (user_id, name) WHERE revoked_at IS NULL;

-- Idempotency: (principal, key) → the first outcome of a write, so a retry
-- (HTTP `request_id`, or an MCP write's content key) replays it instead of
-- applying twice — across restarts. Readers pass their own window; rows
-- older than the longest one (7 days) are swept.
CREATE TABLE IF NOT EXISTS idempotency (
    principal  TEXT NOT NULL,
    key        TEXT NOT NULL,
    response   TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (principal, key)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idempotency_created ON idempotency (created_at);

-- Push devices (APNs): a device token registered by a signed-in user's app.
-- Tokens are not secrets but are scoped to a user: a token re-registered by
-- another user moves to them. A token APNs reports gone (410, BadDeviceToken,
-- Unregistered) is disabled, never retried, until the app registers it again.
CREATE TABLE IF NOT EXISTS push_devices (
    token        TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    platform     TEXT NOT NULL CHECK (platform IN ('ios')),
    env          TEXT NOT NULL CHECK (env IN ('sandbox', 'production')),
    app_version  TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    last_sent_at INTEGER,
    last_error   TEXT,
    disabled_at  INTEGER
);
CREATE INDEX IF NOT EXISTS push_devices_user ON push_devices (user_id);

-- ADR 0004: membership changes, user creation and refused cross-tenant
-- writes. (`audits` above is the auditor gardener's per-doc coverage, keyed
-- by doc; these events are not about one doc.)
CREATE TABLE IF NOT EXISTS audit_events (
    id      TEXT PRIMARY KEY,
    at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- the auth_users id that acted (NULL = the box's CLI / System)
    actor   TEXT,
    event   TEXT NOT NULL,
    subject TEXT NOT NULL DEFAULT '',
    detail  TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS audit_events_at ON audit_events (at);
