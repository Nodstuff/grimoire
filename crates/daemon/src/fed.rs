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
        permission: String,
    },
    Pong,
    Refused {
        reason: String,
    },
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
                    Response::Redeemed {
                        share_id: share.id.to_string(),
                        root_doc: share.root_doc.to_string(),
                        permission: share.permission.as_str().to_string(),
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
