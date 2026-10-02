-- Workspaces: a LABEL on a doc, inherited by its subtree. A doc's workspace
-- is its nearest labelled ancestor's (itself included); no label anywhere up
-- the chain = "Unsorted". Resolution is a query, never stored, so a move
-- re-resolves for free. Run after schema.sql; additive only.
CREATE TABLE IF NOT EXISTS workspaces (
    id         TEXT PRIMARY KEY,
    -- ADR 0004: the owning auth_users id; NULL = the instance owner (a LOCAL
    -- database that never had users)
    owner_id   TEXT,
    -- unique per owner, case-insensitively (workspaces_owner_name)
    name       TEXT NOT NULL COLLATE NOCASE,
    color      TEXT,
    icon       TEXT,
    sort_key   TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS workspaces_owner_name ON workspaces (COALESCE(owner_id, ''), name COLLATE NOCASE);

-- The explicit labels. No FK to docs: a purged doc leaves a row resolution
-- never reaches (it joins through docs). Deleting a workspace un-labels.
CREATE TABLE IF NOT EXISTS doc_workspace (
    doc_id       TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS doc_workspace_by_ws ON doc_workspace (workspace_id);

-- ADR 0004: who may see / write a workspace. The owner is always a member
-- with role 'owner'; more than one member = shared. Membership is a human
-- surface only (box CLI, the owner's app), never MCP.
CREATE TABLE IF NOT EXISTS workspace_members (
    workspace_id TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    user_id      TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    role         TEXT NOT NULL CHECK (role IN ('owner', 'editor', 'viewer')),
    added_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    added_by     TEXT,
    PRIMARY KEY (workspace_id, user_id)
);
CREATE INDEX IF NOT EXISTS workspace_members_by_user ON workspace_members (user_id);
