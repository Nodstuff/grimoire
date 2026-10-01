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

/// What presenting a refresh token did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Live and unused: it is now rotated out; issue its successor.
    Rotated(Grant),
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

const USER_COLS: &str = "id, principal_id, name, role, created_at";
const CRED_COLS: &str = "id, user_id, cred_id, passkey, label, created_at, last_used_at";
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
        self.conn.execute(
            "INSERT INTO auth_users (id, principal_id, name, role, created_at) VALUES (?1, ?2, ?3, 'owner', ?4)",
            params![id.to_string(), principal_id.to_string(), name, now],
        )?;
        self.auth_user(id)?.ok_or_else(|| StoreError::NotFound(format!("auth user {id}")))
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

    // ---- clients ----

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
    /// pair is stored, atomically. Presenting a spent one revokes its grant.
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
                "SELECT grant_id, expires_at, used_at FROM oauth_refresh_tokens WHERE token_hash = ?1",
                [refresh_hash],
                |r| Ok((uuid_of(r.get(0)?)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?)),
            )
            .optional()?;
        let Some((grant_id, expires_at, used_at)) = row else {
            return Ok(RefreshOutcome::Invalid);
        };
        if used_at.is_some() {
            revoke_grant(&tx, grant_id, "refresh token reused", now)?;
            tx.commit()?;
            return Ok(RefreshOutcome::Reused { grant_id });
        }
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
        if expires_at <= now || grant.revoked_at.is_some() {
            return Ok(RefreshOutcome::Invalid);
        }
        let spent = tx.execute(
            "UPDATE oauth_refresh_tokens SET used_at = ?2 WHERE token_hash = ?1 AND used_at IS NULL",
            params![refresh_hash, now],
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
        // the rotated-out token again: the whole family dies, r2 included
        assert_eq!(
            s.oauth_rotate_refresh("r1", 160, "a3", 260, "r3", 1000).unwrap(),
            RefreshOutcome::Reused { grant_id: g.id }
        );
        assert!(s.oauth_access_grant("a2", 161).unwrap().is_none());
        assert_eq!(s.oauth_rotate_refresh("r2", 170, "a4", 270, "r4", 1000).unwrap(), RefreshOutcome::Invalid);
        assert_eq!(s.oauth_grants(false).unwrap().len(), 0);
        assert_eq!(s.oauth_grants(true).unwrap()[0].revoke_why.as_deref(), Some("refresh token reused"));
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
