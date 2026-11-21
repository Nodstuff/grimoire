//! The SIGNING test vectors (`proto/taisce/runtime/v1/testdata/
//! signing-vectors.json`, made by `make_vectors.py` with a hand-written
//! protobuf encoder) against this crate: prost's encoding, the p256
//! verifier, RFC 6979 signing, and the pairing seal. The app and Workbox run
//! the same file against CryptoKit and SwiftProtobuf.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use prost::Message as _;
use serde_json::Value;
use taisce_runtime_proto::signing::{self, Refusal};
use taisce_runtime_proto::v1 as pb;

const VECTORS: &str = include_str!("../../../proto/taisce/runtime/v1/testdata/signing-vectors.json");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn vectors() -> Value {
    serde_json::from_str(VECTORS).unwrap()
}

fn context(kind: &str) -> &'static [u8] {
    match kind {
        "order" => signing::ORDER,
        "settings" => signing::SETTINGS,
        "attention" => signing::ATTENTION,
        "pairing" => signing::PAIRING,
        other => panic!("kind {other}"),
    }
}

fn key(v: &Value, who: &str) -> (SigningKey, Vec<u8>) {
    let k = &v["keys"][who];
    let sk = SigningKey::from_slice(&unhex(k["private_scalar"].as_str().unwrap())).unwrap();
    (sk, unhex(k["public_key"].as_str().unwrap()))
}

fn vector<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["vectors"].as_array().unwrap().iter().find(|x| x["name"] == name).unwrap()
}

#[test]
fn keys_and_ids() {
    let v = vectors();
    for who in ["device", "runtime"] {
        let (sk, public) = key(&v, who);
        assert_eq!(sk.verifying_key().to_sec1_point(false).as_bytes(), &public[..], "{who}: the scalar's public key");
        assert_eq!(signing::device_id(&public), v["keys"][who]["device_id"].as_str().unwrap());
    }
    let d = &v["keys"]["device"];
    assert_eq!(signing::fingerprint(d["device_id"].as_str().unwrap()), d["fingerprint"].as_str().unwrap());
}

/// Every vector verifies under its context with either s, signs to the same
/// bytes under RFC 6979, and is the canonical encoding of its type.
#[test]
fn every_vector_verifies_and_is_canonical() {
    let v = vectors();
    for x in v["vectors"].as_array().unwrap() {
        let name = x["name"].as_str().unwrap();
        let ctx = context(x["kind"].as_str().unwrap());
        let (sk, public) = key(&v, x["signer"].as_str().unwrap());
        let payload = unhex(x["payload"].as_str().unwrap());
        let mut input = ctx.to_vec();
        input.extend_from_slice(&payload);
        assert_eq!(input, unhex(x["signing_input"].as_str().unwrap()), "{name}: signing input");
        for field in ["signature", "signature_other_s"] {
            let sig = unhex(x[field].as_str().unwrap());
            assert_eq!(signing::verify(ctx, &public, &payload, &sig), Ok(()), "{name}: {field}");
        }
        let ours: Signature = sk.sign(&input);
        assert_eq!(hex(&ours.to_bytes()), x["signature"].as_str().unwrap(), "{name}: RFC 6979 signature");
        let canonical = match x["kind"].as_str().unwrap() {
            "order" => signing::decode_canonical::<pb::Order>(&payload).map(|_| ()),
            "settings" => signing::decode_canonical::<pb::Settings>(&payload).map(|_| ()),
            "attention" => signing::decode_canonical::<pb::AttentionRequest>(&payload).map(|_| ()),
            "pairing" => signing::decode_canonical::<pb::PairingClaim>(&payload).map(|_| ()),
            _ => unreachable!(),
        };
        assert_eq!(canonical, Ok(()), "{name}: canonical");
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// prost builds the same bytes from the fields as the hand encoder did.
#[test]
fn prost_encodes_the_vectors_byte_for_byte() {
    let v = vectors();
    let rt = "0193a5b2-7c4d-7e8f-9a0b-1c2d3e4f5a6b".to_string();
    let session = "0193a5b2-8d9e-7f01-a2b3-c4d5e6f70819".to_string();
    let order = |order_id: &str, counter, expires_at, issued_at, what| pb::Order {
        runtime_id: rt.clone(),
        counter,
        expires_at,
        what: Some(what),
        order_id: order_id.into(),
        issued_at,
    };

    let send = order(
        "0193a5b3-0000-7000-8000-00000000b001",
        1795000000123,
        1795003600,
        1795000000,
        pb::order::What::Command(pb::Command {
            session_id: session.clone(),
            verb: pb::Verb::Send as i32,
            text: "good, now write the isolation test".into(),
            after_turn: "0193a5b3-0000-7000-8000-00000000c001".into(),
            ..Default::default()
        }),
    );
    assert_eq!(hex(&send.encode_to_vec()), vector(&v, "order_send")["payload"].as_str().unwrap());

    let request = unhex(vector(&v, "attention_approval")["payload"].as_str().unwrap());
    let approve = order(
        "0193a5b3-0000-7000-8000-00000000b002",
        1795000000456,
        1795000540,
        1795000030,
        pb::order::What::Command(pb::Command {
            session_id: session.clone(),
            verb: pb::Verb::Approve as i32,
            request_id: "0193a5b3-0000-7000-8000-00000000a001".into(),
            request_hash: signing::request_hash(&request).to_vec(),
            ..Default::default()
        }),
    );
    assert_eq!(hex(&approve.encode_to_vec()), vector(&v, "order_approve")["payload"].as_str().unwrap());

    let release = order("0193a5b3-0000-7000-8000-00000000b004", 1795000001000, 1795003600, 1795000300, pb::order::What::Hold(pb::Hold { on: false }));
    assert_eq!(hex(&release.encode_to_vec()), vector(&v, "order_release")["payload"].as_str().unwrap());

    let approval = pb::AttentionRequest {
        runtime_id: rt.clone(),
        session_id: session.clone(),
        request_id: "0193a5b3-0000-7000-8000-00000000a001".into(),
        kind: pb::AttentionKind::Approval as i32,
        text: "./release.sh --notarize".into(),
        tool: "Bash".into(),
        guardrail_id: "release".into(),
        guardrail_label: "Release and deploy scripts".into(),
        turn_id: "0193a5b3-0000-7000-8000-00000000c001".into(),
        raised_at: 1795000000,
        expires_at: 1795000540,
        ..Default::default()
    };
    assert_eq!(approval.encode_to_vec(), request);

    let settings = pb::Settings::decode(&unhex(vector(&v, "settings_v1")["payload"].as_str().unwrap())[..]).unwrap();
    assert_eq!(settings.version, 1);
    assert_eq!(settings.limits, Some(pb::Limits::default()), "a set, empty Limits is written");
    assert_eq!(settings.guardrails[1].builtin, "delete_outside_worktree");
    assert_eq!(settings.fabrics[0].name, "runtime-channel");
}

#[test]
fn what_must_fail_does() {
    let v = vectors();
    let (_, device) = key(&v, "device");
    let fails = v["must_fail"].as_array().unwrap();
    let get = |name: &str| fails.iter().find(|x| x["name"] == name).unwrap();
    let pair = |x: &Value| (unhex(x["payload"].as_str().unwrap()), unhex(x["signature"].as_str().unwrap()));

    let (p, s) = pair(get("order_send_tampered"));
    assert_eq!(signing::verify(signing::ORDER, &device, &p, &s), Err(Refusal::BadSignature));
    let (p, s) = pair(get("order_send_wrong_context"));
    assert_eq!(signing::verify(signing::SETTINGS, &device, &p, &s), Err(Refusal::BadSignature));
    assert_eq!(signing::verify(signing::ORDER, &device, &p, &s), Ok(()));
    for name in ["order_send_not_canonical", "order_send_unknown_field"] {
        let (p, s) = pair(get(name));
        assert_eq!(signing::verify(signing::ORDER, &device, &p, &s), Ok(()), "{name}: validly signed");
        assert!(pb::Order::decode(&p[..]).is_ok(), "{name}: decodes");
        assert_eq!(signing::open::<pb::Order>(signing::ORDER, &device, &p, &s).err(), Some(Refusal::NonCanonical), "{name}");
    }
    let (p, s) = pair(&vector(&v, "order_send").clone());
    assert_eq!(signing::verify(signing::ORDER, &device[..64], &p, &s), Err(Refusal::BadPublicKey));
    assert_eq!(signing::verify(signing::ORDER, &device, &p, &s[..63]), Err(Refusal::BadSignature));
}

#[test]
fn the_pairing_seal_opens_to_a_signed_claim() {
    let v = vectors();
    let x = &v["pairing"];
    let secret = unhex(x["secret"].as_str().unwrap());
    let pairing_id = x["pairing_id"].as_str().unwrap();
    let runtime_id = x["runtime_id"].as_str().unwrap();
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(pairing_id.as_bytes()), &secret);
    let mut k = [0u8; 32];
    hk.expand(b"taisce/pairing/v1", &mut k).unwrap();
    assert_eq!(hex(&k), x["hkdf_key"].as_str().unwrap());

    let sealed = unhex(x["sealed"].as_str().unwrap());
    let (nonce, ct) = sealed.split_at(12);
    assert_eq!(hex(nonce), x["nonce"].as_str().unwrap());
    let cipher = Aes256Gcm::new_from_slice(&k).unwrap();
    let plain = cipher.decrypt(&Nonce::try_from(nonce).unwrap(), Payload { msg: ct, aad: runtime_id.as_bytes() }).unwrap();
    assert_eq!(hex(&plain), x["plaintext_seal"].as_str().unwrap());
    assert!(cipher.decrypt(&Nonce::try_from(nonce).unwrap(), Payload { msg: ct, aad: b"another runtime" }).is_err(), "the runtime id is bound");

    let seal = pb::PairingSeal::decode(&plain[..]).unwrap();
    let (_, device) = key(&v, "device");
    let claim: pb::PairingClaim = signing::open(signing::PAIRING, &device, &seal.claim, &seal.signature).unwrap();
    assert_eq!((claim.runtime_id.as_str(), claim.pairing_id.as_str(), claim.public_key), (runtime_id, pairing_id, device));

    let code = x["code"].as_str().unwrap();
    assert!(code.starts_with(&format!("taisce-pair:?v=1&r={runtime_id}&p={pairing_id}&k=")), "{code}");
}
