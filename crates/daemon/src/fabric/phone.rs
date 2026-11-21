//! A phone, in the tests: a P-256 key standing in for the Secure Enclave's,
//! the orders and settings it signs (SIGNING in `runtime.proto`), and the
//! pairing seal it posts.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use taisce_runtime_proto::prost::Message as _;
use taisce_runtime_proto::signing;
use taisce_runtime_proto::v1 as pb;

pub(crate) struct Phone {
    key: SigningKey,
    pub(crate) public_key: Vec<u8>,
    pub(crate) device_id: String,
    pub(crate) counter: u64,
}

pub(crate) fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

pub(crate) fn unb64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(s).unwrap()
}

impl Phone {
    pub(crate) fn new(seed: &str) -> Self {
        let d: [u8; 32] = Sha256::digest(format!("test phone {seed}")).into();
        let key = SigningKey::from_slice(&d).unwrap();
        let public_key = key.verifying_key().to_sec1_point(false).as_bytes().to_vec();
        let device_id = signing::device_id(&public_key);
        Phone { key, public_key, device_id, counter: 1000 }
    }

    pub(crate) fn sign(&self, context: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut m = context.to_vec();
        m.extend_from_slice(payload);
        let s: Signature = self.key.sign(&m);
        s.to_bytes().to_vec()
    }

    /// The body `POST /api/fabric/orders` takes, and the order's id.
    pub(crate) fn order(&mut self, runtime: &str, what: pb::order::What) -> (Value, String) {
        self.counter += 1;
        let order_id = uuid::Uuid::now_v7().to_string();
        let now = crate::auth::now();
        let o = pb::Order { runtime_id: runtime.into(), counter: self.counter, expires_at: now + 3600, what: Some(what), order_id: order_id.clone(), issued_at: now };
        (self.envelope(signing::ORDER, &o.encode_to_vec()), order_id)
    }

    /// A `send` order to one session.
    pub(crate) fn send(&mut self, runtime: &str, session: &str, text: &str, after_turn: &str) -> (Value, String) {
        self.order(
            runtime,
            pb::order::What::Command(pb::Command {
                session_id: session.into(),
                verb: pb::Verb::Send as i32,
                text: text.into(),
                after_turn: after_turn.into(),
                ..Default::default()
            }),
        )
    }

    pub(crate) fn envelope(&self, context: &[u8], payload: &[u8]) -> Value {
        json!({"payload": b64(payload), "device_id": self.device_id, "signature": b64(&self.sign(context, payload))})
    }

    /// Signed settings at `version`.
    pub(crate) fn settings(&self, version: u64, s: pb::Settings) -> Value {
        let s = pb::Settings { version, issued_at: crate::auth::now(), ..s };
        self.envelope(signing::SETTINGS, &s.encode_to_vec())
    }

    /// What the phone posts after scanning `code`: `{runtime_id, pairing_id, sealed}`.
    pub(crate) fn seal(&self, code: &str, name: &str) -> Value {
        let field = |k: &str| code.split(['?', '&']).find_map(|p| p.strip_prefix(&format!("{k}="))).unwrap().to_string();
        let (runtime_id, pairing_id) = (field("r"), field("p"));
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(field("s")).unwrap();
        let claim = pb::PairingClaim {
            runtime_id: runtime_id.clone(),
            pairing_id: pairing_id.clone(),
            public_key: self.public_key.clone(),
            device_name: name.into(),
            created_at: crate::auth::now(),
        }
        .encode_to_vec();
        let seal = pb::PairingSeal { signature: self.sign(signing::PAIRING, &claim), claim }.encode_to_vec();
        let hk = hkdf::Hkdf::<Sha256>::new(Some(pairing_id.as_bytes()), &secret);
        let mut k = [0u8; 32];
        hk.expand(b"taisce/pairing/v1", &mut k).unwrap();
        let nonce = [7u8; 12];
        let mut sealed = nonce.to_vec();
        sealed.extend(Aes256Gcm::new_from_slice(&k).unwrap().encrypt(&Nonce::try_from(&nonce[..]).unwrap(), Payload { msg: &seal, aad: runtime_id.as_bytes() }).unwrap());
        json!({"runtime_id": runtime_id, "pairing_id": pairing_id, "sealed": b64(&sealed)})
    }
}
