-- The EPUB library (schema v11): one shelf per person. `book` holds only
-- metadata — the file bytes and the cover image live in object storage
-- (file_s3_key, cover_s3_key), never in the db. `ereader` is a paired
-- reading device (KOReader &c.): HTTP basic (secret_hash) for the library
-- endpoints and a KOSYNC key (kosync_key_hash) for progress sync; hashes
-- only, the store never sees a secret. Times are unix seconds, passed in
-- by the caller. Run after shares.sql; additive only.
-- NOT named `shares`: that was federation's table, and an open drops any
-- table by that name (sqlite.rs FEDERATION_TABLES).
CREATE TABLE IF NOT EXISTS book (
    id                TEXT PRIMARY KEY,
    -- the person whose shelf this is (auth_users id); only they see the row
    user_id           TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    title             TEXT NOT NULL,
    author            TEXT NOT NULL,
    series            TEXT,
    series_index      REAL,
    -- the identifier the epub itself carries (ISBN, calibre uuid, …)
    book_identifier   TEXT,
    original_filename TEXT NOT NULL,
    -- SHA-256 (hex) of the file bytes: a re-upload of the same file dedups
    file_sha256       TEXT NOT NULL,
    -- file size in bytes
    size              INTEGER NOT NULL,
    file_s3_key       TEXT NOT NULL,
    cover_s3_key      TEXT,
    added_at          INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    -- soft delete: the row stays until the object-storage sweep catches up
    deleted_at        INTEGER
);
CREATE INDEX IF NOT EXISTS book_by_user_added ON book (user_id, added_at, id);
CREATE INDEX IF NOT EXISTS book_by_user_sha256 ON book (user_id, file_sha256);
CREATE INDEX IF NOT EXISTS book_by_user_identifier ON book (user_id, book_identifier);

-- A paired ereader. The basic-auth secret identifies the device on its own
-- (the username is a formality), so its hash is unique across all users;
-- the KOSYNC key resolves together with the person's name (one KOSYNC
-- account may back several devices). Revoking keeps the row (it stays
-- listed) but ends every resolution.
CREATE TABLE IF NOT EXISTS ereader (
    id               TEXT PRIMARY KEY,
    user_id          TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    label            TEXT NOT NULL,
    -- SHA-256 (hex) of the basic-auth password
    secret_hash      TEXT NOT NULL,
    -- SHA-256 (hex) of the KOSYNC api key: md5 of the password exactly as
    -- the person types it on the device. The primary hash is the form the
    -- UI shows (dashed, lowercase); the alt is the undashed form. '' never
    -- matches (a presented key always hashes to 64 hex chars).
    kosync_key_hash  TEXT NOT NULL,
    kosync_key_hash_alt TEXT NOT NULL DEFAULT '',
    -- 1: the daemon streams the file from object storage through itself;
    -- 0: it answers with a redirect to a presigned URL
    stream_downloads INTEGER NOT NULL DEFAULT 1,
    created_at       INTEGER NOT NULL,
    last_used_at     INTEGER,
    revoked_at       INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS ereader_secret_hash ON ereader (secret_hash);
CREATE INDEX IF NOT EXISTS ereader_by_user ON ereader (user_id);

-- KOSYNC reading progress: one row per (person, document), newest-wins —
-- an upsert whose stamp is not newer than the stored one leaves the row
-- untouched (books.rs kosync_upsert).
CREATE TABLE IF NOT EXISTS kosync_progress (
    user_id    TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    -- the document hash KOSYNC keys on (32 lowercase hex chars)
    document   TEXT NOT NULL,
    -- the position marker (an XPath-ish fragment), opaque here
    progress   TEXT NOT NULL,
    percentage REAL NOT NULL,
    device     TEXT NOT NULL,
    device_id  TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, document)
);
