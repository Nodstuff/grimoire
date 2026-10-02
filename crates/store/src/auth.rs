//! Auth storage for server mode: users, passkeys, OAuth clients, codes,
//! grants and tokens (schema.sql, "Auth"). The store never sees a secret:
//! callers pass SHA-256 hashes and serialised passkeys, and every read that
//! could hand out access checks expiry and revocation in its own query.
//! Times are unix seconds, passed in by the caller (tests drive the clock).

use crate::sqlite::SqliteStore;
use crate::{Result, StoreError};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub name: String,
    /// `owner` or `member`
    pub role: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCredential {
    pub id: Uuid,
    pub user_id: Uuid,
    /// WebAuthn credential id, base64url
    pub cred_id: String,
    /// serialised webauthn-rs `Passkey`
    pub passkey: String,
    pub label: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthClient {
    pub client_id: String,
    /// `dcr` or `cimd`
    pub kind: String,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    /// the registration request / fetched document, as JSON
    pub metadata: String,
    pub created_at: i64,
    pub refresh_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCode {
    pub code_hash: String,
    pub client_id: String,
    pub user_id: Uuid,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub resource: Option<String>,
    pub scope: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: Uuid,
    pub client_id: String,
    pub user_id: Uuid,
    pub resource: Option<String>,
    pub scope: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
    pub revoke_why: Option<String>,
}

/// A personal access token (the secret itself is never stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

/// Seconds between `last_used_at` writes for one token: a busy MCP client
/// costs one UPDATE a minute, not one per request.
pub const API_TOKEN_TOUCH_EVERY: i64 = 60;

/// A web UI session (the cookie's value is never stored, only its hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub created_at: i64,
    pub last_used_at: i64,
    /// the absolute end (sign-in + `WEB_SESSION_ABSOLUTE`)
    pub expires_at: i64,
    /// coarse device summary, e.g. "Mac · Safari"
    pub user_agent: String,
    pub revoked_at: Option<i64>,
}

/// A web session ends after 14 idle days…
pub const WEB_SESSION_IDLE: i64 = 14 * 86400;
/// …or 90 days after sign-in, whichever comes first.
pub const WEB_SESSION_ABSOLUTE: i64 = 90 * 86400;
/// Seconds between `last_used_at` writes for one session.
pub const WEB_SESSION_TOUCH_EVERY: i64 = 60;

/// What presenting an authorization code did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeOutcome {
    /// First use, unexpired: the code is now spent.
    Fresh(AuthCode),
    /// Already exchanged once: the grant it produced (if any) is revoked.
    Replayed { grant_id: Option<Uuid> },
    /// Unknown or expired.
    Invalid,
}

/// Seconds after a rotation during which re-presenting the old refresh
/// token re-issues (rather than revokes) — if its successor is still unused.
pub const REFRESH_GRACE: i64 = 60;

/// What presenting a refresh token did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Live and unused: it is now rotated out; issue its successor.
    Rotated(Grant),
    /// A retry inside the grace window with the successor still unused:
    /// the successor pair is replaced by the new one.
    Reissued(Grant),
    /// Already rotated: the whole grant family is now revoked.
    Reused { grant_id: Uuid },
    /// Unknown, expired, or its grant is revoked.
    Invalid,
}

fn uuid_of(s: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&s).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

fn user_row(r: &rusqlite::Row) -> rusqlite::Result<AuthUser> {
    Ok(AuthUser {
        id: uuid_of(r.get(0)?)?,
        principal_id: uuid_of(r.get(1)?)?,
        name: r.get(2)?,
        role: r.get(3)?,
        created_at: r.get(4)?,
    })
}

fn cred_row(r: &rusqlite::Row) -> rusqlite::Result<AuthCredential> {
    Ok(AuthCredential {
        id: uuid_of(r.get(0)?)?,
        user_id: uuid_of(r.get(1)?)?,
        cred_id: r.get(2)?,
        passkey: r.get(3)?,
        label: r.get(4)?,
        created_at: r.get(5)?,
        last_used_at: r.get(6)?,
    })
}

fn grant_row(r: &rusqlite::Row) -> rusqlite::Result<Grant> {
    Ok(Grant {
        id: uuid_of(r.get(0)?)?,
        client_id: r.get(1)?,
        user_id: uuid_of(r.get(2)?)?,
        resource: r.get(3)?,
        scope: r.get(4)?,
        created_at: r.get(5)?,
        revoked_at: r.get(6)?,
        revoke_why: r.get(7)?,
    })
}

fn api_token_row(r: &rusqlite::Row) -> rusqlite::Result<ApiToken> {
    Ok(ApiToken {
        id: uuid_of(r.get(0)?)?,
        user_id: uuid_of(r.get(1)?)?,
        name: r.get(2)?,
        created_at: r.get(3)?,
        last_used_at: r.get(4)?,
        revoked_at: r.get(5)?,
    })
}

fn web_session_row(r: &rusqlite::Row) -> rusqlite::Result<WebSession> {
    Ok(WebSession {
        id: uuid_of(r.get(0)?)?,
        user_id: uuid_of(r.get(1)?)?,
        created_at: r.get(2)?,
        last_used_at: r.get(3)?,
        expires_at: r.get(4)?,
        user_agent: r.get(5)?,
        revoked_at: r.get(6)?,
    })
}

const WEB_SESSION_COLS: &str = "id, user_id, created_at, last_used_at, expires_at, user_agent, revoked_at";
const USER_COLS: &str = "id, principal_id, name, role, created_at";
const CRED_COLS: &str = "id, user_id, cred_id, passkey, label, created_at, last_used_at";
const API_TOKEN_COLS: &str = "id, user_id, name, created_at, last_used_at, revoked_at";
const GRANT_COLS: &str = "g.id, g.client_id, g.user_id, g.resource, g.scope, g.created_at, g.revoked_at, g.revoke_why";

impl SqliteStore {
    // ---- users ----

    /// The owner, created on first call and attributed to `principal_id`
    /// (the instance's human). There is one owner; members come later.
    pub fn auth_ensure_owner(&mut self, principal_id: Uuid, name: &str, now: i64) -> Result<AuthUser> {
        if let Some(u) = self.auth_owner()? {
            return Ok(u);
        }
        let id = Uuid::now_v7();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO auth_users (id, principal_id, name, role, created_at) VALUES (?1, ?2, ?3, 'owner', ?4)",
            params![id.to_string(), principal_id.to_string(), name, now],
        )?;
        // ADR 0004: a database that served LOCAL first becomes the owner's
        crate::tenancy::adopt_unowned_conn(&tx, id)?;
        crate::tenancy::audit_conn(&tx, None, "user.create", &id.to_string(), &serde_json::json!({"name": name, "role": "owner"}))?;
        tx.commit()?;
        self.auth_user(id)?.ok_or_else(|| StoreError::NotFound(format!("auth user {id}")))
    }

    /// Add another person (ADR 0004): a `member` user with their own human
    /// principal. System scope only (the box's CLI). Names are unique
    /// case-insensitively so `--user <name>` stays unambiguous.
    pub fn auth_add_user(&mut self, name: &str, now: i64) -> Result<AuthUser> {
        if self.scope() != crate::Scope::System {
            return Err(StoreError::Forbidden("users are added on the box's CLI".into()));
        }
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(StoreError::InvalidOp("a user's name must be 1..64 characters".into()));
        }
        crate::tenancy::check_person_name(name)?;
        if crate::tenancy::name_taken_conn(&self.conn, name, None)? {
            return Err(StoreError::InvalidOp(format!("the name {name:?} is taken (a person or an agent has it)")));
        }
        if self.auth_owner()?.is_none() {
            return Err(StoreError::InvalidOp("no owner yet: serve once in server mode (or `auth enroll`) first".into()));
        }
        let principal = Uuid::now_v7();
        let id = Uuid::now_v7();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO principals (id, kind, display_name, name_key) VALUES (?1, 'human', ?2, ?3)",
            params![principal.to_string(), name, crate::tenancy::name_key(name)],
        )
        .map_err(crate::tenancy::name_conflict)?;
        tx.execute(
            "INSERT INTO auth_users (id, principal_id, name, role, created_at) VALUES (?1, ?2, ?3, 'member', ?4)",
            params![id.to_string(), principal.to_string(), name, now],
        )?;
        crate::tenancy::audit_conn(&tx, None, "user.create", &id.to_string(), &serde_json::json!({"name": name, "role": "member"}))?;
        tx.commit()?;
        self.auth_user(id)?.ok_or_else(|| StoreError::NotFound(format!("auth user {id}")))
    }

    /// A user by id, unique id prefix, or (case-insensitive) name.
    /// A user by id, unique id prefix, or (case-insensitive) name — only
    /// when exactly one user matches.
    pub fn auth_find_user(&self, key: &str) -> Result<Option<AuthUser>> {
        let mut hits = self.auth_users_matching(key)?;
        Ok(if hits.len() == 1 { hits.pop() } else { None })
    }

    /// Every user a key could mean: by name (case-insensitive), else by id
    /// prefix. The CLI prints them when there is not exactly one.
    pub fn auth_users_matching(&self, key: &str) -> Result<Vec<AuthUser>> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(Vec::new());
        }
        let users = self.auth_users()?;
        let by_name: Vec<AuthUser> = users.iter().filter(|u| u.name.eq_ignore_ascii_case(key)).cloned().collect();
        if !by_name.is_empty() {
            return Ok(by_name);
        }
        Ok(users.into_iter().filter(|u| u.id.to_string().starts_with(&key.to_lowercase())).collect())
    }

    pub fn auth_owner(&self) -> Result<Option<AuthUser>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {USER_COLS} FROM auth_users WHERE role = 'owner' ORDER BY created_at LIMIT 1"),
                [],
                user_row,
            )
            .optional()?)
    }

    pub fn auth_user(&self, id: Uuid) -> Result<Option<AuthUser>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {USER_COLS} FROM auth_users WHERE id = ?1"), [id.to_string()], user_row)
            .optional()?)
    }

    pub fn auth_users(&self) -> Result<Vec<AuthUser>> {
        let mut st = self.conn.prepare(&format!("SELECT {USER_COLS} FROM auth_users ORDER BY created_at"))?;
        let rows = st.query_map([], user_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    // ---- enrollment links ----

    pub fn auth_add_enrollment(&mut self, token_hash: &str, user_id: Uuid, expires_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO auth_enrollments (token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
            params![token_hash, user_id.to_string(), expires_at],
        )?;
        Ok(())
    }

    /// The user an unused, unexpired enrollment link is for.
    pub fn auth_peek_enrollment(&self, token_hash: &str, now: i64) -> Result<Option<Uuid>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT user_id FROM auth_enrollments WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
                params![token_hash, now],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.map(uuid_of).transpose()?)
    }

    /// Spend an enrollment link and store the passkey it registered, in one
    /// transaction: a link registers exactly one credential.
    pub fn auth_enroll_credential(&mut self, token_hash: &str, cred: &AuthCredential, now: i64) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let spent = tx.execute(
            "UPDATE auth_enrollments SET used_at = ?2
             WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2 AND user_id = ?3",
            params![token_hash, now, cred.user_id.to_string()],
        )?;
        if spent != 1 {
            return Ok(false);
        }
        insert_credential(&tx, cred)?;
        tx.commit()?;
        Ok(true)
    }

    // ---- passkeys ----

    pub fn auth_add_credential(&mut self, cred: &AuthCredential) -> Result<()> {
        insert_credential(&self.conn, cred)
    }

    /// Every credential, or one user's.
    pub fn auth_credentials(&self, user: Option<Uuid>) -> Result<Vec<AuthCredential>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {CRED_COLS} FROM auth_credentials WHERE ?1 IS NULL OR user_id = ?1 ORDER BY created_at"
        ))?;
        let rows = st
            .query_map([user.map(|u| u.to_string())], cred_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn auth_credential_by_cred_id(&self, cred_id: &str) -> Result<Option<AuthCredential>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {CRED_COLS} FROM auth_credentials WHERE cred_id = ?1"), [cred_id], cred_row)
            .optional()?)
    }

    /// After a successful assertion: the updated passkey (counter) and use time.
    pub fn auth_touch_credential(&mut self, cred_id: &str, passkey: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE auth_credentials SET passkey = ?2, last_used_at = ?3 WHERE cred_id = ?1",
            params![cred_id, passkey, now],
        )?;
        Ok(())
    }

    /// Delete a credential by its row id (or a unique prefix of it).
    pub fn auth_delete_credential(&mut self, id: &str) -> Result<usize> {
        let id = id.trim();
        let ids: Vec<String> = {
            let mut st = self.conn.prepare("SELECT id FROM auth_credentials WHERE id LIKE ?1 || '%'")?;
            st.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        if ids.len() != 1 || id.is_empty() {
            return Ok(0);
        }
        Ok(self.conn.execute("DELETE FROM auth_credentials WHERE id = ?1", [&ids[0]])?)
    }

    // ---- personal access tokens ----

    /// A new token for `user_id` under `name` (unique among the user's live
    /// tokens: a clash is `InvalidOp`).
    pub fn auth_create_api_token(&mut self, user_id: Uuid, name: &str, token_hash: &str, now: i64) -> Result<ApiToken> {
        let id = Uuid::now_v7();
        let r = self.conn.execute(
            "INSERT INTO auth_api_tokens (id, user_id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id.to_string(), user_id.to_string(), name, token_hash, now],
        );
        match r {
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                return Err(StoreError::InvalidOp(format!("a live token named {name:?} already exists")));
            }
            r => r?,
        };
        Ok(ApiToken { id, user_id, name: name.to_string(), created_at: now, last_used_at: None, revoked_at: None })
    }

    /// The live (unrevoked) token behind a hash, its user still present.
    pub fn auth_api_token_by_hash(&self, token_hash: &str) -> Result<Option<ApiToken>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {API_TOKEN_COLS} FROM auth_api_tokens WHERE token_hash = ?1 AND revoked_at IS NULL"),
                [token_hash],
                api_token_row,
            )
            .optional()?)
    }

    /// Record a use, unless one was recorded in the last minute. True when
    /// it wrote (callers can skip the call when `last_used_at` is fresh).
    pub fn auth_touch_api_token(&mut self, id: Uuid, now: i64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE auth_api_tokens SET last_used_at = ?2
             WHERE id = ?1 AND (last_used_at IS NULL OR last_used_at <= ?2 - ?3)",
            params![id.to_string(), now, API_TOKEN_TOUCH_EVERY],
        )?;
        Ok(n == 1)
    }

    /// Every token, revoked ones included, oldest first.
    pub fn auth_api_tokens(&self) -> Result<Vec<ApiToken>> {
        let mut st = self.conn.prepare(&format!("SELECT {API_TOKEN_COLS} FROM auth_api_tokens ORDER BY created_at, id"))?;
        let rows = st.query_map([], api_token_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Revoke a live token by name, id, or a unique id prefix. None when
    /// nothing (or more than one token) matches.
    pub fn auth_revoke_api_token(&mut self, key: &str, now: i64) -> Result<Option<ApiToken>> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(None);
        }
        let live: Vec<ApiToken> = self.auth_api_tokens()?.into_iter().filter(|t| t.revoked_at.is_none()).collect();
        let by_name: Vec<&ApiToken> = live.iter().filter(|t| t.name == key).collect();
        let hits: Vec<&ApiToken> = if by_name.is_empty() {
            live.iter().filter(|t| t.id.to_string().starts_with(key)).collect()
        } else {
            by_name
        };
        let [t] = hits.as_slice() else {
            return Ok(None);
        };
        self.conn.execute(
            "UPDATE auth_api_tokens SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![t.id.to_string(), now],
        )?;
        Ok(Some(ApiToken { revoked_at: Some(now), ..(*t).clone() }))
    }

    // ---- web sessions ----

    /// A new web session for `user_id` (signed in now), ending at the latest
    /// `WEB_SESSION_ABSOLUTE` from now.
    pub fn auth_create_web_session(&mut self, user_id: Uuid, token_hash: &str, user_agent: &str, now: i64) -> Result<WebSession> {
        let s = WebSession {
            id: Uuid::now_v7(),
            user_id,
            created_at: now,
            last_used_at: now,
            expires_at: now + WEB_SESSION_ABSOLUTE,
            user_agent: user_agent.chars().take(80).collect(),
            revoked_at: None,
        };
        self.conn.execute(
            "INSERT INTO auth_web_sessions (id, user_id, token_hash, created_at, last_used_at, expires_at, user_agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![s.id.to_string(), user_id.to_string(), token_hash, now, now, s.expires_at, s.user_agent],
        )?;
        Ok(s)
    }

    /// The live session behind a cookie's hash: unrevoked, inside its
    /// absolute lifetime, used within the idle window, its user present.
    pub fn auth_web_session_by_hash(&self, token_hash: &str, now: i64) -> Result<Option<WebSession>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {WEB_SESSION_COLS} FROM auth_web_sessions
                     WHERE token_hash = ?1 AND revoked_at IS NULL AND expires_at > ?2 AND last_used_at > ?2 - ?3
                       AND user_id IN (SELECT id FROM auth_users)"
                ),
                params![token_hash, now, WEB_SESSION_IDLE],
                web_session_row,
            )
            .optional()?)
    }

    /// Roll `last_used_at` forward, unless it moved in the last minute.
    /// True when it wrote.
    pub fn auth_touch_web_session(&mut self, id: Uuid, now: i64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE auth_web_sessions SET last_used_at = ?2
             WHERE id = ?1 AND revoked_at IS NULL AND last_used_at <= ?2 - ?3",
            params![id.to_string(), now, WEB_SESSION_TOUCH_EVERY],
        )?;
        Ok(n == 1)
    }

    /// Every session, revoked and lapsed ones included, oldest first.
    pub fn auth_web_sessions(&self) -> Result<Vec<WebSession>> {
        let mut st = self.conn.prepare(&format!("SELECT {WEB_SESSION_COLS} FROM auth_web_sessions ORDER BY created_at, id"))?;
        let rows = st.query_map([], web_session_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Revoke a session by its cookie's hash (sign-out). The session, when
    /// one was live.
    pub fn auth_revoke_web_session_by_hash(&mut self, token_hash: &str, now: i64) -> Result<Option<WebSession>> {
        let found = self
            .conn
            .query_row(
                &format!("SELECT {WEB_SESSION_COLS} FROM auth_web_sessions WHERE token_hash = ?1 AND revoked_at IS NULL"),
                [token_hash],
                web_session_row,
            )
            .optional()?;
        let Some(s) = found else { return Ok(None) };
        self.conn.execute("UPDATE auth_web_sessions SET revoked_at = ?2 WHERE id = ?1", params![s.id.to_string(), now])?;
        Ok(Some(WebSession { revoked_at: Some(now), ..s }))
    }

    /// Revoke an unrevoked session by id or a unique id prefix. None when
    /// nothing (or more than one session) matches.
    pub fn auth_revoke_web_session(&mut self, key: &str, now: i64) -> Result<Option<WebSession>> {
        let key = key.trim().to_lowercase();
        if key.is_empty() {
            return Ok(None);
        }
        let hits: Vec<WebSession> = self
            .auth_web_sessions()?
            .into_iter()
            .filter(|s| s.revoked_at.is_none() && s.id.to_string().starts_with(&key))
            .collect();
        let [s] = hits.as_slice() else {
            return Ok(None);
        };
        self.conn.execute(
            "UPDATE auth_web_sessions SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![s.id.to_string(), now],
        )?;
        Ok(Some(WebSession { revoked_at: Some(now), ..s.clone() }))
    }

    // ---- clients ----

    /// Is this client the person's own app (first party)?
    pub fn oauth_is_first_party(&self, client_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM oauth_first_party WHERE client_id = ?1)",
            [client_id],
            |r| r.get(0),
        )?)
    }

    /// Mark a client first party (the server's own fixed app client).
    pub fn oauth_mark_first_party(&mut self, client_id: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO oauth_first_party (client_id, added_at) VALUES (?1, ?2)",
            params![client_id, now],
        )?;
        Ok(())
    }

    /// Once per database: the DCR clients registered before first-party
    /// clients were pinned whose every redirect is the app's scheme become
    /// first party, so the app's live sessions keep working. Returns how
    /// many; later registrations never go through here.
    pub fn oauth_grandfather_first_party(&mut self, app_redirect: &str, now: i64) -> Result<usize> {
        const KEY: &str = "auth.first_party_grandfathered";
        let done: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [KEY], |r| r.get(0))
            .optional()?;
        if done.is_some() {
            return Ok(0);
        }
        // only clients someone actually signed in with: the app's exact
        // redirect AND a live grant (unrevoked, with an unexpired refresh
        // token). A registration nobody completed — anyone could DCR the
        // scheme before this build — is never pinned.
        let rows: Vec<(String, String)> = {
            let mut st = self.conn.prepare(
                "SELECT c.client_id, c.redirect_uris FROM oauth_clients c
                 WHERE c.kind = 'dcr' AND EXISTS (
                     SELECT 1 FROM oauth_grants g JOIN oauth_refresh_tokens t ON t.grant_id = g.id
                     WHERE g.client_id = c.client_id AND g.revoked_at IS NULL AND t.expires_at > ?1)",
            )?;
            st.query_map([now], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        for (id, uris) in rows {
            let uris: Vec<String> = serde_json::from_str(&uris).unwrap_or_default();
            if uris.len() == 1 && uris[0] == app_redirect {
                tx.execute("INSERT OR IGNORE INTO oauth_first_party (client_id, added_at) VALUES (?1, ?2)", params![id, now])?;
                n += 1;
            }
        }
        tx.execute("INSERT INTO settings (key, value) VALUES (?1, '1') ON CONFLICT (key) DO NOTHING", [KEY])?;
        tx.commit()?;
        Ok(n)
    }

    pub fn oauth_upsert_client(&mut self, c: &OAuthClient) -> Result<()> {
        self.conn.execute(
            "INSERT INTO oauth_clients (client_id, kind, client_name, redirect_uris, metadata, created_at, refresh_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (client_id) DO UPDATE SET client_name = excluded.client_name,
                 redirect_uris = excluded.redirect_uris, metadata = excluded.metadata,
                 refresh_at = excluded.refresh_at",
            params![
                c.client_id,
                c.kind,
                c.client_name,
                serde_json::to_string(&c.redirect_uris)?,
                c.metadata,
                c.created_at,
                c.refresh_at
            ],
        )?;
        Ok(())
    }

    pub fn oauth_client(&self, client_id: &str) -> Result<Option<OAuthClient>> {
        let row = self
            .conn
            .query_row(
                "SELECT client_id, kind, client_name, redirect_uris, metadata, created_at, refresh_at
                 FROM oauth_clients WHERE client_id = ?1",
                [client_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((client_id, kind, client_name, uris, metadata, created_at, refresh_at)) = row else {
            return Ok(None);
        };
        Ok(Some(OAuthClient {
            client_id,
            kind,
            client_name,
            redirect_uris: serde_json::from_str(&uris)?,
            metadata,
            created_at,
            refresh_at,
        }))
    }

    // ---- authorization codes ----

    pub fn oauth_insert_code(&mut self, c: &AuthCode) -> Result<()> {
        self.conn.execute(
            "INSERT INTO oauth_codes (code_hash, client_id, user_id, redirect_uri, code_challenge, resource, scope, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                c.code_hash,
                c.client_id,
                c.user_id.to_string(),
                c.redirect_uri,
                c.code_challenge,
                c.resource,
                c.scope,
                c.expires_at
            ],
        )?;
        Ok(())
    }

    /// Spend a code. A second presentation revokes the grant the first one
    /// produced (RFC 6749 §4.1.2): whoever replays it has stolen it.
    pub fn oauth_consume_code(&mut self, code_hash: &str, now: i64) -> Result<CodeOutcome> {
        let tx = self.conn.transaction()?;
        let row = tx
            .query_row(
                "SELECT client_id, user_id, redirect_uri, code_challenge, resource, scope, expires_at, used_at, grant_id
                 FROM oauth_codes WHERE code_hash = ?1",
                [code_hash],
                |r| {
                    Ok((
                        AuthCode {
                            code_hash: code_hash.to_string(),
                            client_id: r.get(0)?,
                            user_id: uuid_of(r.get(1)?)?,
                            redirect_uri: r.get(2)?,
                            code_challenge: r.get(3)?,
                            resource: r.get(4)?,
                            scope: r.get(5)?,
                            expires_at: r.get(6)?,
                        },
                        r.get::<_, Option<i64>>(7)?,
                        r.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((code, used_at, grant_id)) = row else {
            return Ok(CodeOutcome::Invalid);
        };
        if used_at.is_some() {
            let grant_id = grant_id.map(uuid_of).transpose()?;
            if let Some(g) = grant_id {
                revoke_grant(&tx, g, "authorization code replayed", now)?;
            }
            tx.commit()?;
            return Ok(CodeOutcome::Replayed { grant_id });
        }
        if code.expires_at <= now {
            return Ok(CodeOutcome::Invalid);
        }
        tx.execute("UPDATE oauth_codes SET used_at = ?2 WHERE code_hash = ?1", params![code_hash, now])?;
        tx.commit()?;
        Ok(CodeOutcome::Fresh(code))
    }

    // ---- grants and tokens ----

    /// Create a grant from a spent code and issue its first token pair, in
    /// one transaction.
    pub fn oauth_issue_grant(
        &mut self,
        code_hash: Option<&str>,
        grant: &Grant,
        access_hash: &str,
        access_expires: i64,
        refresh_hash: &str,
        refresh_expires: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO oauth_grants (id, client_id, user_id, resource, scope, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                grant.id.to_string(),
                grant.client_id,
                grant.user_id.to_string(),
                grant.resource,
                grant.scope,
                grant.created_at
            ],
        )?;
        if let Some(c) = code_hash {
            tx.execute("UPDATE oauth_codes SET grant_id = ?2 WHERE code_hash = ?1", params![c, grant.id.to_string()])?;
        }
        insert_tokens(&tx, grant.id, grant.created_at, access_hash, access_expires, refresh_hash, refresh_expires)?;
        tx.commit()?;
        Ok(())
    }

    /// Rotate a refresh token: on success the old one is spent and the new
    /// pair is stored, atomically. Presenting a spent one revokes its grant —
    /// except a retry within [`REFRESH_GRACE`] of the rotation whose
    /// successor was never used (the client lost the response): the unused
    /// successor pair is replaced by the new one (`Reissued`). Only hashes are
    /// stored, so the lost pair cannot be handed back verbatim.
    pub fn oauth_rotate_refresh(
        &mut self,
        refresh_hash: &str,
        now: i64,
        new_access_hash: &str,
        access_expires: i64,
        new_refresh_hash: &str,
        refresh_expires: i64,
    ) -> Result<RefreshOutcome> {
        let tx = self.conn.transaction()?;
        let row = tx
            .query_row(
                "SELECT grant_id, expires_at, used_at, next_refresh, next_access
                 FROM oauth_refresh_tokens WHERE token_hash = ?1",
                [refresh_hash],
                |r| {
                    Ok((
                        uuid_of(r.get(0)?)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((grant_id, expires_at, used_at, next_refresh, next_access)) = row else {
            return Ok(RefreshOutcome::Invalid);
        };
        let grant = tx
            .query_row(
                &format!("SELECT {GRANT_COLS} FROM oauth_grants g WHERE g.id = ?1"),
                [grant_id.to_string()],
                grant_row,
            )
            .optional()?;
        let Some(grant) = grant else {
            return Ok(RefreshOutcome::Invalid);
        };
        if grant.revoked_at.is_some() {
            return Ok(RefreshOutcome::Invalid);
        }
        if let Some(used) = used_at {
            // the successor is still unused (and unexpired) only if nobody
            // has rotated it: that is a lost response, not a stolen token
            let successor_unused = match &next_refresh {
                Some(n) => tx
                    .query_row(
                        "SELECT 1 FROM oauth_refresh_tokens WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
                        params![n, now],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some(),
                None => false,
            };
            if now - used <= REFRESH_GRACE && successor_unused {
                tx.execute("DELETE FROM oauth_refresh_tokens WHERE token_hash = ?1", [next_refresh.as_deref()])?;
                tx.execute("DELETE FROM oauth_access_tokens WHERE token_hash = ?1", [next_access.as_deref()])?;
                insert_tokens(&tx, grant_id, now, new_access_hash, access_expires, new_refresh_hash, refresh_expires)?;
                // the window stays anchored at the first rotation
                tx.execute(
                    "UPDATE oauth_refresh_tokens SET next_refresh = ?2, next_access = ?3 WHERE token_hash = ?1",
                    params![refresh_hash, new_refresh_hash, new_access_hash],
                )?;
                tx.commit()?;
                return Ok(RefreshOutcome::Reissued(grant));
            }
            revoke_grant(&tx, grant_id, "refresh token reused", now)?;
            tx.commit()?;
            return Ok(RefreshOutcome::Reused { grant_id });
        }
        if expires_at <= now {
            return Ok(RefreshOutcome::Invalid);
        }
        let spent = tx.execute(
            "UPDATE oauth_refresh_tokens SET used_at = ?2, next_refresh = ?3, next_access = ?4
             WHERE token_hash = ?1 AND used_at IS NULL",
            params![refresh_hash, now, new_refresh_hash, new_access_hash],
        )?;
        if spent != 1 {
            revoke_grant(&tx, grant_id, "refresh token reused", now)?;
            tx.commit()?;
            return Ok(RefreshOutcome::Reused { grant_id });
        }
        insert_tokens(&tx, grant_id, now, new_access_hash, access_expires, new_refresh_hash, refresh_expires)?;
        tx.commit()?;
        Ok(RefreshOutcome::Rotated(grant))
    }

    /// The live grant behind an access token: unexpired, grant not revoked.
    pub fn oauth_access_grant(&self, access_hash: &str, now: i64) -> Result<Option<Grant>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {GRANT_COLS} FROM oauth_access_tokens t JOIN oauth_grants g ON g.id = t.grant_id
                     WHERE t.token_hash = ?1 AND t.expires_at > ?2 AND g.revoked_at IS NULL"
                ),
                params![access_hash, now],
                grant_row,
            )
            .optional()?)
    }

    pub fn oauth_grants(&self, include_revoked: bool) -> Result<Vec<Grant>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {GRANT_COLS} FROM oauth_grants g WHERE ?1 OR g.revoked_at IS NULL ORDER BY g.created_at"
        ))?;
        let rows = st.query_map([include_revoked], grant_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Revoke a grant by id (or a unique prefix). Returns the grant id.
    pub fn oauth_revoke_grant(&mut self, id: &str, why: &str, now: i64) -> Result<Option<Uuid>> {
        let id = id.trim();
        let ids: Vec<String> = {
            let mut st = self
                .conn
                .prepare("SELECT id FROM oauth_grants WHERE id LIKE ?1 || '%' AND revoked_at IS NULL")?;
            st.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        if ids.len() != 1 || id.is_empty() {
            return Ok(None);
        }
        let g = uuid_of(ids[0].clone())?;
        revoke_grant(&self.conn, g, why, now)?;
        Ok(Some(g))
    }

    /// RFC 7009: revoke the grant behind an access or refresh token.
    pub fn oauth_revoke_by_token(&mut self, token_hash: &str, why: &str, now: i64) -> Result<Option<Uuid>> {
        let grant: Option<String> = self
            .conn
            .query_row(
                "SELECT grant_id FROM oauth_access_tokens WHERE token_hash = ?1
                 UNION ALL SELECT grant_id FROM oauth_refresh_tokens WHERE token_hash = ?1 LIMIT 1",
                [token_hash],
                |r| r.get(0),
            )
            .optional()?;
        let Some(g) = grant.map(uuid_of).transpose()? else {
            return Ok(None);
        };
        revoke_grant(&self.conn, g, why, now)?;
        Ok(Some(g))
    }

    /// Delete everything past its use: expired codes, tokens and enrollment
    /// links, tokens of revoked grants, and grants revoked over 30 days ago.
    pub fn oauth_cleanup(&mut self, now: i64) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        n += tx.execute("DELETE FROM oauth_codes WHERE expires_at <= ?1 - 600", [now])?;
        n += tx.execute("DELETE FROM oauth_access_tokens WHERE expires_at <= ?1", [now])?;
        // spent refresh tokens stay until expiry: they are the reuse tripwire
        n += tx.execute("DELETE FROM oauth_refresh_tokens WHERE expires_at <= ?1", [now])?;
        n += tx.execute(
            "DELETE FROM oauth_access_tokens WHERE grant_id IN (SELECT id FROM oauth_grants WHERE revoked_at IS NOT NULL)",
            [],
        )?;
        n += tx.execute("DELETE FROM oauth_grants WHERE revoked_at IS NOT NULL AND revoked_at <= ?1 - 2592000", [now])?;
        n += tx.execute("DELETE FROM auth_enrollments WHERE expires_at <= ?1", [now])?;
        // web sessions: gone 30 days after they ended (revoked, lapsed, idle),
        // so `auth list` still shows a recent sign-out
        n += tx.execute(
            "DELETE FROM auth_web_sessions
             WHERE MIN(COALESCE(revoked_at, expires_at), expires_at, last_used_at + ?2) <= ?1 - 2592000",
            params![now, WEB_SESSION_IDLE],
        )?;
        tx.commit()?;
        Ok(n)
    }
}

fn insert_credential(conn: &rusqlite::Connection, c: &AuthCredential) -> Result<()> {
    conn.execute(
        "INSERT INTO auth_credentials (id, user_id, cred_id, passkey, label, created_at, last_used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            c.id.to_string(),
            c.user_id.to_string(),
            c.cred_id,
            c.passkey,
            c.label,
            c.created_at,
            c.last_used_at
        ],
    )?;
    Ok(())
}

fn insert_tokens(
    conn: &rusqlite::Connection,
    grant: Uuid,
    now: i64,
    access_hash: &str,
    access_expires: i64,
    refresh_hash: &str,
    refresh_expires: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO oauth_access_tokens (token_hash, grant_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        params![access_hash, grant.to_string(), now, access_expires],
    )?;
    conn.execute(
        "INSERT INTO oauth_refresh_tokens (token_hash, grant_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        params![refresh_hash, grant.to_string(), now, refresh_expires],
    )?;
    Ok(())
}

fn revoke_grant(conn: &rusqlite::Connection, grant: Uuid, why: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE oauth_grants SET revoked_at = ?2, revoke_why = ?3 WHERE id = ?1 AND revoked_at IS NULL",
        params![grant.to_string(), now, why],
    )?;
    conn.execute("DELETE FROM oauth_access_tokens WHERE grant_id = ?1", [grant.to_string()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockStore, PrincipalKind};

    fn store_with_owner() -> (SqliteStore, AuthUser) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "me", None).unwrap();
        let u = s.auth_ensure_owner(p.id, "me", 100).unwrap();
        (s, u)
    }

    fn grant(u: &AuthUser, now: i64) -> Grant {
        Grant {
            id: Uuid::now_v7(),
            client_id: "c1".into(),
            user_id: u.id,
            resource: None,
            scope: "grimoire".into(),
            created_at: now,
            revoked_at: None,
            revoke_why: None,
        }
    }

    #[test]
    fn owner_is_created_once() {
        let (mut s, u) = store_with_owner();
        let again = s.auth_ensure_owner(u.principal_id, "other", 200).unwrap();
        assert_eq!(again, u);
        assert_eq!(s.auth_users().unwrap().len(), 1);
    }

    #[test]
    fn web_sessions_expire_idle_and_absolute_and_revoke() {
        let (mut s, u) = store_with_owner();
        let t0 = 1_000_000;
        let w = s.auth_create_web_session(u.id, "w1", "Mac · Safari", t0).unwrap();
        assert_eq!(w.expires_at, t0 + WEB_SESSION_ABSOLUTE);
        assert_eq!(s.auth_web_session_by_hash("w1", t0 + 1).unwrap().unwrap().id, w.id);
        assert!(s.auth_web_session_by_hash("nope", t0 + 1).unwrap().is_none());
        // idle: one second short of 14 days is live, 14 days is not
        assert!(s.auth_web_session_by_hash("w1", t0 + WEB_SESSION_IDLE - 1).unwrap().is_some());
        assert!(s.auth_web_session_by_hash("w1", t0 + WEB_SESSION_IDLE).unwrap().is_none());
        // rolling: touched once a minute at most
        assert!(!s.auth_touch_web_session(w.id, t0 + 59).unwrap());
        assert!(s.auth_touch_web_session(w.id, t0 + 60).unwrap());
        // keep it busy: every 13 days, so idle never ends it, but 90 days does
        let mut t = t0 + 60;
        while t + 13 * 86400 < t0 + WEB_SESSION_ABSOLUTE {
            t += 13 * 86400;
            assert!(s.auth_web_session_by_hash("w1", t).unwrap().is_some(), "live at {t}");
            assert!(s.auth_touch_web_session(w.id, t).unwrap());
        }
        assert!(s.auth_web_session_by_hash("w1", t0 + WEB_SESSION_ABSOLUTE).unwrap().is_none(), "absolute end");
        // revoke by prefix and by hash
        let w2 = s.auth_create_web_session(u.id, "w2", "iPhone · Safari", t0).unwrap();
        let w3 = s.auth_create_web_session(u.id, "w3", "", t0).unwrap();
        assert!(s.auth_revoke_web_session("", t0).unwrap().is_none());
        let r = s.auth_revoke_web_session(&w2.id.to_string()[..30], t0 + 5).unwrap().unwrap();
        assert_eq!((r.id, r.revoked_at), (w2.id, Some(t0 + 5)));
        assert!(s.auth_web_session_by_hash("w2", t0 + 6).unwrap().is_none());
        assert!(s.auth_revoke_web_session(&w2.id.to_string(), t0 + 7).unwrap().is_none(), "already revoked");
        assert_eq!(s.auth_revoke_web_session_by_hash("w3", t0 + 8).unwrap().unwrap().id, w3.id);
        assert!(s.auth_revoke_web_session_by_hash("w3", t0 + 9).unwrap().is_none());
        assert_eq!(s.auth_web_sessions().unwrap().len(), 3);
        // cleanup drops sessions 30 days after they ended
        s.oauth_cleanup(t0 + 10).unwrap();
        assert_eq!(s.auth_web_sessions().unwrap().len(), 3);
        s.oauth_cleanup(t0 + 8 + 2_592_000).unwrap();
        let left: Vec<Uuid> = s.auth_web_sessions().unwrap().into_iter().map(|x| x.id).collect();
        assert_eq!(left, vec![w.id], "w2 and w3 ended at sign-out; w1 is still within 30 days of its end");
    }

    #[test]
    fn api_tokens_live_until_revoked() {
        let (mut s, u) = store_with_owner();
        let t = s.auth_create_api_token(u.id, "laptop", "h1", 100).unwrap();
        assert!(matches!(s.auth_create_api_token(u.id, "laptop", "h2", 101), Err(StoreError::InvalidOp(_))));
        assert_eq!(s.auth_api_token_by_hash("h1").unwrap().unwrap().id, t.id);
        assert!(s.auth_api_token_by_hash("nope").unwrap().is_none());
        // touched once a minute at most
        assert!(s.auth_touch_api_token(t.id, 200).unwrap());
        assert!(!s.auth_touch_api_token(t.id, 259).unwrap());
        assert!(s.auth_touch_api_token(t.id, 260).unwrap());
        assert_eq!(s.auth_api_token_by_hash("h1").unwrap().unwrap().last_used_at, Some(260));
        // revoke by name; the name is free again
        assert!(s.auth_revoke_api_token("nobody", 300).unwrap().is_none());
        assert_eq!(s.auth_revoke_api_token("laptop", 300).unwrap().unwrap().revoked_at, Some(300));
        assert!(s.auth_api_token_by_hash("h1").unwrap().is_none());
        assert!(s.auth_revoke_api_token("laptop", 301).unwrap().is_none(), "already revoked");
        let t2 = s.auth_create_api_token(u.id, "laptop", "h3", 400).unwrap();
        // revoke by id prefix
        let prefix = &t2.id.to_string()[..18];
        assert_eq!(s.auth_revoke_api_token(prefix, 500).unwrap().unwrap().id, t2.id);
        assert_eq!(s.auth_api_tokens().unwrap().len(), 2);
    }

    #[test]
    fn enrollment_link_is_single_use_and_expires() {
        let (mut s, u) = store_with_owner();
        s.auth_add_enrollment("h1", u.id, 1000).unwrap();
        assert_eq!(s.auth_peek_enrollment("h1", 999).unwrap(), Some(u.id));
        assert_eq!(s.auth_peek_enrollment("h1", 1000).unwrap(), None, "expired");
        let cred = AuthCredential {
            id: Uuid::now_v7(),
            user_id: u.id,
            cred_id: "cred".into(),
            passkey: "{}".into(),
            label: "phone".into(),
            created_at: 500,
            last_used_at: None,
        };
        assert!(s.auth_enroll_credential("h1", &cred, 500).unwrap());
        let second = AuthCredential { id: Uuid::now_v7(), cred_id: "cred2".into(), ..cred.clone() };
        assert!(!s.auth_enroll_credential("h1", &second, 501).unwrap(), "spent");
        assert_eq!(s.auth_credentials(Some(u.id)).unwrap().len(), 1);
        assert_eq!(s.auth_delete_credential(&cred.id.to_string()[..8]).unwrap(), 1);
        assert!(s.auth_credentials(None).unwrap().is_empty());
    }

    #[test]
    fn code_is_single_use_and_replay_revokes_the_grant() {
        let (mut s, u) = store_with_owner();
        let code = AuthCode {
            code_hash: "ch".into(),
            client_id: "c1".into(),
            user_id: u.id,
            redirect_uri: "http://127.0.0.1:1/cb".into(),
            code_challenge: "x".into(),
            resource: None,
            scope: "grimoire".into(),
            expires_at: 160,
        };
        s.oauth_insert_code(&code).unwrap();
        let CodeOutcome::Fresh(got) = s.oauth_consume_code("ch", 120).unwrap() else { panic!() };
        assert_eq!(got, code);
        let g = grant(&u, 120);
        s.oauth_issue_grant(Some("ch"), &g, "a1", 3720, "r1", 9999).unwrap();
        assert!(s.oauth_access_grant("a1", 121).unwrap().is_some());
        assert_eq!(s.oauth_consume_code("ch", 130).unwrap(), CodeOutcome::Replayed { grant_id: Some(g.id) });
        assert!(s.oauth_access_grant("a1", 131).unwrap().is_none(), "replay revoked the grant");
        assert_eq!(s.oauth_consume_code("nope", 130).unwrap(), CodeOutcome::Invalid);
    }

    #[test]
    fn expired_code_is_invalid() {
        let (mut s, u) = store_with_owner();
        let code = AuthCode {
            code_hash: "ch".into(),
            client_id: "c1".into(),
            user_id: u.id,
            redirect_uri: "x".into(),
            code_challenge: "x".into(),
            resource: None,
            scope: "grimoire".into(),
            expires_at: 160,
        };
        s.oauth_insert_code(&code).unwrap();
        assert_eq!(s.oauth_consume_code("ch", 160).unwrap(), CodeOutcome::Invalid);
    }

    #[test]
    fn refresh_rotates_and_reuse_revokes_the_family() {
        let (mut s, u) = store_with_owner();
        let g = grant(&u, 100);
        s.oauth_issue_grant(None, &g, "a1", 200, "r1", 1000).unwrap();
        assert!(s.oauth_access_grant("a1", 199).unwrap().is_some());
        assert!(s.oauth_access_grant("a1", 200).unwrap().is_none(), "access expiry enforced");
        let RefreshOutcome::Rotated(got) = s.oauth_rotate_refresh("r1", 150, "a2", 250, "r2", 1000).unwrap() else {
            panic!()
        };
        assert_eq!(got.id, g.id);
        // the successor is used, so r1 again is theft even inside the window
        assert!(matches!(s.oauth_rotate_refresh("r2", 155, "a3", 255, "r3", 1000).unwrap(), RefreshOutcome::Rotated(_)));
        assert_eq!(
            s.oauth_rotate_refresh("r1", 160, "a4", 260, "r4", 1000).unwrap(),
            RefreshOutcome::Reused { grant_id: g.id }
        );
        assert!(s.oauth_access_grant("a3", 161).unwrap().is_none());
        assert_eq!(s.oauth_rotate_refresh("r3", 170, "a5", 270, "r5", 1000).unwrap(), RefreshOutcome::Invalid);
        assert_eq!(s.oauth_grants(false).unwrap().len(), 0);
        assert_eq!(s.oauth_grants(true).unwrap()[0].revoke_why.as_deref(), Some("refresh token reused"));
    }

    #[test]
    fn retry_inside_the_grace_window_reissues() {
        let (mut s, u) = store_with_owner();
        let g = grant(&u, 100);
        s.oauth_issue_grant(None, &g, "a1", 2000, "r1", 9000).unwrap();
        assert!(matches!(s.oauth_rotate_refresh("r1", 150, "a2", 2000, "r2", 9000).unwrap(), RefreshOutcome::Rotated(_)));
        // the response was lost: r1 again within 60s, r2 never used
        let RefreshOutcome::Reissued(got) = s.oauth_rotate_refresh("r1", 150 + REFRESH_GRACE, "a3", 2000, "r3", 9000).unwrap()
        else {
            panic!()
        };
        assert_eq!(got.id, g.id);
        // the lost pair is dead (not a reuse tripwire); the new one is live
        assert!(s.oauth_access_grant("a2", 211).unwrap().is_none());
        assert_eq!(s.oauth_rotate_refresh("r2", 211, "x", 2000, "y", 9000).unwrap(), RefreshOutcome::Invalid);
        assert!(s.oauth_access_grant("a3", 211).unwrap().is_some());
        assert_eq!(s.oauth_grants(false).unwrap().len(), 1, "grant survives");
        assert!(matches!(s.oauth_rotate_refresh("r3", 212, "a4", 2000, "r4", 9000).unwrap(), RefreshOutcome::Rotated(_)));
    }

    #[test]
    fn retry_after_the_grace_window_revokes() {
        let (mut s, u) = store_with_owner();
        let g = grant(&u, 100);
        s.oauth_issue_grant(None, &g, "a1", 2000, "r1", 9000).unwrap();
        assert!(matches!(s.oauth_rotate_refresh("r1", 150, "a2", 2000, "r2", 9000).unwrap(), RefreshOutcome::Rotated(_)));
        assert_eq!(
            s.oauth_rotate_refresh("r1", 150 + REFRESH_GRACE + 1, "a3", 2000, "r3", 9000).unwrap(),
            RefreshOutcome::Reused { grant_id: g.id }
        );
        assert!(s.oauth_access_grant("a2", 212).unwrap().is_none());
        assert_eq!(s.oauth_grants(false).unwrap().len(), 0);
    }

    #[test]
    fn expired_refresh_is_invalid_and_cleanup_sweeps() {
        let (mut s, u) = store_with_owner();
        let g = grant(&u, 100);
        s.oauth_issue_grant(None, &g, "a1", 200, "r1", 300).unwrap();
        assert_eq!(s.oauth_rotate_refresh("r1", 300, "a2", 400, "r2", 900).unwrap(), RefreshOutcome::Invalid);
        assert!(s.oauth_cleanup(301).unwrap() >= 2);
        assert_eq!(s.oauth_rotate_refresh("r1", 301, "a2", 400, "r2", 900).unwrap(), RefreshOutcome::Invalid);
    }

    #[test]
    fn revoke_by_prefix() {
        let (mut s, u) = store_with_owner();
        let g = grant(&u, 100);
        s.oauth_issue_grant(None, &g, "a1", 2000, "r1", 3000).unwrap();
        assert_eq!(s.oauth_revoke_grant(&g.id.to_string()[..13], "cli", 150).unwrap(), Some(g.id));
        assert!(s.oauth_access_grant("a1", 151).unwrap().is_none());
        assert_eq!(s.oauth_rotate_refresh("r1", 152, "a2", 400, "r2", 900).unwrap(), RefreshOutcome::Invalid);
        assert_eq!(s.oauth_revoke_grant("", "cli", 150).unwrap(), None);
    }

    #[test]
    fn clients_round_trip() {
        let (mut s, _) = store_with_owner();
        let c = OAuthClient {
            client_id: "https://x.example/client.json".into(),
            kind: "cimd".into(),
            client_name: "X".into(),
            redirect_uris: vec!["https://x.example/cb".into()],
            metadata: "{}".into(),
            created_at: 1,
            refresh_at: Some(2),
        };
        s.oauth_upsert_client(&c).unwrap();
        assert_eq!(s.oauth_client(&c.client_id).unwrap(), Some(c.clone()));
        let c2 = OAuthClient { client_name: "Y".into(), refresh_at: Some(9), ..c.clone() };
        s.oauth_upsert_client(&c2).unwrap();
        assert_eq!(s.oauth_client(&c.client_id).unwrap(), Some(c2));
    }
}
