#!/usr/bin/env python3
"""Writes signing-vectors.json: test vectors for SIGNING and the pairing
seal in ../runtime.proto.

The protobuf bytes are encoded here by hand, field by field, from the rules
the .proto states (ascending field numbers, proto3 defaults left out, a set
oneof member or sub-message written even when empty), not by a protobuf
library: so the vectors check every implementation against the rules, not
against another library's habits. Signatures are deterministic (RFC 6979),
so running this again writes the same file. CryptoKit's signatures are
randomised and will differ; any valid one verifies.

    python3 make_vectors.py > signing-vectors.json

Needs the `cryptography` package (44 or later).
"""

import hashlib
import json

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature, encode_dss_signature
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

N = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551  # P-256 order

CONTEXT = {
    "order": b"taisce/order/v1\n",
    "settings": b"taisce/settings/v1\n",
    "attention": b"taisce/attention/v1\n",
    "pairing": b"taisce/pairing/v1\n",
}


# --- protobuf, by hand -------------------------------------------------------

def varint(n: int) -> bytes:
    if n < 0:
        n += 1 << 64  # int64: two's complement, ten bytes
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def key(field: int, wire: int) -> bytes:
    return varint(field << 3 | wire)


def v(field: int, n: int) -> bytes:
    """A varint field (uint64, int64, enum, bool); left out at 0."""
    return key(field, 0) + varint(int(n)) if n else b""


def b(field: int, data) -> bytes:
    """A string or bytes field; left out when empty."""
    if isinstance(data, str):
        data = data.encode()
    return key(field, 2) + varint(len(data)) + data if data else b""


def m(field: int, body: bytes) -> bytes:
    """A set sub-message or oneof member: written even when empty."""
    return key(field, 2) + varint(len(body)) + body


# --- keys ---------------------------------------------------------------------

def private_key(label: str) -> ec.EllipticCurvePrivateKey:
    d = int.from_bytes(hashlib.sha256(label.encode()).digest(), "big") % N
    return ec.derive_private_key(d, ec.SECP256R1())


def public_x963(k: ec.EllipticCurvePrivateKey) -> bytes:
    return k.public_key().public_bytes(Encoding.X962, PublicFormat.UncompressedPoint)


def device_id(pub: bytes) -> str:
    return hashlib.sha256(pub).hexdigest()


def fingerprint(did: str) -> str:
    return " ".join(did[i : i + 4] for i in range(0, 16, 4))


def sign(k: ec.EllipticCurvePrivateKey, context: bytes, payload: bytes) -> bytes:
    der = k.sign(context + payload, ec.ECDSA(hashes.SHA256(), deterministic_signing=True))
    r, s = decode_dss_signature(der)
    return r.to_bytes(32, "big") + s.to_bytes(32, "big")


def flip_s(sig: bytes) -> bytes:
    """The same signature with s replaced by n - s: also valid."""
    r, s = sig[:32], int.from_bytes(sig[32:], "big")
    return r + (N - s).to_bytes(32, "big")


def verifies(k: ec.EllipticCurvePrivateKey, context: bytes, payload: bytes, sig: bytes) -> bool:
    der = encode_dss_signature(int.from_bytes(sig[:32], "big"), int.from_bytes(sig[32:], "big"))
    try:
        k.public_key().verify(der, context + payload, ec.ECDSA(hashes.SHA256()))
        return True
    except Exception:
        return False


# --- the vectors --------------------------------------------------------------

RUNTIME_ID = "0193a5b2-7c4d-7e8f-9a0b-1c2d3e4f5a6b"
SESSION_ID = "0193a5b2-8d9e-7f01-a2b3-c4d5e6f70819"

device = private_key("taisce test vector: device key")
runtime = private_key("taisce test vector: runtime key")
DEVICE_PUB = public_x963(device)
RUNTIME_PUB = public_x963(runtime)
DEVICE_ID = device_id(DEVICE_PUB)


def keys():
    return {
        "device": {
            "private_scalar": format(device.private_numbers().private_value, "064x"),
            "public_key": DEVICE_PUB.hex(),
            "device_id": DEVICE_ID,
            "fingerprint": fingerprint(DEVICE_ID),
        },
        "runtime": {
            "private_scalar": format(runtime.private_numbers().private_value, "064x"),
            "public_key": RUNTIME_PUB.hex(),
            "device_id": device_id(RUNTIME_PUB),
        },
    }


def signed(name, kind, k, fields, payload, note):
    sig = sign(k, CONTEXT[kind], payload)
    assert verifies(k, CONTEXT[kind], payload, sig) and verifies(k, CONTEXT[kind], payload, flip_s(sig))
    return {
        "name": name,
        "kind": kind,
        "note": note,
        "signer": "device" if k is device else "runtime",
        "fields": fields,
        "payload": payload.hex(),
        "signing_input": (CONTEXT[kind] + payload).hex(),
        "signature": sig.hex(),
        "signature_other_s": flip_s(sig).hex(),
    }


def attention_request():
    fields = {
        "runtime_id": RUNTIME_ID,
        "session_id": SESSION_ID,
        "request_id": "0193a5b3-0000-7000-8000-00000000a001",
        "kind": "ATTENTION_KIND_APPROVAL (1)",
        "text": "./release.sh --notarize",
        "tool": "Bash",
        "guardrail_id": "release",
        "guardrail_label": "Release and deploy scripts",
        "turn_id": "0193a5b3-0000-7000-8000-00000000c001",
        "raised_at": 1795000000,
        "expires_at": 1795000540,
    }
    payload = (
        b(1, RUNTIME_ID) + b(2, SESSION_ID) + b(3, fields["request_id"]) + v(4, 1) + b(5, fields["text"])
        + b(7, "Bash") + b(8, "release") + b(9, fields["guardrail_label"]) + b(10, fields["turn_id"])
        + v(11, 1795000000) + v(12, 1795000540)
    )
    return fields, payload


def question_request():
    fields = {
        "runtime_id": RUNTIME_ID,
        "session_id": SESSION_ID,
        "request_id": "0193a5b3-0000-7000-8000-00000000a002",
        "kind": "ATTENTION_KIND_QUESTION (2)",
        "text": "The retry test is flaky on CI. Fix the test, or skip it and file a ticket?",
        "choices": [{"id": "fix", "label": "Fix the test"}, {"id": "skip", "label": "Skip it, file a ticket"}],
        "raised_at": 1795000100,
    }
    choices = b"".join(m(6, b(1, c["id"]) + b(2, c["label"])) for c in fields["choices"])
    payload = (
        b(1, RUNTIME_ID) + b(2, SESSION_ID) + b(3, fields["request_id"]) + v(4, 2) + b(5, fields["text"])
        + choices + v(11, 1795000100)
    )
    return fields, payload


def order(order_id, counter, what_field, what_body, expires_at=1795003600, issued_at=1795000000):
    return (
        b(1, RUNTIME_ID) + v(2, counter) + v(3, expires_at) + m(what_field, what_body) + b(9, order_id)
        + v(10, issued_at)
    )


def main():
    out = {
        "about": "Test vectors for SIGNING and the pairing seal in proto/taisce/runtime/v1/runtime.proto. "
        "Written by make_vectors.py, which encodes protobuf by hand from the rules the .proto states. "
        "Every 'payload' is the canonical encoding of 'fields'; 'signing_input' is CONTEXT || payload; "
        "'signature' (r || s, deterministic RFC 6979) and 'signature_other_s' (n - s) both verify against "
        "the signer's public key. Integers in 'fields' are as encoded; enums are given by name and number.",
        "keys": keys(),
        "vectors": [],
        "must_fail": [],
    }
    vec = out["vectors"]

    # 1. a reply typed on the phone
    send = {
        "session_id": SESSION_ID,
        "verb": "VERB_SEND (1)",
        "text": "good, now write the isolation test",
        "after_turn": "0193a5b3-0000-7000-8000-00000000c001",
    }
    cmd = b(1, SESSION_ID) + v(2, 1) + b(3, send["text"]) + b(4, send["after_turn"])
    order_id = "0193a5b3-0000-7000-8000-00000000b001"
    payload = order(order_id, 1795000000123, 4, cmd)
    vec.append(signed(
        "order_send", "order", device,
        {"runtime_id": RUNTIME_ID, "counter": 1795000000123, "expires_at": 1795003600, "command": send,
         "order_id": order_id, "issued_at": 1795000000},
        payload,
        "Order{command: SEND}: the oneof member (4) sits between expires_at (3) and order_id (9).",
    ))
    send_payload = payload

    # 2. the approval request a runtime raises, and the phone's approval of it
    fields, request = attention_request()
    vec.append(signed(
        "attention_approval", "attention", runtime, fields, request,
        "AttentionRequest, signed by the runtime. request_hash below is SHA-256 of this payload.",
    ))
    request_hash = hashlib.sha256(request).digest()
    approve = {
        "session_id": SESSION_ID,
        "verb": "VERB_APPROVE (3)",
        "request_id": fields["request_id"],
        "request_hash": request_hash.hex(),
    }
    cmd = b(1, SESSION_ID) + v(2, 3) + b(6, fields["request_id"]) + b(7, request_hash)
    order_id = "0193a5b3-0000-7000-8000-00000000b002"
    vec.append(signed(
        "order_approve", "order", device,
        {"runtime_id": RUNTIME_ID, "counter": 1795000000456, "expires_at": 1795000540, "command": approve,
         "order_id": order_id, "issued_at": 1795000030},
        order(order_id, 1795000000456, 4, cmd, expires_at=1795000540, issued_at=1795000030),
        "Order{command: APPROVE}: request_hash binds the exact AttentionRequest bytes of 'attention_approval'.",
    ))

    # 3. a question with choices, and the answer tapped
    fields, request = question_request()
    vec.append(signed(
        "attention_question", "attention", runtime, fields, request,
        "AttentionRequest{kind: QUESTION} with two choices: repeated sub-messages, field 6, in order.",
    ))
    answer = {
        "session_id": SESSION_ID,
        "verb": "VERB_ANSWER (8)",
        "text": "Fix the test",
        "request_id": fields["request_id"],
        "request_hash": hashlib.sha256(request).hexdigest(),
        "choice_id": "fix",
    }
    cmd = (b(1, SESSION_ID) + v(2, 8) + b(3, "Fix the test") + b(6, fields["request_id"])
           + b(7, hashlib.sha256(request).digest()) + b(8, "fix"))
    order_id = "0193a5b3-0000-7000-8000-00000000b003"
    vec.append(signed(
        "order_answer", "order", device,
        {"runtime_id": RUNTIME_ID, "counter": 1795000000789, "expires_at": 1795003600, "command": answer,
         "order_id": order_id, "issued_at": 1795000200},
        order(order_id, 1795000000789, 4, cmd, issued_at=1795000200),
        "Order{command: ANSWER}.",
    ))

    # 4. release Hold all: Hold{on: false} is an empty message, written anyway
    order_id = "0193a5b3-0000-7000-8000-00000000b004"
    vec.append(signed(
        "order_release", "order", device,
        {"runtime_id": RUNTIME_ID, "counter": 1795000001000, "expires_at": 1795003600, "hold": {"on": False},
         "order_id": order_id, "issued_at": 1795000300},
        order(order_id, 1795000001000, 7, b"", issued_at=1795000300),
        "Order{hold: {on: false}}: the oneof member is set and empty, so it is written as 3a 00.",
    ))

    # 5. reorder a queue: repeated strings are never packed
    reorder = {
        "session_id": SESSION_ID,
        "verb": "VERB_REORDER (10)",
        "order_ids": ["0193a5b3-0000-7000-8000-00000000b011", "0193a5b3-0000-7000-8000-00000000b010"],
    }
    cmd = b(1, SESSION_ID) + v(2, 10) + b"".join(b(9, x) for x in reorder["order_ids"])
    order_id = "0193a5b3-0000-7000-8000-00000000b005"
    vec.append(signed(
        "order_reorder", "order", device,
        {"runtime_id": RUNTIME_ID, "counter": 1795000001234, "expires_at": 1795003600, "command": reorder,
         "order_id": order_id, "issued_at": 1795000400},
        order(order_id, 1795000001234, 4, cmd, issued_at=1795000400),
        "Order{command: REORDER}: order_ids is field 9, once per id.",
    ))

    # 6. settings: guardrails, limits left at their defaults, one fabric
    s = {
        "version": 1,
        "issued_at": 1795000500,
        "guardrails": [
            {"id": "force-push", "label": "Force push", "enabled": True, "tools": ["Bash"],
             "pattern": "\\bgit\\s+push\\b.*(\\s--force(-with-lease)?\\b|\\s-f\\b|\\s\\+\\S)"},
            {"id": "delete-outside-worktree", "label": "Delete outside the worktree", "enabled": True,
             "builtin": "delete_outside_worktree"},
        ],
        "limits": {},
        "fabrics": [
            {"id": "0193a5b3-0000-7000-8000-00000000f001", "name": "runtime-channel",
             "purpose": "Workbox and Taisce building the connection between them.",
             "doc_id": "01a1f841-777d-7342-96fc-254a64999325", "doc_title": "Runtime Channel"},
        ],
    }
    g = s["guardrails"]
    rails = (m(3, b(1, g[0]["id"]) + b(2, g[0]["label"]) + v(3, 1) + b(4, "Bash") + b(5, g[0]["pattern"]))
             + m(3, b(1, g[1]["id"]) + b(2, g[1]["label"]) + v(3, 1) + b(6, g[1]["builtin"])))
    f = s["fabrics"][0]
    fabrics = m(5, b(1, f["id"]) + b(2, f["name"]) + b(3, f["purpose"]) + b(4, f["doc_id"]) + b(5, f["doc_title"]))
    payload = v(1, 1) + v(2, 1795000500) + rails + m(4, b"") + fabrics
    vec.append(signed(
        "settings_v1", "settings", device, s, payload,
        "Settings: limits is set and empty (every limit at its default), so it is written as 22 00.",
    ))

    # 7. pairing: the claim, its signature, the seal
    secret = hashlib.sha256(b"taisce test vector: pairing secret").digest()
    pairing_id = "0193a5b3-0000-7000-8000-00000000d001"
    claim_fields = {"runtime_id": RUNTIME_ID, "pairing_id": pairing_id, "public_key": DEVICE_PUB.hex(),
                    "device_name": "Test iPhone", "created_at": 1795000600}
    claim = b(1, RUNTIME_ID) + b(2, pairing_id) + b(3, DEVICE_PUB) + b(4, "Test iPhone") + v(5, 1795000600)
    claim_vec = signed("pairing_claim", "pairing", device, claim_fields, claim,
                       "PairingClaim, signed by the device it names.")
    vec.append(claim_vec)
    seal = b(1, claim) + b(2, bytes.fromhex(claim_vec["signature"]))
    hk = HKDF(algorithm=hashes.SHA256(), length=32, salt=pairing_id.encode(), info=b"taisce/pairing/v1")
    k = hk.derive(secret)
    nonce = bytes(range(12))
    sealed = nonce + AESGCM(k).encrypt(nonce, seal, RUNTIME_ID.encode())
    b64 = lambda x: __import__("base64").urlsafe_b64encode(x).rstrip(b"=").decode()
    out["pairing"] = {
        "note": "The seal a phone sends for the code below. The nonce is fixed here only to make the vector "
        "repeatable; a real seal uses a random one.",
        "code": f"taisce-pair:?v=1&r={RUNTIME_ID}&p={pairing_id}&k={b64(RUNTIME_PUB)}&s={b64(secret)}&h=taisce.example",
        "secret": secret.hex(),
        "runtime_id": RUNTIME_ID,
        "pairing_id": pairing_id,
        "hkdf_key": k.hex(),
        "nonce": nonce.hex(),
        "plaintext_seal": seal.hex(),
        "sealed": sealed.hex(),
    }

    # what must be refused
    bad = bytearray(send_payload)
    bad[-1] ^= 1  # issued_at changed by one second
    out["must_fail"].append({
        "name": "order_send_tampered",
        "why": "bad_signature: one bit of the payload of order_send flipped, its signature kept",
        "payload": bytes(bad).hex(),
        "signature": vec[0]["signature"],
    })
    out["must_fail"].append({
        "name": "order_send_wrong_context",
        "why": "bad_signature: order_send's signature checked under the Settings context",
        "context": CONTEXT["settings"].decode().replace("\n", "\\n"),
        "payload": send_payload.hex(),
        "signature": vec[0]["signature"],
    })
    # the same fields, order_id written before runtime_id: decodes, but is
    # not canonical (and so is not what a signer produces)
    oid = b(9, "0193a5b3-0000-7000-8000-00000000b001")
    reordered = oid + send_payload.replace(oid, b"")
    assert reordered != send_payload and len(reordered) == len(send_payload)
    out["must_fail"].append({
        "name": "order_send_not_canonical",
        "why": "non_canonical: order_send's fields with order_id (9) written first; validly signed, refused "
        "by the hub because it does not re-encode to the same bytes",
        "payload": reordered.hex(),
        "signature": sign(device, CONTEXT["order"], reordered).hex(),
    })
    # an unknown field (99, a varint) appended to a validly signed order
    unknown = send_payload + v(99, 1)
    out["must_fail"].append({
        "name": "order_send_unknown_field",
        "why": "unsupported: order_send with an unknown field 99 appended, validly signed",
        "payload": unknown.hex(),
        "signature": sign(device, CONTEXT["order"], unknown).hex(),
    })
    print(json.dumps(out, indent=2))


if __name__ == "__main__":
    main()
