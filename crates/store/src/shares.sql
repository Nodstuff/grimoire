-- Share links (schema v10): a read-only SNAPSHOT of a doc, published at
-- /s/<token> for anyone holding the link. The public routes read only these
-- tables, never docs or blocks. Run after schema.sql; additive only.
-- NOT named `shares`: that was federation's table, and an open drops any
-- table by that name (sqlite.rs FEDERATION_TABLES).
CREATE TABLE IF NOT EXISTS share_links (
    id               TEXT PRIMARY KEY,
    -- the person who published it (auth_users id); only they see the row
    owner_id         TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    -- the doc it was taken from (no FK: a purged doc leaves the snapshot)
    doc_id           TEXT NOT NULL,
    -- SHA-256 (hex) of the link's token. The token itself is never stored:
    -- it is b64url(HMAC-SHA256(<db dir>/share-links.key, id)), recomputed
    -- for the owner's URL (the key file is not in the db or its replicas)
    token_hash       TEXT NOT NULL,
    title            TEXT NOT NULL,
    markdown         TEXT NOT NULL,
    theme            TEXT NOT NULL CHECK (theme IN ('light', 'dark', 'auto')),
    -- (the page is rendered from `markdown` when served, by the running
    -- renderer, and cached in memory: nothing rendered is stored)
    revision         INTEGER NOT NULL DEFAULT 1,
    comments_enabled INTEGER NOT NULL DEFAULT 1,
    -- unix seconds
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    snapshot_at      INTEGER NOT NULL,
    expires_at       INTEGER,
    revoked_at       INTEGER,
    views            INTEGER NOT NULL DEFAULT 0,
    last_viewed_at   INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS share_links_token_hash ON share_links (token_hash);
CREATE INDEX IF NOT EXISTS share_links_by_owner_doc ON share_links (owner_id, doc_id, created_at);

-- The snapshot's images (diagrams rendered to SVG by the app, pictures).
CREATE TABLE IF NOT EXISTS share_link_assets (
    share_id     TEXT NOT NULL REFERENCES share_links (id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    content_type TEXT NOT NULL,
    data         BLOB NOT NULL,
    width        INTEGER,
    height       INTEGER,
    PRIMARY KEY (share_id, name)
);

-- Comments from link readers (and the owner's replies).
CREATE TABLE IF NOT EXISTS share_link_comments (
    id         TEXT PRIMARY KEY,
    share_id   TEXT NOT NULL REFERENCES share_links (id) ON DELETE CASCADE,
    parent_id  TEXT,
    author     TEXT NOT NULL,
    is_owner   INTEGER NOT NULL DEFAULT 0,
    body       TEXT NOT NULL,
    -- {"block": n, "quote": "…"} or NULL
    anchor     TEXT,
    created_at INTEGER NOT NULL,
    -- the share's revision the comment was made on
    revision   INTEGER NOT NULL,
    -- SHA-256 of (share id, client ip) for the rate limit; never the ip
    ip_hash    TEXT,
    -- when the owner read it (GET /api/shares/{id}/comments)
    read_at    INTEGER
);
CREATE INDEX IF NOT EXISTS share_link_comments_by_share ON share_link_comments (share_id, created_at);
CREATE INDEX IF NOT EXISTS share_link_comments_by_ip ON share_link_comments (share_id, ip_hash, created_at);
