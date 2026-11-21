import CryptoKit
import Foundation
import Testing
@testable import TaisceKit

/// SIGNING (runtime.proto) against `proto/taisce/runtime/v1/testdata/signing-vectors.json`,
/// made by an independent hand encoder and checked there against prost and
/// p256: the encoder must reproduce every payload byte for byte, the
/// verifier must take both s values and refuse every must_fail case, and
/// the pairing seal must match with the vector's fixed nonce.
@Suite struct FabricSigningTests {
    static let contract = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .appending(path: "proto/taisce/runtime/v1")

    struct Vectors: Decodable {
        struct Key: Decodable {
            var private_scalar: String
            var public_key: String
            var device_id: String
            var fingerprint: String?
        }

        struct Vector: Decodable {
            var name: String
            var kind: String
            var signer: String
            var payload: String
            var signing_input: String
            var signature: String
            var signature_other_s: String
        }

        struct Fail: Decodable {
            var name: String
            var context: String?
            var payload: String
            var signature: String
        }

        struct Pairing: Decodable {
            var code: String
            var secret: String
            var runtime_id: String
            var pairing_id: String
            var hkdf_key: String
            var nonce: String
            var plaintext_seal: String
            var sealed: String
        }

        var keys: [String: Key]
        var vectors: [Vector]
        var must_fail: [Fail]
        var pairing: Pairing
    }

    static let vectors: Vectors = {
        let data = try! Data(contentsOf: contract.appending(path: "testdata/signing-vectors.json"))
        return try! JSONDecoder().decode(Vectors.self, from: data)
    }()

    static func hex(_ s: String) -> Data { Data(Hex.bytes(s)) }
    static func vector(_ name: String) -> Vectors.Vector { vectors.vectors.first { $0.name == name }! }

    static let runtime = "0193a5b2-7c4d-7e8f-9a0b-1c2d3e4f5a6b"
    static let session = "0193a5b2-8d9e-7f01-a2b3-c4d5e6f70819"

    /// The messages the vectors' `fields` describe, built with the app's types.
    static var built: [String: Data] {
        let attentionApproval = AttentionRequest(
            runtimeID: runtime, sessionID: session, requestID: "0193a5b3-0000-7000-8000-00000000a001", kind: .approval,
            text: "./release.sh --notarize", tool: "Bash", guardrailID: "release", guardrailLabel: "Release and deploy scripts",
            turnID: "0193a5b3-0000-7000-8000-00000000c001", raisedAt: 1_795_000_000, expiresAt: 1_795_000_540)
        let attentionQuestion = AttentionRequest(
            runtimeID: runtime, sessionID: session, requestID: "0193a5b3-0000-7000-8000-00000000a002", kind: .question,
            text: "The retry test is flaky on CI. Fix the test, or skip it and file a ticket?",
            choices: [FabricChoice(id: "fix", label: "Fix the test"), FabricChoice(id: "skip", label: "Skip it, file a ticket")],
            turnID: questionFields["turn_id"] as? String ?? "", raisedAt: Int64(questionFields["raised_at"] as? Int ?? 0),
            expiresAt: Int64(questionFields["expires_at"] as? Int ?? 0))
        let settingsFields = fields("settings_v1")
        let fabrics = (settingsFields["fabrics"] as? [[String: Any]] ?? []).map {
            FabricDefinition(id: $0["id"] as? String ?? "", name: $0["name"] as? String ?? "", purpose: $0["purpose"] as? String ?? "",
                             docID: $0["doc_id"] as? String ?? "", docTitle: $0["doc_title"] as? String ?? "")
        }
        let orderAnswer = fields("order_answer")
        let orderReorder = fields("order_reorder")
        let orderRelease = fields("order_release")
        let orderApprove = fields("order_approve")
        return [
            "order_send": FabricOrder(
                runtimeID: runtime, counter: 1_795_000_000_123, expiresAt: 1_795_003_600,
                what: .command(FabricCommand(sessionID: session, verb: .send, text: "good, now write the isolation test",
                                             afterTurn: "0193a5b3-0000-7000-8000-00000000c001")),
                orderID: "0193a5b3-0000-7000-8000-00000000b001", issuedAt: 1_795_000_000).encoded(),
            "attention_approval": attentionApproval.encoded(),
            "order_approve": FabricOrder(
                runtimeID: runtime, counter: 1_795_000_000_456, expiresAt: 1_795_000_540,
                what: .command(FabricCommand(sessionID: session, verb: .approve, requestID: "0193a5b3-0000-7000-8000-00000000a001",
                                             requestHash: FabricSignature.requestHash(attentionApproval.encoded()))),
                orderID: orderApprove["order_id"] as! String, issuedAt: Int64(orderApprove["issued_at"] as! Int)).encoded(),
            "attention_question": attentionQuestion.encoded(),
            "order_answer": FabricOrder(
                runtimeID: runtime, counter: 1_795_000_000_789, expiresAt: 1_795_003_600,
                what: .command(FabricCommand(sessionID: session, verb: .answer, text: "Fix the test", requestID: "0193a5b3-0000-7000-8000-00000000a002",
                                             requestHash: FabricSignature.requestHash(attentionQuestion.encoded()), choiceID: "fix")),
                orderID: orderAnswer["order_id"] as! String, issuedAt: Int64(orderAnswer["issued_at"] as! Int)).encoded(),
            "order_release": FabricOrder(
                runtimeID: runtime, counter: 1_795_000_001_000, expiresAt: 1_795_003_600, what: .hold(false),
                orderID: orderRelease["order_id"] as! String, issuedAt: 1_795_000_300).encoded(),
            "order_reorder": FabricOrder(
                runtimeID: runtime, counter: 1_795_000_001_234, expiresAt: 1_795_003_600,
                what: .command(FabricCommand(sessionID: session, verb: .reorder,
                                             orderIDs: ["0193a5b3-0000-7000-8000-00000000b011", "0193a5b3-0000-7000-8000-00000000b010"])),
                orderID: orderReorder["order_id"] as! String, issuedAt: 1_795_000_400).encoded(),
            "settings_v1": FabricSettings(
                version: 1, issuedAt: 1_795_000_500,
                guardrails: [
                    FabricGuardrail(id: "force-push", label: "Force push", tools: ["Bash"], pattern: #"\bgit\s+push\b.*(\s--force(-with-lease)?\b|\s-f\b|\s\+\S)"#),
                    FabricGuardrail(id: "delete-outside-worktree", label: "Delete outside the worktree", tools: [], builtin: "delete_outside_worktree"),
                ],
                limits: FabricLimits(), fabrics: fabrics).encoded(),
            "pairing_claim": PairingClaim(runtimeID: runtime, pairingID: "0193a5b3-0000-7000-8000-00000000d001",
                                          publicKey: hex(vectors.keys["device"]!.public_key), deviceName: "Test iPhone", createdAt: 1_795_000_600).encoded(),
        ]
    }

    static var questionFields: [String: Any] { fields("attention_question") }

    /// The vector's `fields`, read loosely (only to copy values that are not worth retyping).
    static func fields(_ name: String) -> [String: Any] {
        let data = try! Data(contentsOf: contract.appending(path: "testdata/signing-vectors.json"))
        let root = try! JSONSerialization.jsonObject(with: data) as! [String: Any]
        let v = (root["vectors"] as! [[String: Any]]).first { $0["name"] as? String == name }!
        return v["fields"] as! [String: Any]
    }

    static func context(_ kind: String) -> SigningContext {
        switch kind {
        case "order": .order
        case "settings": .settings
        case "attention": .attention
        default: .pairing
        }
    }

    @Test func theEncoderReproducesEveryVectorByteForByte() throws {
        let built = Self.built
        #expect(Set(built.keys) == Set(Self.vectors.vectors.map(\.name)), "every vector is covered")
        for v in Self.vectors.vectors {
            let payload = try #require(built[v.name])
            #expect(payload.hex == v.payload, "\(v.name)")
            #expect(Self.context(v.kind).input(payload).hex == v.signing_input, "\(v.name)")
        }
    }

    @Test func requestHashesBindTheExactRequestBytes() {
        let approval = Self.hex(Self.vector("attention_approval").payload)
        #expect(FabricSignature.requestHash(approval).hex == "90b9e04137c830c253f697eec0644ed704079e67f31d45e72d106988e944e6b1")
        let question = Self.hex(Self.vector("attention_question").payload)
        #expect(FabricSignature.requestHash(question).hex == "eb7f41b92ee7199beac6df201d334adf11850c076d200dac07126066796819ec")
    }

    @Test func bothSignaturesOfEveryVectorVerify() {
        for v in Self.vectors.vectors {
            let key = Self.hex(Self.vectors.keys[v.signer]!.public_key)
            let payload = Self.hex(v.payload)
            #expect(FabricSignature.verify(publicKey: key, context: Self.context(v.kind), payload: payload, signature: Self.hex(v.signature)), "\(v.name)")
            #expect(FabricSignature.verify(publicKey: key, context: Self.context(v.kind), payload: payload, signature: Self.hex(v.signature_other_s)), "\(v.name), other s")
            // never under another kind's label
            for other in SigningContext.allCases where other != Self.context(v.kind) {
                #expect(!FabricSignature.verify(publicKey: key, context: other, payload: payload, signature: Self.hex(v.signature)), "\(v.name) as \(other)")
            }
        }
    }

    @Test func theMustFailCasesFail() throws {
        let device = Self.hex(Self.vectors.keys["device"]!.public_key)
        for f in Self.vectors.must_fail {
            let payload = Self.hex(f.payload)
            let context = f.context.map { SigningContext(rawValue: $0.replacingOccurrences(of: "\\n", with: "\n"))! } ?? .order
            let verifies = FabricSignature.verify(publicKey: device, context: context, payload: payload, signature: Self.hex(f.signature))
            switch f.name {
            case "order_send_tampered", "order_send_wrong_context":
                #expect(!verifies, "\(f.name): bad_signature")
            case "order_send_not_canonical":
                #expect(verifies, "validly signed")
                #expect(!FabricOrder.isCanonical(payload), "non_canonical")
            case "order_send_unknown_field":
                #expect(verifies, "validly signed")
                #expect(throws: ProtoError.unknownField(99)) { try FabricOrder.decode(payload) }
                #expect(!FabricOrder.isCanonical(payload))
            default:
                Issue.record("a must_fail case this test does not know: \(f.name)")
            }
        }
        // and every good order is canonical
        for v in Self.vectors.vectors where v.kind == "order" {
            #expect(FabricOrder.isCanonical(Self.hex(v.payload)), "\(v.name)")
        }
    }

    @Test func attentionRequestsDecodeStrictlyFromTheSignedBytes() throws {
        let question = try AttentionRequest.decode(Self.hex(Self.vector("attention_question").payload))
        #expect(question.kind == .question && question.choices.map(\.id) == ["fix", "skip"])
        #expect(question.choices.map(\.label) == ["Fix the test", "Skip it, file a ticket"])
        let approval = try AttentionRequest.decode(Self.hex(Self.vector("attention_approval").payload))
        #expect(approval.text == "./release.sh --notarize" && approval.tool == "Bash" && approval.guardrailLabel == "Release and deploy scripts")
        #expect(approval.expiresAt == 1_795_000_540 && approval.runtimeID == Self.runtime && approval.sessionID == Self.session)
        // a field this build does not know: refused, not ignored
        var extra = Self.hex(Self.vector("attention_approval").payload)
        extra.append(contentsOf: [0x98, 0x06, 0x01])
        #expect(throws: ProtoError.unknownField(99)) { try AttentionRequest.decode(extra) }
        #expect(throws: ProtoError.truncated) { try AttentionRequest.decode(Self.hex(Self.vector("attention_approval").payload).dropLast(3)) }
    }

    @Test func deviceIDsAndFingerprints() {
        for (_, key) in Self.vectors.keys {
            #expect(FabricSignature.deviceID(publicKey: Self.hex(key.public_key)) == key.device_id)
            if let fp = key.fingerprint { #expect(FabricSignature.fingerprint(deviceID: key.device_id) == fp) }
        }
        let software = SoftwareDeviceKey(try! P256.Signing.PrivateKey(rawRepresentation: Self.hex(Self.vectors.keys["device"]!.private_scalar)))
        #expect(software.publicKey.hex == Self.vectors.keys["device"]!.public_key)
        #expect(software.fingerprint == "55b3 96d9 92bf d94c")
    }

    @Test func whatTheAppSignsVerifies() throws {
        let key = SoftwareDeviceKey(try P256.Signing.PrivateKey(rawRepresentation: Self.hex(Self.vectors.keys["device"]!.private_scalar)))
        let payload = try #require(Self.built["order_send"])
        let sig = try key.sign(.order, payload)
        #expect(sig.count == 64)
        #expect(FabricSignature.verify(publicKey: key.publicKey, context: .order, payload: payload, signature: sig))
        #expect(FabricSignature.verify(publicKey: key.publicKey, context: .order, payload: payload, signature: FabricSignature.lowS(sig)))
        #expect(!FabricSignature.verify(publicKey: key.publicKey, context: .settings, payload: payload, signature: sig))
    }

    @Test func thePairingCodeParsesAndTheSealMatches() throws {
        let p = Self.vectors.pairing
        let code = try PairingCode(p.code)
        #expect(code.runtimeID == p.runtime_id && code.pairingID == p.pairing_id && code.host == "taisce.example")
        #expect(code.secret.hex == p.secret)
        #expect(code.runtimeKey.hex == Self.vectors.keys["runtime"]!.public_key)
        #expect(code.sealKey.withUnsafeBytes { Data($0) }.hex == p.hkdf_key)
        let key = SoftwareDeviceKey(try P256.Signing.PrivateKey(rawRepresentation: Self.hex(Self.vectors.keys["device"]!.private_scalar)))
        // the claim inside is what the vector signs; ECDSA here is randomised, so
        // the seal is compared by opening it and checking what is inside
        let sealed = try code.seal(with: key, deviceName: "Test iPhone", now: Date(timeIntervalSince1970: 1_795_000_600),
                                   nonce: try AES.GCM.Nonce(data: Self.hex(p.nonce)))
        let box = try AES.GCM.SealedBox(combined: sealed)
        let plain = try AES.GCM.open(box, using: code.sealKey, authenticating: Data(p.runtime_id.utf8))
        let vectorPlain = Self.hex(p.plaintext_seal)
        // the same claim bytes (field 1: tag, two length bytes, the claim) as the vector's
        let claim = Self.hex(Self.vector("pairing_claim").payload)
        #expect(plain.prefix(3 + claim.count) == vectorPlain.prefix(3 + claim.count), "PairingSeal.claim, byte for byte")
        #expect(Data(plain.dropFirst(3).prefix(claim.count)) == claim)
        // the vector's own seal opens with our key, and the vector's plaintext
        // sealed with its nonce is the vector's sealed bytes
        let theirs = try AES.GCM.SealedBox(combined: Self.hex(p.sealed))
        #expect(try AES.GCM.open(theirs, using: code.sealKey, authenticating: Data(p.runtime_id.utf8)) == vectorPlain)
        let again = try AES.GCM.seal(vectorPlain, using: code.sealKey, nonce: try AES.GCM.Nonce(data: Self.hex(p.nonce)), authenticating: Data(p.runtime_id.utf8))
        #expect(again.combined?.hex == p.sealed)
        // and the signature inside verifies over the claim
        #expect(FabricSignature.verify(publicKey: key.publicKey, context: .pairing, payload: Self.hex(Self.vector("pairing_claim").payload),
                                       signature: Data(plain.suffix(64))))
    }

    @Test func badPairingCodesAreRefused() {
        #expect(throws: PairingCode.ParseError.notAPairingCode) { try PairingCode("https://example.com") }
        #expect(throws: PairingCode.ParseError.unsupportedVersion) { try PairingCode("taisce-pair:?v=2&r=x") }
        let good = Self.vectors.pairing.code
        #expect(throws: PairingCode.ParseError.malformed("k")) { try PairingCode(good.replacingOccurrences(of: "&k=BPgj", with: "&k=BPgk")) }
        #expect(throws: PairingCode.ParseError.missing("s")) { try PairingCode(good.replacingOccurrences(of: "&s=", with: "&x=")) }
    }

    @Test func theEmbeddedDefaultsAreTheContractsAndEncode() throws {
        let embedded = try Data(contentsOf: try #require(Bundle.module.url(forResource: "default-settings", withExtension: "json")))
        let contract = try Data(contentsOf: Self.contract.appending(path: "default-settings.json"))
        #expect(embedded == contract, "Sources/TaisceKit/Fabric/Resources/default-settings.json is a copy of the contract's")
        let d = FabricDefaults.settings
        #expect(d.version == 0 && d.guardrails.count == 12 && d.limits == FabricLimits.defaults)
        #expect(d.guardrails.first { $0.id == "delete-outside-worktree" }?.builtin == "delete_outside_worktree")
        // every pattern compiles here as it must on the runtime
        for g in d.guardrails where !g.pattern.isEmpty {
            #expect((try? NSRegularExpression(pattern: g.pattern)) != nil, "\(g.id)")
        }
    }

    @Test func truncationKeepsCharactersWhole() {
        #expect("Tom’s iPhone".truncatedUTF8(5) == "Tom")
        #expect("Tom’s iPhone".truncatedUTF8(6) == "Tom’")
        #expect("short".truncatedUTF8(100) == "short")
    }
}
