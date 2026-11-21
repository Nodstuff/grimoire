import CryptoKit
import Foundation
import Testing
@testable import TaisceKit

/// Slices 2–5's viewer API (ADR 0008 §11) as the kit reads and writes it,
/// the commander that builds and signs orders, and approval verification.
/// The JSON here is typed from the ADR's shapes (the server half was being
/// built at the same time); the live run against a scratch daemon is the
/// check that the two agree.
@Suite struct FabricAPITests {
    static let runtime = "0193a5b2-7c4d-7e8f-9a0b-1c2d3e4f5a6b"
    static let session = "0193a5b2-8d9e-7f01-a2b3-c4d5e6f70819"
    static let runtimeKey = SoftwareDeviceKey()

    static func approvalRequest(runtime: String = runtime, session: String = session) -> AttentionRequest {
        AttentionRequest(runtimeID: runtime, sessionID: session, requestID: "0193a5b3-0000-7000-8000-00000000a001", kind: .approval,
                         text: "./release.sh --notarize", tool: "Bash", guardrailID: "release-deploy", guardrailLabel: "Release and deploy scripts",
                         raisedAt: 1_795_000_000, expiresAt: 1_795_000_540)
    }

    static func item(_ request: AttentionRequest, signer: any FabricDeviceKey = runtimeKey, text: String? = nil) throws -> FabricAttentionItem {
        let bytes = request.encoded()
        return FabricAttentionItem(id: request.requestID, source: .runtime, runtimeID: runtime, sessionID: session, address: "workbox/main",
                                   kind: .approval, text: text ?? request.text, raisedAt: 1_795_000_000, expiresAt: Int(request.expiresAt),
                                   signed: SignedAttention(request: bytes, signature: try signer.sign(.attention, bytes)))
    }

    // MARK: shapes

    @Test func aSlice5FleetDecodes() throws {
        let item = try Self.item(Self.approvalRequest())
        let signed = try JSONEncoder().encode(item.signed)
        let json = """
        {"rev":3,"runtimes":[{"id":"\(Self.runtime)","name":"mbp","created_at":1,"online":true,"last_seen_at":2,"version":"wb/1","accounts":[],"accounts_at":null,
          "held":true,"held_since":1795000100,"settings_version":2,"present":false,"public_key":"\(Self.runtimeKey.publicKey.base64EncodedString())",
          "devices":[{"device_id":"abc","fingerprint":"abcd 0000 0000 0000","name":"Tom’s iPhone","public_key":null,"paired_at":5,"counter":1795000000999}]}],
         "sessions":[{"id":"\(Self.session)","runtime_id":"\(Self.runtime)","address":"workbox/main","fabric":"release","project":"workbox","worktree":"main",
          "branch":"main","account":"work","state":"needs_you","tool":"","note":"","last_line":"","context_used_pct":null,"since":1,"updated_at":1,"quiet_until":null,
          "fabric_id":"f1","turn_id":"t9","queue":["o1","o2"],"queue_paused":"question","following":true,"text_off":false}],
         "attention":[{"id":"\(item.id)","source":"runtime","runtime_id":"\(Self.runtime)","session_id":"\(Self.session)","address":"workbox/main","kind":"approval",
          "text":"./release.sh --notarize","choices":[],"tool":"Bash","guardrail_label":"Release and deploy scripts","turn_id":"","raised_at":1795000000,
          "expires_at":1795000540,"signed":\(String(decoding: signed, as: UTF8.self)),"conversation_id":null,"message_id":null,"from_address":null}]}
        """
        let fleet = try JSONDecoder().decode(Fleet.self, from: Data(json.utf8))
        let r = try #require(fleet.runtimes.first)
        #expect(r.held && r.heldSince == 1_795_000_100 && r.settingsVersion == 2 && r.publicKey == Self.runtimeKey.publicKey)
        #expect(r.devices.first?.counter == 1_795_000_000_999 && r.devices.first?.name == "Tom’s iPhone" && r.isPaired("abc"))
        let s = try #require(fleet.sessions.first)
        #expect(s.fabricID == "f1" && s.turnID == "t9" && s.queue == ["o1", "o2"] && s.queuePaused == "question" && s.following && !s.textOff)
        #expect(fleet.attention.count == 1 && fleet.attention(for: s.key).first?.kind == .approval)
        #expect(fleet.attention[0].signed == item.signed, "the signed bytes travel base64, byte for byte")
    }

    @Test func turnsOrdersAndMessagesDecode() throws {
        let turn = try JSONDecoder().decode(FabricTurn.self, from: Data("""
        {"runtime_id":"r","session_id":"s","address":"taisce/main","turn_id":"t1","prompt":"Fix the heartbeat","reply":"Done.","tool_count":7,
         "outcome":{"files_changed":2,"lines_added":6,"lines_removed":2,"commits":1,"pull_request_url":null,"pull_request_number":null},
         "started":100,"ended":160,"truncated":false,"context_used_pct":44,"end":"question","source":"order","order_id":"o1","text_off":false}
        """.utf8))
        #expect(turn.outcome.linesAdded == 6 && turn.outcome.pullRequestURL == nil && turn.end == .question && turn.source == .order && turn.orderID == "o1")
        let odd = try JSONDecoder().decode(FabricTurn.self, from: Data(#"{"turn_id":"t2","end":"exploded","source":"carrier pigeon","outcome":null}"#.utf8))
        #expect(odd.end == .unknown && odd.source == .unknown && odd.outcome.isEmpty, "a newer word is unknown, not an error")
        let order = try JSONDecoder().decode(FabricOrderView.self, from: Data("""
        {"id":"o1","runtime_id":"r","session_id":"s","device_id":"d","counter":5,"kind":"command","verb":"send","hold":null,"text":"go on",
         "after_turn":"t1","queued":true,"request_id":null,"choice_id":null,"order_ids":[],"issued_at":1,"expires_at":2,"created_at":1,"updated_at":1,
         "state":"refused","reason":"wrong_turn","detail":"The session moved on."}
        """.utf8))
        #expect(order.fabricVerb == .send && order.state == .refused && order.state.isFinal && order.reason == "wrong_turn")
        let message = try JSONDecoder().decode(FabricMessage.self, from: Data("""
        {"id":"m1","conversation_id":"c1","from":{"runtime_id":"r","session_id":"a","address":"workbox/main"},"to":{"runtime_id":"r","session_id":"b","address":"taisce/main"},
         "kind":"ask","body":null,"doc":null,"in_reply_to":"m0","sent_at":5,"read_at":9,"state":"read","bounce_reason":null,"answered":true}
        """.utf8))
        #expect(message.kind == .ask && message.body == nil && message.replyTo == "m0" && message.answered && message.to.address == "taisce/main")
    }

    @Test func theStreamsNewEventsDecode() throws {
        func event(_ name: String, _ data: String) -> SSEEvent { SSEEvent(id: nil, event: name, data: data) }
        let cleared = try FabricStreamEvent.decode(event("attention_cleared", #"{"id":"a1","runtime_id":"r","session_id":"s","resolution":"approved"}"#))
        #expect(cleared == .attentionCleared(FabricAttentionCleared(id: "a1", runtimeID: "r", sessionID: "s", resolution: "approved")))
        if case .pairing(let p)? = try FabricStreamEvent.decode(event("pairing", #"{"pairing_id":"p","runtime_id":"r","state":"paired","device_id":"d","detail":null}"#)) {
            #expect(p.state == .paired && p.state.isFinal)
        } else { Issue.record("pairing") }
        #expect(try FabricStreamEvent.decode(event("session", "{}")) == nil, "fleet events are FleetChange's")
        #expect(try FabricStreamEvent.decode(event("hologram", "{}")) == nil, "an unknown event is ignored")
        #expect(throws: (any Error).self) { try FabricStreamEvent.decode(event("turn", "not json")) }
    }

    @Test func theFollowerPassesTheNewEventsOn() async throws {
        let server = MockServer { _ in
            .sse("retry: 1000\n\nid: 1\nevent: fleet\ndata: {\"rev\":1,\"runtimes\":[],\"sessions\":[]}\n\n",
                 "event: attention_cleared\ndata: {\"id\":\"a1\"}\n\n")
        }
        let follower = FleetFollower(service: server.client(), sleep: { _ in throw CancellationError() })
        var events: [FabricStreamEvent] = []
        for await signal in follower.signals() {
            if case .event(let e) = signal { events.append(e) }
        }
        #expect(events == [.attentionCleared(FabricAttentionCleared(id: "a1"))])
    }

    // MARK: routes

    @Test func anOrderPostsItsSignedBytesAndARefusalKeepsItsCode() async throws {
        let server = MockServer { r in
            switch (r.httpMethod, r.url?.path) {
            case ("POST", "/api/fabric/orders"):
                let body = try! JSONSerialization.jsonObject(with: r.httpBody!) as! [String: String]
                if body["device_id"] == "replay" {
                    return MockServer.Reply(status: 409, chunks: [Data(#"{"error":"counter at or below the last","code":"replayed"}"#.utf8)])
                }
                return MockServer.Reply(status: 202, chunks: [Data(#"{"order":{"id":"o1","runtime_id":"r","state":"pending","verb":"send"}}"#.utf8)])
            case ("PUT", "/api/fabric/settings"):
                return MockServer.Reply(status: 409, chunks: [Data(#"{"error":"stale","code":"stale_settings","version":4}"#.utf8)])
            default:
                return MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found"}"#.utf8)])
            }
        }
        let api = server.client()
        let body = FabricSignedBody(payload: Data([1, 2, 3]), deviceID: "dev", signature: Data([9]))
        let order = try await api.postOrder(body)
        #expect(order.id == "o1" && order.state == .pending)
        let sent = try JSONSerialization.jsonObject(with: try #require(server.requests.last?.httpBody)) as! [String: String]
        #expect(sent == ["payload": "AQID", "device_id": "dev", "signature": "CQ=="], "standard base64, padded")
        await #expect(throws: FabricError(status: 409, code: "replayed", message: "counter at or below the last")) {
            try await api.postOrder(FabricSignedBody(payload: Data(), deviceID: "replay", signature: Data()))
        }
        await #expect(throws: FabricError(status: 409, code: "stale_settings", message: "stale", version: 4)) {
            try await api.putSettings(body)
        }
        // a 404 without a code is still the usual not-found
        await #expect(throws: APIError.self) { try await api.withdrawOrder("nope") }
    }

    @Test func settingsWithNothingSignedAreTheDefaults() throws {
        let view = try JSONDecoder().decode(FabricSettingsView.self, from: Data(#"{"version":0,"issued_at":0,"defaults":true,"settings":null,"signed":null,"runtimes":[]}"#.utf8))
        #expect(view.defaults && view.settings == FabricDefaults.settings)
    }

    // MARK: signing orders

    @Test func countersOnlyRiseAndStartAboveWhatTheRuntimeTook() throws {
        let commander = FabricCommander(key: SoftwareDeviceKey())
        let now = Date(timeIntervalSince1970: 1_795_000_000)
        var mac = FleetRuntime(id: Self.runtime, name: "mbp", online: true)
        let (a, _) = try commander.order(.hold(true), runtime: mac, now: now)
        let (b, _) = try commander.order(.hold(false), runtime: mac, now: now)
        #expect(a.counter == 1_795_000_000_000 && b.counter == a.counter + 1, "unix ms, then one more within the same ms")
        mac.devices = [FabricPairedDevice(deviceID: commander.deviceID, counter: 9_000_000_000_000)]
        let (c, _) = try commander.order(.hold(true), runtime: mac, now: now)
        #expect(c.counter == 9_000_000_000_001, "above what the runtime last took from this device")
        #expect(c.expiresAt == 1_795_003_600 && c.issuedAt == 1_795_000_000)
    }

    @Test func whatIsSignedIsWhatWasAskedAndVerifies() throws {
        let key = SoftwareDeviceKey()
        let commander = FabricCommander(key: key)
        let mac = FleetRuntime(id: Self.runtime, name: "mbp", online: true)
        let send = FabricCommand(sessionID: Self.session, verb: .send, text: "Yes, open it", afterTurn: "t9")
        let (order, body) = try commander.order(.command(send), runtime: mac)
        #expect(body.deviceID == key.deviceID && body.payload == order.encoded())
        #expect(FabricSignature.verify(publicKey: key.publicKey, context: .order, payload: body.payload, signature: body.signature))
        #expect(try FabricOrder.decode(body.payload) == order)
        #expect(FabricOrder.isCanonical(body.payload))
        let (settings, sbody) = try commander.settings(FabricDefaults.settings, version: 1)
        #expect(settings.version == 1 && sbody.payload == settings.encoded())
        #expect(FabricSignature.verify(publicKey: key.publicKey, context: .settings, payload: sbody.payload, signature: sbody.signature))
    }

    @Test func textThatWouldRunSomethingIsNeverSigned() throws {
        let commander = FabricCommander(key: SoftwareDeviceKey())
        let mac = FleetRuntime(id: Self.runtime, name: "mbp", online: true)
        for bad in ["!rm -rf ~", "/clear", "  /compact", "\n!ls"] {
            #expect(throws: FabricOrderProblem.unsafeText) {
                try commander.order(.command(FabricCommand(sessionID: Self.session, verb: .send, text: bad)), runtime: mac)
            }
        }
        #expect(throws: FabricOrderProblem.emptyText) { try commander.order(.command(FabricCommand(sessionID: Self.session, verb: .answer, text: "  ")), runtime: mac) }
        #expect(throws: FabricOrderProblem.tooLong) {
            try commander.order(.command(FabricCommand(sessionID: Self.session, verb: .send, text: String(repeating: "a", count: 16 * 1024 + 1))), runtime: mac)
        }
        // a deny's reason is not typed as a prompt, and a path mid-sentence is fine
        _ = try commander.order(.command(FabricCommand(sessionID: Self.session, verb: .send, text: "look at /tmp/x")), runtime: mac)
    }

    // MARK: approvals

    @Test func anApprovalIsVerifiedAgainstTheKeyFromThePairingCode() throws {
        let trust = MemoryTrustStore([Self.runtime: Self.runtimeKey.publicKey])
        let item = try Self.item(Self.approvalRequest(), text: "the hub's copy says something else")
        let verified = try VerifiedAttention.verify(item, trust: trust)
        #expect(verified.request.text == "./release.sh --notarize", "what is shown comes from the signed bytes")
        let approve = verified.approve()
        #expect(approve.verb == .approve && approve.requestHash == Data(SHA256.hash(data: item.signed!.request)) && approve.requestID == item.id)
        #expect(verified.deny(because: "not today").text == "not today")
        #expect(verified.lifetime(now: Date(timeIntervalSince1970: 1_795_000_000)) == 540)

        // not paired with that Mac: nothing to check against
        #expect(throws: FabricOrderProblem.notPaired) { try VerifiedAttention.verify(item, trust: MemoryTrustStore()) }
        // signed by something else (the hub, say)
        let forged = try Self.item(Self.approvalRequest(), signer: SoftwareDeviceKey())
        #expect(throws: FabricOrderProblem.self) { try VerifiedAttention.verify(forged, trust: trust) }
        // a validly signed request for another session, presented as this one
        let elsewhere = try Self.item(Self.approvalRequest(session: "0193a5b2-0000-7000-8000-000000000000"))
        #expect(throws: FabricOrderProblem.unverified("The request names another session.")) { try VerifiedAttention.verify(elsewhere, trust: trust) }
        // no signature at all
        var bare = item
        bare.signed = nil
        #expect(throws: FabricOrderProblem.self) { try VerifiedAttention.verify(bare, trust: trust) }
    }
}
