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
    -- colour covers (schema v14; sqlite.rs adds these to older tables):
    -- the embedded cover's verdict, NULL = not looked at yet (a pre-v14
    -- book, or one without a cover), 1 = greyscale, 0 = colour
    cover_grey        INTEGER,
    -- the ISBNs among the OPF's identifiers, comma-separated; NULL = never
    -- read (pre-v14), '' = none
    isbns             TEXT,
    -- a colour cover found for the book, stored beside it (never inside
    -- the EPUB): what every cover route serves while cover_choice='color'
    color_cover_s3_key TEXT,
    -- 'none' (nobody chose), 'embedded' (keep the book's own) or 'color';
    -- an explicit choice is never overwritten by a library pass
    cover_choice      TEXT NOT NULL DEFAULT 'none',
    -- the human principal who chose, and when
    cover_choice_by   TEXT,
    cover_choice_at   INTEGER,
    -- when Open Library was last asked for this book (the candidates cache)
    color_lookup_at   INTEGER,
    added_at          INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    -- soft delete: the row stays (its keys name the objects); the daemon
    -- deletes the objects at once, leaving S3 noncurrent versions that the
    -- bucket lifecycle expires after 30 days
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

-- A book upload token (schema v12): a bearer secret (`tbu_…`) an
-- integration (a Readarr/Chaptarr custom script, a cron job) sends to
-- POST /api/books, and nowhere else. Minted by the person in Settings,
-- shown once, stored as its SHA-256 only; unique across all users, since
-- the hash alone names the user. Revoking keeps the row (it stays listed)
-- but ends every resolution.
CREATE TABLE IF NOT EXISTS upload_token (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    label        TEXT NOT NULL,
    -- SHA-256 (hex) of the whole token, `tbu_` prefix included
    secret_hash  TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    revoked_at   INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS upload_token_secret_hash ON upload_token (secret_hash);
CREATE INDEX IF NOT EXISTS upload_token_by_user ON upload_token (user_id);

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

-- Diagram images for docs sent to e-readers (schema v13, epub.rs): the web
-- app rasterizes a doc's mermaid/d2/reladraw/vega fences to PNG (the
-- server cannot draw them) and uploads them here, keyed by the block and a
-- hash of the fence's source, so an edited diagram's old picture is never
-- used (the EPUB falls back to the source). One copy per uploader
-- (`owner_key` = the auth_users id, '' for LOCAL mode): a render for one
-- person only ever uses pictures that person uploaded.
CREATE TABLE IF NOT EXISTS epub_image (
    owner_key   TEXT NOT NULL,
    doc_id      TEXT NOT NULL,
    block_id    TEXT NOT NULL,
    -- SHA-256 (hex) of `lang + "\n" + normalized source` (epub.rs)
    source_hash TEXT NOT NULL,
    png         BLOB NOT NULL,
    width       INTEGER NOT NULL,
    height      INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (owner_key, block_id, source_hash)
);
CREATE INDEX IF NOT EXISTS epub_image_by_doc ON epub_image (owner_key, doc_id);
CREATE TRIGGER IF NOT EXISTS epub_image_doc_purged AFTER DELETE ON docs BEGIN
    DELETE FROM epub_image WHERE doc_id = OLD.id;
END;

-- Colour-cover candidates (schema v14, library covers.rs): images the
-- server fetched from Open Library for one book and validated (decodes,
-- tall enough, actually colourful). A cache, replaced on every lookup and
-- dropped when the book's cover is chosen or the book is deleted; small
-- (tens of KB each, at most a handful per book), so the bytes live here
-- rather than in the bucket. Clients reference a candidate by this id,
-- never by a URL: the server alone decides what it fetches.
CREATE TABLE IF NOT EXISTS book_cover_candidate (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES auth_users (id) ON DELETE CASCADE,
    book_id      TEXT NOT NULL REFERENCES book (id) ON DELETE CASCADE,
    -- 'isbn' (that edition's cover) or 'search' (title and author)
    source       TEXT NOT NULL,
    -- the ISBN or Open Library cover id it came from
    source_ref   TEXT NOT NULL,
    -- best first
    rank         INTEGER NOT NULL,
    image        BLOB NOT NULL,
    ext          TEXT NOT NULL,
    width        INTEGER NOT NULL,
    height       INTEGER NOT NULL,
    colorfulness REAL NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS book_cover_candidate_by_book ON book_cover_candidate (user_id, book_id, rank);
