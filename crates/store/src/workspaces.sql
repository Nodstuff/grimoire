-- Workspaces: a LABEL on a doc, inherited by its subtree. A doc's workspace
-- is its nearest labelled ancestor's (itself included); no label anywhere up
-- the chain = "Unsorted". Resolution is a query, never stored, so a move
-- re-resolves for free. Run after schema.sql; additive only.
CREATE TABLE IF NOT EXISTS workspaces (
    id         TEXT PRIMARY KEY,
    -- unique case-insensitively: MCP names a workspace by name
    name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    color      TEXT,
    icon       TEXT,
    sort_key   TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- The explicit labels. No FK to docs: a purged doc leaves a row resolution
-- never reaches (it joins through docs). Deleting a workspace un-labels.
CREATE TABLE IF NOT EXISTS doc_workspace (
    doc_id       TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS doc_workspace_by_ws ON doc_workspace (workspace_id);
