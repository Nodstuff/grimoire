//! SIGNING, as `runtime.proto` states it: what a paired device signs (an
//! Order, a Settings), what a runtime signs (an AttentionRequest) and what a
//! device signs to pair (a PairingClaim). ECDSA over P-256 with SHA-256, over
//! `CONTEXT || payload`; signatures are 64 bytes `r || s`; public keys are 65
//! bytes `0x04 || X || Y`. `proto/taisce/runtime/v1/testdata/
//! signing-vectors.json` holds test vectors, checked by this crate's tests.
//!
//! The hub checks an order before it queues it, as a courtesy: the runtime
//! checks again against the keys it was given at the Mac, and only that
//! check is the one that matters.

use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use prost::Message;
use sha2::{Digest, Sha256};

/// What precedes the payload in the signed bytes, per kind. Each ends in a
/// newline, so none is a prefix of another.
pub const ORDER: &[u8] = b"taisce/order/v1\n";
pub const SETTINGS: &[u8] = b"taisce/settings/v1\n";
pub const ATTENTION: &[u8] = b"taisce/attention/v1\n";
pub const PAIRING: &[u8] = b"taisce/pairing/v1\n";

/// The largest signed payload taken.
pub const MAX_PAYLOAD: usize = 64 * 1024;
/// An X9.63 uncompressed P-256 public key.
pub const PUBLIC_KEY_LEN: usize = 65;
/// `r || s`.
pub const SIGNATURE_LEN: usize = 64;

/// Why a signed payload was refused. The names are the `reason` codes a
/// Result and the hub's answers use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not 65 bytes, or not a point on the curve.
    BadPublicKey,
    /// Not 64 bytes, or does not verify.
    BadSignature,
    /// Over [`MAX_PAYLOAD`], or not a protobuf message of the expected type.
    Malformed,
    /// Decodes, but is not the canonical encoding: fields out of order, a
    /// default written, or a field this build does not know.
    NonCanonical,
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::BadPublicKey => "bad_public_key",
            Refusal::BadSignature => "bad_signature",
            Refusal::Malformed => "malformed",
            Refusal::NonCanonical => "non_canonical",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for Refusal {}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A device's id: lowercase hex of SHA-256 over its 65-byte public key.
pub fn device_id(public_key: &[u8]) -> String {
    hex(&Sha256::digest(public_key))
}

/// What people compare at pairing: the device id's first 16 characters in
/// groups of four.
pub fn fingerprint(device_id: &str) -> String {
    let head = device_id.get(..16).unwrap_or(device_id);
    head.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// SHA-256 of an AttentionRequest's bytes: what an approve, deny or answer
/// names as `request_hash`.
pub fn request_hash(request: &[u8]) -> [u8; 32] {
    Sha256::digest(request).into()
}

/// Does `signature` sign `context || payload` under `public_key`? Either s
/// is accepted.
pub fn verify(context: &[u8], public_key: &[u8], payload: &[u8], signature: &[u8]) -> Result<(), Refusal> {
    if public_key.len() != PUBLIC_KEY_LEN {
        return Err(Refusal::BadPublicKey);
    }
    let key = VerifyingKey::from_sec1_bytes(public_key).map_err(|_| Refusal::BadPublicKey)?;
    if signature.len() != SIGNATURE_LEN {
        return Err(Refusal::BadSignature);
    }
    let sig = Signature::from_slice(signature).map_err(|_| Refusal::BadSignature)?;
    // a high s is as valid as a low one: nothing keys on the signature
    let sig = sig.normalize_s();
    let mut message = Vec::with_capacity(context.len() + payload.len());
    message.extend_from_slice(context);
    message.extend_from_slice(payload);
    key.verify(&message, &sig).map_err(|_| Refusal::BadSignature)
}

/// Decode a signed payload, refusing one that is not the canonical encoding
/// of what it decodes to. That also refuses a field this build does not
/// know: prost drops it on decode, so the re-encoding is shorter.
pub fn decode_canonical<M: Message + Default>(payload: &[u8]) -> Result<M, Refusal> {
    if payload.len() > MAX_PAYLOAD {
        return Err(Refusal::Malformed);
    }
    let message = M::decode(payload).map_err(|_| Refusal::Malformed)?;
    if message.encode_to_vec() != payload {
        return Err(Refusal::NonCanonical);
    }
    Ok(message)
}

/// Verify, then decode canonically: what the hub does with a signed order
/// or settings before it queues them.
pub fn open<M: Message + Default>(context: &[u8], public_key: &[u8], payload: &[u8], signature: &[u8]) -> Result<M, Refusal> {
    if payload.len() > MAX_PAYLOAD {
        return Err(Refusal::Malformed);
    }
    verify(context, public_key, payload, signature)?;
    decode_canonical(payload)
}
