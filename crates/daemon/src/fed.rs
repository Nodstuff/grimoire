//! Federation endpoint (ADR 0002 decisions 5/6/7, ticket #56).
//!
//! An iroh endpoint on its own ALPN, alongside — never inside — the HTTP
//! surfaces. Deny-by-default: a connection from a pubkey that is not a
//! non-revoked contact may do exactly one thing, redeem an invite secret;
//! every other request from an unknown peer is refused. Known contacts get
//! the (for now tiny) authenticated protocol. /api and /admin are not
//! reachable here by construction — this module only ever touches the store
//! through the specific calls below.
//!
//! Wire format: one request per bi-stream. The opener writes one JSON frame
//! and finishes; the acceptor replies with one JSON frame. Every frame
//! carries `v` (protocol version) — refused loudly on mismatch, so the
//! version dance is cheap now instead of impossible later.

use anyhow::{Context, Result};
use grimoire_store::{BlockStore, SqliteStore};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

pub const ALPN: &[u8] = b"grimoire/fed/0";
pub const PROTOCOL_VERSION: u32 = 0;
/// Frame cap. Snapshots (#58) will stream doc-by-doc, not grow this.
const MAX_FRAME: usize = 32 * 1024 * 1024;

/// Invite secrets are stored hashed (share_invites.secret_hash); the secret
/// itself only ever exists inside the grimoire:// link. Mint (#57) and
/// redeem must agree on this.
pub fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Frame<T> {
    pub v: u32,
    #[serde(flatten)]
    pub msg: T,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// The only request an unknown peer may make.
    Redeem { secret: String, petname: String },
    /// Authenticated no-op: proves the allowlist path end to end.
    Ping,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Redeemed {
        share_id: String,
        root_doc: String,
        root_title: String,
        permission: String,
        /// The owner's self-chosen display name — the grantee's default
        /// petname for them (renameable, like any petname).
        owner_name: String,
    },
    Pong,
    Refused {
        reason: String,
    },
}

/// The invite ticket: everything a grantee needs to join, serialized as a
/// `grimoire://join/<base64url(json)>` link. The node id is enough to dial —
/// iroh discovery resolves it — so no relay hint in v0. The secret is the
/// trust anchor and only ever exists here and in flight.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Ticket {
    pub v: u32,
    /// Owner's endpoint id (hex ed25519 pubkey).
    pub node: String,
    pub share: String,
    pub secret: String,
}

const LINK_PREFIX: &str = "grimoire://join/";

impl Ticket {
    pub fn new(node: String, share: String, secret: String) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            node,
            share,
            secret,
        }
    }

    pub fn to_link(&self) -> String {
        let json = serde_json::to_vec(self).expect("ticket serializes");
        format!(
            "{LINK_PREFIX}{}",
            data_encoding::BASE64URL_NOPAD.encode(&json)
        )
    }

    pub fn parse(link: &str) -> Result<Self> {
        let encoded = link
            .trim()
            .strip_prefix(LINK_PREFIX)
            .context("not a grimoire://join/ link")?;
        let json = data_encoding::BASE64URL_NOPAD
            .decode(encoded.as_bytes())
            .context("ticket is not valid base64url")?;
        let ticket: Ticket = serde_json::from_slice(&json).context("ticket is not valid JSON")?;
        if ticket.v != PROTOCOL_VERSION {
            anyhow::bail!(
                "ticket is protocol version {} (this instance speaks {})",
                ticket.v,
                PROTOCOL_VERSION
            );
        }
        Ok(ticket)
    }
}

/// Bind the production endpoint: n0 relays + discovery, our identity.
pub async fn bind(secret: [u8; 32]) -> Result<Endpoint> {
    Endpoint::builder(presets::N0)
        .secret_key(SecretKey::from_bytes(&secret))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .context("binding federation endpoint")
}

/// Accept loop. Spawned once per daemon; lives until the endpoint closes.
pub async fn serve(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>) {
    tracing::info!("federation endpoint listening (node id {})", endpoint.id());
    while let Some(incoming) = endpoint.accept().await {
        let store = store.clone();
        tokio::spawn(async move {
            let conn = match incoming.accept() {
                Ok(accepting) => match accepting.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::debug!("federation handshake failed: {e:#}");
                        return;
                    }
                },
                Err(e) => {
                    tracing::debug!("federation accept failed: {e:#}");
                    return;
                }
            };
            let peer = conn.remote_id().to_string();
            if let Err(e) = handle_conn(conn, &peer, store).await {
                tracing::debug!(peer, "federation connection ended: {e:#}");
            }
        });
    }
}

async fn handle_conn(
    conn: iroh::endpoint::Connection,
    peer: &str,
    store: Arc<Mutex<SqliteStore>>,
) -> Result<()> {
    // authed = peer is a non-revoked contact. Checked once per request, not
    // once per connection: a successful redeem upgrades the session, and a
    // mid-session revoke takes effect on the next request.
    loop {
        let (mut send, mut recv) = match conn.accept_bi().await {
            Ok(s) => s,
            // peer closed: normal end of conversation
            Err(_) => return Ok(()),
        };
        let raw = recv.read_to_end(MAX_FRAME).await?;
        let response = match serde_json::from_slice::<Frame<Request>>(&raw) {
            Err(e) => Response::Refused {
                reason: format!("bad frame: {e}"),
            },
            Ok(frame) if frame.v != PROTOCOL_VERSION => Response::Refused {
                reason: format!(
                    "protocol version {} not supported (this instance speaks {})",
                    frame.v, PROTOCOL_VERSION
                ),
            },
            Ok(frame) => dispatch(frame.msg, peer, &store),
        };
        let out = serde_json::to_vec(&Frame {
            v: PROTOCOL_VERSION,
            msg: response,
        })?;
        send.write_all(&out).await?;
        send.finish()?;
    }
}

fn dispatch(req: Request, peer: &str, store: &Arc<Mutex<SqliteStore>>) -> Response {
    let mut store = store.lock().unwrap_or_else(|p| p.into_inner());
    let contact = match store.contact_by_pubkey(peer) {
        Ok(c) => c.filter(|c| !c.revoked),
        Err(e) => {
            return Response::Refused {
                reason: format!("store error: {e}"),
            };
        }
    };
    match req {
        Request::Redeem { secret, petname } => {
            match store.redeem_invite(&hash_secret(&secret), peer, &petname) {
                Ok((contact, share)) => {
                    tracing::info!(
                        peer,
                        petname = contact.petname,
                        share = %share.id,
                        "invite redeemed; contact paired"
                    );
                    let root_title = store
                        .get_doc(share.root_doc)
                        .map(|d| d.title)
                        .unwrap_or_default();
                    let owner_name = store
                        .list_principals()
                        .ok()
                        .and_then(|ps| {
                            ps.into_iter()
                                .find(|p| p.kind == grimoire_store::PrincipalKind::Human)
                        })
                        .map(|p| p.display_name)
                        .unwrap_or_else(|| "owner".into());
                    Response::Redeemed {
                        share_id: share.id.to_string(),
                        root_doc: share.root_doc.to_string(),
                        root_title,
                        permission: share.permission.as_str().to_string(),
                        owner_name,
                    }
                }
                Err(e) => {
                    tracing::warn!(peer, "invite redeem refused: {e}");
                    Response::Refused {
                        reason: e.to_string(),
                    }
                }
            }
        }
        Request::Ping => match contact {
            Some(c) => {
                tracing::debug!(peer, petname = c.petname, "ping");
                Response::Pong
            }
            None => {
                tracing::warn!(peer, "unauthenticated request refused");
                Response::Refused {
                    reason: "unknown peer: redeem an invite first".into(),
                }
            }
        },
    }
}

/// One request against a remote instance (grantee-side; the pull loop and
/// the redeem flow both come through here).
#[allow(dead_code)] // wired up by the pairing flow (#57) and pull loop (#59)
pub async fn request(
    endpoint: &Endpoint,
    addr: impl Into<EndpointAddr>,
    req: Request,
) -> Result<Response> {
    let conn = endpoint
        .connect(addr, ALPN)
        .await
        .context("dialing federation peer")?;
    let (mut send, mut recv) = conn.open_bi().await?;
    let out = serde_json::to_vec(&Frame {
        v: PROTOCOL_VERSION,
        msg: req,
    })?;
    send.write_all(&out).await?;
    send.finish()?;
    let raw = recv.read_to_end(MAX_FRAME).await?;
    let frame: Frame<Response> = serde_json::from_slice(&raw).context("bad response frame")?;
    Ok(frame.msg)
}

/// The outcome of a completed join, for the UI/CLI.
#[derive(Debug, Serialize)]
pub struct JoinOutcome {
    pub owner: String,
    pub owner_name: String,
    pub root_doc: String,
    pub root_title: String,
    pub permission: String,
}

/// One join attempt (grantee-side): dial the ticket's node, redeem, pair the
/// owner as a contact, and materialize the mirror root — a placeholder doc
/// under the origin UUID that the pull loop (#59) fills with the subtree.
pub async fn join_once(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    ticket: &Ticket,
) -> Result<JoinOutcome> {
    // dial by node id alone; iroh discovery resolves it
    let owner_id: iroh::EndpointId = ticket
        .node
        .parse()
        .context("ticket has a malformed node id")?;
    join_at(endpoint, store, ticket, EndpointAddr::from(owner_id)).await
}

/// join_once with an explicit address (tests, LAN-known peers).
pub async fn join_at(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    ticket: &Ticket,
    addr: EndpointAddr,
) -> Result<JoinOutcome> {
    let my_name = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        s.list_principals()
            .ok()
            .and_then(|ps| {
                ps.into_iter()
                    .find(|p| p.kind == grimoire_store::PrincipalKind::Human)
            })
            .map(|p| p.display_name)
            .unwrap_or_else(|| "someone".into())
    };
    let res = request(
        endpoint,
        addr,
        Request::Redeem {
            secret: ticket.secret.clone(),
            petname: my_name,
        },
    )
    .await?;
    let Response::Redeemed {
        share_id,
        root_doc,
        root_title,
        permission,
        owner_name,
    } = res
    else {
        anyhow::bail!("owner refused the invite: {res:?}");
    };

    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
    let owner_contact = s.pair_contact(&ticket.node, &owner_name)?;
    let root_uuid: uuid::Uuid = root_doc.parse().context("owner sent a bad doc id")?;
    let share_uuid: uuid::Uuid = share_id.parse().context("owner sent a bad share id")?;
    // same UUID = same doc; if a mirror root already exists (re-join), keep it
    if s.get_doc(root_uuid).is_err() {
        let title = if root_title.is_empty() {
            "(shared doc)".to_string()
        } else {
            root_title.clone()
        };
        s.create_doc_with_id(root_uuid, &title, None, owner_contact.principal)?;
    }
    s.upsert_mirror(root_uuid, owner_contact.id, share_uuid, 0)?;
    tracing::info!(
        owner = ticket.node,
        root = root_doc,
        permission,
        "joined share"
    );
    Ok(JoinOutcome {
        owner: ticket.node.clone(),
        owner_name,
        root_doc,
        root_title,
        permission,
    })
}

/// Background retry for joins whose owner was offline (async redeem,
/// ADR 0002 decision 6). Every failure is recorded; success removes the row.
pub async fn join_retry_loop(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>) {
    const RETRY_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
    loop {
        tokio::time::sleep(RETRY_EVERY).await;
        let pending = {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            s.list_pending_joins().unwrap_or_default()
        };
        for join in pending {
            let ticket = match Ticket::parse(&join.ticket) {
                Ok(t) => t,
                Err(e) => {
                    // unparseable tickets can never succeed; drop them
                    tracing::warn!("dropping unparseable pending join: {e:#}");
                    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
                    s.remove_pending_join(join.id).ok();
                    continue;
                }
            };
            match join_once(&endpoint, &store, &ticket).await {
                Ok(out) => {
                    tracing::info!(root = out.root_doc, "queued join completed");
                    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
                    s.remove_pending_join(join.id).ok();
                }
                Err(e) => {
                    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
                    s.record_join_attempt(join.id, &format!("{e:#}")).ok();
                }
            }
        }
    }
}

/// Mint a share invite: secret, hash, expiry, link (owner-side, #57).
pub fn mint_invite(
    store: &mut SqliteStore,
    node_id: &str,
    root_doc: uuid::Uuid,
    permission: grimoire_store::SharePermission,
) -> Result<(grimoire_store::Share, String)> {
    let share = store.create_share(root_doc, None, permission, None)?;
    let mut secret_bytes = [0u8; 32];
    getrandom::fill(&mut secret_bytes).expect("OS entropy");
    let secret = hex::encode(secret_bytes);
    let expires = (chrono::Utc::now() + chrono::Duration::days(7))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    store.create_invite(share.id, &hash_secret(&secret), &expires)?;
    let link = Ticket::new(node_id.to_string(), share.id.to_string(), secret).to_link();
    Ok((share, link))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::{PrincipalKind, SharePermission};
    use iroh::TransportAddr;

    /// Local-only endpoint: no relays, no discovery, explicit addressing.
    async fn local_endpoint() -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .bind_addr("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
            .unwrap()
            .bind()
            .await
            .unwrap()
    }

    fn direct_addr(ep: &Endpoint) -> EndpointAddr {
        EndpointAddr::from_parts(
            ep.id(),
            ep.bound_sockets().into_iter().map(TransportAddr::Ip),
        )
    }

    /// Owner store with one doc, one share, one minted invite.
    fn owner_store(secret: &str) -> Arc<Mutex<SqliteStore>> {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = s.create_doc("shared-runbook", None, tom.id).unwrap();
        let share = s
            .create_share(doc.id, None, SharePermission::View, None)
            .unwrap();
        s.create_invite(share.id, &hash_secret(secret), "2099-01-01T00:00:00.000Z")
            .unwrap();
        Arc::new(Mutex::new(s))
    }

    #[tokio::test]
    async fn unknown_peer_is_refused_everything_but_redeem() {
        let store = owner_store("the-secret");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone()));

        let stranger = local_endpoint().await;
        let res = request(&stranger, addr, Request::Ping).await.unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn redeem_pairs_and_upgrades_the_session() {
        let store = owner_store("the-secret");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone()));

        let alice = local_endpoint().await;
        let alice_id = alice.id().to_string();

        // wrong secret first: refused, nothing paired
        let res = request(
            &alice,
            addr.clone(),
            Request::Redeem {
                secret: "wrong".into(),
                petname: "alice".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));

        let res = request(
            &alice,
            addr.clone(),
            Request::Redeem {
                secret: "the-secret".into(),
                petname: "alice".into(),
            },
        )
        .await
        .unwrap();
        let Response::Redeemed { permission, .. } = res else {
            panic!("expected Redeemed, got {res:?}");
        };
        assert_eq!(permission, "view");

        // paired under alice's real endpoint id
        {
            let s = store.lock().unwrap();
            let c = s.contact_by_pubkey(&alice_id).unwrap().unwrap();
            assert_eq!(c.petname, "alice");
        }

        // session upgraded: authenticated requests now work
        let res = request(&alice, addr.clone(), Request::Ping).await.unwrap();
        assert_eq!(res, Response::Pong);

        // burned: same secret from another peer is refused
        let mallory = local_endpoint().await;
        let res = request(
            &mallory,
            addr,
            Request::Redeem {
                secret: "the-secret".into(),
                petname: "also-alice".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn revoked_contact_is_refused_on_next_request() {
        let store = owner_store("the-secret");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone()));

        let alice = local_endpoint().await;
        let alice_id = alice.id().to_string();
        request(
            &alice,
            addr.clone(),
            Request::Redeem {
                secret: "the-secret".into(),
                petname: "alice".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            request(&alice, addr.clone(), Request::Ping).await.unwrap(),
            Response::Pong
        );

        let contact_id = {
            let s = store.lock().unwrap();
            s.contact_by_pubkey(&alice_id).unwrap().unwrap().id
        };
        store.lock().unwrap().revoke_contact(contact_id).unwrap();

        let res = request(&alice, addr, Request::Ping).await.unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[test]
    fn ticket_link_round_trips() {
        let t = Ticket::new("ab".repeat(32), "share-id".into(), "s3cret".into());
        let link = t.to_link();
        assert!(link.starts_with("grimoire://join/"));
        assert_eq!(Ticket::parse(&link).unwrap(), t);
        assert_eq!(Ticket::parse(&format!("  {link}\n")).unwrap(), t); // pasted whitespace
        assert!(Ticket::parse("https://example.com/nope").is_err());
    }

    #[tokio::test]
    async fn join_materializes_mirror_root_and_pairs_both_sides() {
        // owner side: doc + minted invite via the real mint path
        let mut owner_store = SqliteStore::open_in_memory().unwrap();
        let tom = owner_store
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = owner_store.create_doc("Team Runbook", None, tom.id).unwrap();
        let owner_ep = local_endpoint().await;
        let owner_id = owner_ep.id().to_string();
        let (share, link) = mint_invite(
            &mut owner_store,
            &owner_id,
            doc.id,
            SharePermission::Propose,
        )
        .unwrap();
        let owner_store = Arc::new(Mutex::new(owner_store));
        let addr = direct_addr(&owner_ep);
        tokio::spawn(serve(owner_ep, owner_store.clone()));

        // grantee side
        let mut alice_store = SqliteStore::open_in_memory().unwrap();
        alice_store
            .create_principal(PrincipalKind::Human, "alice", None)
            .unwrap();
        let alice_store = Arc::new(Mutex::new(alice_store));
        let alice_ep = local_endpoint().await;

        let ticket = Ticket::parse(&link).unwrap();
        let out = join_at(&alice_ep, &alice_store, &ticket, addr)
            .await
            .unwrap();
        assert_eq!(out.owner_name, "tom");
        assert_eq!(out.root_title, "Team Runbook");
        assert_eq!(out.permission, "propose");

        // grantee: owner paired, mirror root exists under the ORIGIN uuid
        {
            let s = alice_store.lock().unwrap();
            let owner_contact = s.contact_by_pubkey(&owner_id).unwrap().unwrap();
            assert_eq!(owner_contact.petname, "tom");
            let mirror = s.get_mirror(doc.id).unwrap().unwrap();
            assert_eq!(mirror.owner, owner_contact.id);
            assert_eq!(mirror.synced_epoch, 0);
            assert_eq!(s.get_doc(doc.id).unwrap().title, "Team Runbook");
        }
        // owner: grantee paired under her real key, share active
        {
            let s = owner_store.lock().unwrap();
            let alice_contact = s
                .contact_by_pubkey(&alice_ep.id().to_string())
                .unwrap()
                .unwrap();
            assert_eq!(alice_contact.petname, "alice");
            let share = s.get_share(share.id).unwrap();
            assert_eq!(share.contact, Some(alice_contact.id));
        }
    }

    #[tokio::test]
    async fn version_mismatch_is_refused_loudly() {
        let store = owner_store("s");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store));

        let client = local_endpoint().await;
        let conn = client.connect(addr, ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(br#"{"v": 99, "type": "ping"}"#).await.unwrap();
        send.finish().unwrap();
        let raw = recv.read_to_end(MAX_FRAME).await.unwrap();
        let frame: Frame<Response> = serde_json::from_slice(&raw).unwrap();
        let Response::Refused { reason } = frame.msg else {
            panic!("expected Refused");
        };
        assert!(reason.contains("version"));
    }
}
