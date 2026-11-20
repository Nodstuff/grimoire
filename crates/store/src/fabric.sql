-- Agent Fabric (schema v16, ADR 0007): a person's runtimes (programs that
-- host Claude sessions on a machine and report them over the runtime
-- channel) and the sessions those runtimes last reported. A fleet belongs to
-- one person and is never shared: workspace membership does not reach it.
-- Times are unix seconds, passed in by the caller. Run after books.sql;
-- additive only.
-- NOT named `shares`: an open drops any table by that name (sqlite.rs
-- FEDERATION_TABLES).

-- A runtime is a setting: it stays until its owner removes it. Only the
-- SHA-256 of its credential (`trt_…`) is stored; the credential opens the
-- runtime channel and nothing else.
CREATE TABLE IF NOT EXISTS fabric_runtimes (
    id              TEXT PRIMARY KEY,
    -- the person whose machine this is (auth_users id); only they see the row
    user_id         TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    credential_hash TEXT NOT NULL UNIQUE,
    created_at      INTEGER NOT NULL,
    -- removed: the credential stops resolving and the sessions are deleted;
    -- the row stays so `taisce fabric runtime list` can say when
    revoked_at      INTEGER,
    -- when its stream was last up, as the hub saw it
    last_seen_at    INTEGER,
    -- the launch that last connected, and the last event the hub took from
    -- it (what a reconnecting runtime is told to replay after)
    instance_id     TEXT,
    last_up_seq     INTEGER NOT NULL DEFAULT 0,
    -- the runtime's own version, as its Hello said
    version         TEXT NOT NULL DEFAULT '',
    -- the last headroom report: a JSON array, one object per Claude account
    accounts        TEXT NOT NULL DEFAULT '[]',
    accounts_at     INTEGER
);
-- a name is unique among a person's live runtimes, so a removed name can be
-- enrolled again
CREATE UNIQUE INDEX IF NOT EXISTS fabric_runtimes_name ON fabric_runtimes (user_id, name) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS fabric_runtimes_user ON fabric_runtimes (user_id);

-- A session as its runtime last reported it. The id is the runtime's own
-- (a terminal pane's UUID), so the key is (runtime, id): one runtime can
-- never write a row another runtime holds. Rows are history, not settings:
-- they go 7 days after their last update (`fabric_sessions_expire`).
CREATE TABLE IF NOT EXISTS fabric_sessions (
    runtime_id       TEXT NOT NULL REFERENCES fabric_runtimes (id) ON DELETE CASCADE,
    id               TEXT NOT NULL,
    -- the runtime's owner, copied from its row at write time
    user_id          TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    address          TEXT NOT NULL,
    fabric           TEXT NOT NULL DEFAULT '',
    project          TEXT NOT NULL DEFAULT '',
    worktree         TEXT NOT NULL DEFAULT '',
    branch           TEXT NOT NULL DEFAULT '',
    account          TEXT NOT NULL DEFAULT '',
    -- working | needs_you | finished | stalled | waiting_on_peer | idle |
    -- asleep | exited
    state            TEXT NOT NULL,
    tool             TEXT NOT NULL DEFAULT '',
    note             TEXT NOT NULL DEFAULT '',
    last_line        TEXT NOT NULL DEFAULT '',
    -- 0–100; NULL = unknown
    context_used_pct INTEGER,
    -- when the session entered `state`, by the runtime's clock
    since            INTEGER NOT NULL,
    -- when the hub last heard this session exists
    updated_at       INTEGER NOT NULL,
    -- snoozed by its person until this time: the hub sends no alert for it
    -- before then. NULL = not snoozed. The person's, not the runtime's: a
    -- runtime's report never writes it (`fabric_session_quiet` does)
    quiet_until      INTEGER,
    PRIMARY KEY (runtime_id, id)
);
CREATE INDEX IF NOT EXISTS fabric_sessions_user ON fabric_sessions (user_id);
CREATE INDEX IF NOT EXISTS fabric_sessions_updated ON fabric_sessions (updated_at);
