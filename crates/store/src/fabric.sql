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
    accounts_at     INTEGER,
    -- the last RuntimeStatus, as JSON (key, Hold, settings version, devices,
    -- presence): what the hub checks a device's order against
    status          TEXT NOT NULL DEFAULT '{}'
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
    -- the person follows it: every finished turn alerts. Theirs, like quiet_until
    following        INTEGER NOT NULL DEFAULT 0,
    -- ADR 0008: the named fabric's id, the latest turn, the queued order ids
    -- (JSON array) and why the queue is paused
    fabric_id        TEXT NOT NULL DEFAULT '',
    turn_id          TEXT NOT NULL DEFAULT '',
    queue            TEXT NOT NULL DEFAULT '[]',
    queue_paused     TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (runtime_id, id)
);
CREATE INDEX IF NOT EXISTS fabric_sessions_user ON fabric_sessions (user_id);
CREATE INDEX IF NOT EXISTS fabric_sessions_updated ON fabric_sessions (updated_at);

-- ---------------------------------------------------------------------
-- Slices 2-5 (ADR 0008): turns, attention items, orders, settings,
-- pairing, project switches, agent messages. Every row names its person
-- (user_id) and is read only in their Scope::User. Rows keyed by an id a
-- runtime or a device chose are keyed with the runtime too, so one person's
-- id can never collide with another's row.
-- ---------------------------------------------------------------------

-- One finished turn of a session (Up `turn`). 7 days.
CREATE TABLE IF NOT EXISTS fabric_turns (
    runtime_id       TEXT NOT NULL REFERENCES fabric_runtimes (id) ON DELETE CASCADE,
    session_id       TEXT NOT NULL,
    turn_id          TEXT NOT NULL,
    user_id          TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    address          TEXT NOT NULL DEFAULT '',
    project          TEXT NOT NULL DEFAULT '',
    -- empty under the project's Text off (text_off = 1)
    prompt           TEXT NOT NULL DEFAULT '',
    reply            TEXT NOT NULL DEFAULT '',
    text_off         INTEGER NOT NULL DEFAULT 0,
    tool_count       INTEGER NOT NULL DEFAULT 0,
    -- {files_changed, lines_added, lines_removed, commits, pull_request_url, pull_request_number}
    outcome          TEXT NOT NULL DEFAULT '{}',
    started          INTEGER NOT NULL,
    ended            INTEGER NOT NULL,
    truncated        INTEGER NOT NULL DEFAULT 0,
    context_used_pct INTEGER,
    -- clean | question | error | interrupted | denied | unknown
    end_kind         TEXT NOT NULL DEFAULT 'unknown',
    -- mac | order | answer | wake | unknown
    source           TEXT NOT NULL DEFAULT 'unknown',
    order_id         TEXT,
    created_at       INTEGER NOT NULL,
    PRIMARY KEY (runtime_id, session_id, turn_id)
);
CREATE INDEX IF NOT EXISTS fabric_turns_user_ended ON fabric_turns (user_id, ended);

-- An attention item: raised by a runtime (signed request) or by the hub
-- (a wake offer, a paused pair, an ask waiting). Open while cleared_at is
-- NULL. 7 days.
CREATE TABLE IF NOT EXISTS fabric_attention (
    runtime_id      TEXT NOT NULL REFERENCES fabric_runtimes (id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    user_id         TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    session_id      TEXT NOT NULL,
    address         TEXT NOT NULL DEFAULT '',
    project         TEXT NOT NULL DEFAULT '',
    -- runtime | hub
    source          TEXT NOT NULL,
    -- approval | question | dialog | tamper | unknown | wake_offer | pair_paused | ask_waiting
    kind            TEXT NOT NULL,
    text            TEXT NOT NULL DEFAULT '',
    -- JSON [{id, label}]
    choices         TEXT NOT NULL DEFAULT '[]',
    tool            TEXT NOT NULL DEFAULT '',
    guardrail_id    TEXT NOT NULL DEFAULT '',
    guardrail_label TEXT NOT NULL DEFAULT '',
    turn_id         TEXT NOT NULL DEFAULT '',
    raised_at       INTEGER NOT NULL,
    expires_at      INTEGER,
    -- runtime items: the AttentionRequest bytes and the runtime's signature,
    -- passed to the phone byte for byte. NULL under Text off once cleared.
    request         BLOB,
    signature       BLOB,
    -- hub items
    conversation_id TEXT,
    message_id      TEXT,
    from_address    TEXT,
    cleared_at      INTEGER,
    -- approved | denied | answered | at_mac | timed_out | withdrawn | dismissed
    resolution      TEXT,
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (runtime_id, id)
);
CREATE INDEX IF NOT EXISTS fabric_attention_user ON fabric_attention (user_id, cleared_at);

-- A signed order (POST /api/fabric/orders) and its audit trail. The signed
-- bytes are kept to be sent down, byte for byte. 7 days after its last change.
CREATE TABLE IF NOT EXISTS fabric_orders (
    runtime_id    TEXT NOT NULL REFERENCES fabric_runtimes (id) ON DELETE CASCADE,
    id            TEXT NOT NULL,
    user_id       TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    session_id    TEXT,
    device_id     TEXT NOT NULL,
    counter       INTEGER NOT NULL,
    -- command | hold
    kind          TEXT NOT NULL,
    verb          TEXT,
    hold          INTEGER,
    -- NULL once delivered under Text off
    text          TEXT,
    after_turn    TEXT NOT NULL DEFAULT '',
    queued        INTEGER NOT NULL DEFAULT 0,
    request_id    TEXT NOT NULL DEFAULT '',
    choice_id     TEXT NOT NULL DEFAULT '',
    -- JSON array of order ids (cancel, reorder)
    order_ids     TEXT NOT NULL DEFAULT '[]',
    issued_at     INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    -- the Signed envelope; NULL once the runtime has it (or it is dead) under Text off
    payload       BLOB,
    signature     BLOB,
    -- pending | sent | received | queued | done | refused | failed | expired | withdrawn
    state         TEXT NOT NULL,
    reason        TEXT,
    detail        TEXT,
    -- the stream it went down on last (instance) and its number there
    down_instance TEXT,
    down_seq      INTEGER,
    received_at   INTEGER,
    -- the session's project when it was taken (Text off)
    project       TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (runtime_id, id)
);
CREATE INDEX IF NOT EXISTS fabric_orders_user ON fabric_orders (user_id, created_at);
CREATE INDEX IF NOT EXISTS fabric_orders_device ON fabric_orders (runtime_id, device_id, counter);

-- The newest signed Settings, one per person. Kept until replaced.
CREATE TABLE IF NOT EXISTS fabric_settings (
    user_id    TEXT PRIMARY KEY REFERENCES auth_users (id) ON DELETE CASCADE,
    version    INTEGER NOT NULL,
    issued_at  INTEGER NOT NULL,
    payload    BLOB NOT NULL,
    device_id  TEXT NOT NULL,
    signature  BLOB NOT NULL,
    -- the decoded settings, as the viewer routes serve them
    json       TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- A person's switches per project. Kept until changed.
CREATE TABLE IF NOT EXISTS fabric_projects (
    user_id               TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    name                  TEXT NOT NULL,
    text_off              INTEGER NOT NULL DEFAULT 0,
    private_notifications INTEGER NOT NULL DEFAULT 0,
    updated_at            INTEGER NOT NULL,
    PRIMARY KEY (user_id, name)
);

-- A pairing offer on its way to a runtime. Live 10 minutes; the row a day.
CREATE TABLE IF NOT EXISTS fabric_pairings (
    runtime_id    TEXT NOT NULL REFERENCES fabric_runtimes (id) ON DELETE CASCADE,
    id            TEXT NOT NULL,
    user_id       TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    sealed        BLOB,
    -- pending | sent | paired | rejected | expired | invalid
    state         TEXT NOT NULL,
    device_id     TEXT,
    detail        TEXT,
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    down_instance TEXT,
    down_seq      INTEGER,
    received_at   INTEGER,
    PRIMARY KEY (runtime_id, id)
);
CREATE INDEX IF NOT EXISTS fabric_pairings_user ON fabric_pairings (user_id, id);

-- Two sessions that have messaged each other: what the pair limit counts.
-- (a_*) sorts before (b_*). 7 days after the last message.
CREATE TABLE IF NOT EXISTS fabric_conversations (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    a_runtime   TEXT NOT NULL,
    a_session   TEXT NOT NULL,
    a_address   TEXT NOT NULL,
    b_runtime   TEXT NOT NULL,
    b_session   TEXT NOT NULL,
    b_address   TEXT NOT NULL,
    -- messages since the person last ordered either session
    count       INTEGER NOT NULL DEFAULT 0,
    total       INTEGER NOT NULL DEFAULT 0,
    last_at     INTEGER NOT NULL,
    paused      INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL,
    UNIQUE (user_id, a_runtime, a_session, b_runtime, b_session)
);

-- One message to one receiver. The body goes when it is read, or after a
-- day unread (it bounces); the envelope stays 7 days.
CREATE TABLE IF NOT EXISTS fabric_messages (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    conversation_id TEXT,
    from_runtime    TEXT NOT NULL,
    from_session    TEXT NOT NULL,
    from_address    TEXT NOT NULL,
    to_runtime      TEXT NOT NULL,
    to_session      TEXT NOT NULL,
    to_address      TEXT NOT NULL,
    -- what the sender wrote in `to` (empty for a reply routed by id)
    to_written      TEXT NOT NULL DEFAULT '',
    -- note | ask | reply | bounce
    kind            TEXT NOT NULL,
    body            TEXT,
    doc             TEXT NOT NULL DEFAULT '',
    reply_to        TEXT NOT NULL DEFAULT '',
    sent_at         INTEGER NOT NULL,
    read_at         INTEGER,
    -- unread | read | held | bounced
    state           TEXT NOT NULL,
    bounce_reason   TEXT,
    answered        INTEGER NOT NULL DEFAULT 0,
    -- its nudge: the stream it went down on, and what the runtime said
    down_instance   TEXT,
    down_seq        INTEGER,
    received_at     INTEGER,
    nudge_outcome   TEXT,
    nudge_reason    TEXT,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS fabric_messages_to ON fabric_messages (user_id, to_runtime, to_session, state);
CREATE INDEX IF NOT EXISTS fabric_messages_conversation ON fabric_messages (conversation_id, sent_at);
