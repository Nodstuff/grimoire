import Foundation
import Testing
import Valet
@testable import TaisceKit

/// What the phone believes before it signs: a request is the item it is
/// shown as and only answered the way the Mac signed it; keys and trust in
/// the Keychain survive a failed read; settings are edited only on top of
/// what was signed, and another device's only after a look; one unreadable
/// stream event or attention item never takes the fleet with it.
@Suite struct FabricTrustTests {
    static let runtime = FabricAPITests.runtime
    static let session = FabricAPITests.session
    static var trust: MemoryTrustStore { MemoryTrustStore([runtime: FabricAPITests.runtimeKey.publicKey]) }

    // MARK: attention

    @Test func aRequestUnderAnotherItemsIdIsNotVerified() throws {
        var item = try FabricAPITests.item(FabricAPITests.approvalRequest())
        item.id = "0193a5b3-0000-7000-8000-00000000a999"
        #expect(throws: FabricOrderProblem.unverified("The request is not the one named.")) { try VerifiedAttention.verify(item, trust: Self.trust) }
    }

    @Test func whatTheMacSignedDecidesWhichVerbsApply() throws {
        // the server calls it an approval; the Mac signed a question
        var question = AttentionRequest(runtimeID: Self.runtime, sessionID: Self.session, requestID: "0193a5b3-0000-7000-8000-00000000b001", kind: .question,
                                        text: "Ship it?", choices: [FabricChoice(id: "yes", label: "Yes"), FabricChoice(id: "no", label: "No, stop")])
        var item = try FabricAPITests.item(question)
        item.kind = .approval
        let v = try VerifiedAttention.verify(item, trust: Self.trust)
        #expect(v.kind == .question)
        #expect(throws: FabricOrderProblem.wrongKind) { try v.approve() }
        #expect(throws: FabricOrderProblem.wrongKind) { try v.deny() }
        #expect(try v.answer(choice: FabricChoice(id: "no", label: "No, stop")).text == "No, stop")
        // a choice whose label is not the one the Mac signed for that id
        #expect(throws: FabricOrderProblem.self) { try v.answer(choice: FabricChoice(id: "no", label: "Yes")) }
        // an approval is never answered, nor a dialog
        let approval = try VerifiedAttention.verify(try FabricAPITests.item(FabricAPITests.approvalRequest()), trust: Self.trust)
        #expect(throws: FabricOrderProblem.wrongKind) { try approval.answer(words: "yes") }
        question.kind = .dialog
        let dialog = try VerifiedAttention.verify(try FabricAPITests.item(question), trust: Self.trust)
        #expect(throws: FabricOrderProblem.wrongKind) { try dialog.answer(words: "yes") }
    }

    // MARK: keys and trust in the Keychain

    @Test func onlyAMissingKeyIsMadeAndAFailedReadNeverOverwrites() throws {
        let secrets = MemorySecrets()
        let first = try FabricDeviceIdentity(secrets: secrets, enclave: false).current()
        // a locked or denied Keychain: the next launch must not mint a new key over the old one
        secrets.fail(with: KeychainError.couldNotAccessKeychain)
        #expect(throws: (any Error).self) { try FabricDeviceIdentity(secrets: secrets, enclave: false).current() }
        #expect(FabricDeviceIdentity(secrets: secrets, enclave: false).existing() == nil)
        secrets.fail(with: nil)
        #expect(try FabricDeviceIdentity(secrets: secrets, enclave: false).current().deviceID == first.deviceID, "the same key, never replaced")

        let store = KeychainTrustStore(secrets: secrets)
        try store.trust("R1", key: Data([1]))
        secrets.fail(with: KeychainError.couldNotAccessKeychain)
        #expect(throws: (any Error).self) { try store.trust("r2", key: Data([2])) }
        #expect(store.runtimeKey("r1") == nil, "unreadable: nothing verifies")
        // and a request is not taken for one from an unpaired Mac: the failure is said
        let keyed = KeychainTrustStore(secrets: MemorySecrets())
        try keyed.trust(Self.runtime, key: FabricAPITests.runtimeKey.publicKey)
        let item = try FabricAPITests.item(FabricAPITests.approvalRequest())
        #expect(try VerifiedAttention.verify(item, trust: keyed).kind == .approval)
        let locked = MemorySecrets()
        try KeychainTrustStore(secrets: locked).trust(Self.runtime, key: FabricAPITests.runtimeKey.publicKey)
        locked.fail(with: KeychainError.couldNotAccessKeychain)
        do {
            _ = try VerifiedAttention.verify(item, trust: KeychainTrustStore(secrets: locked))
            Issue.record("verified with an unreadable trust list")
        } catch let FabricOrderProblem.unverified(why) {
            #expect(why.contains("can't be read"), "\(why)")
        }
        secrets.fail(with: nil)
        #expect(store.trusted() == ["r1": Data([1])], "the list a failed read would have overwritten is intact")
    }

    // MARK: settings an edit may start from

    static func view(_ s: FabricSettings, by key: any FabricDeviceKey, version: UInt64, signer: (any FabricDeviceKey)? = nil) throws -> FabricSettingsView {
        var s = s
        s.version = version
        let payload = s.encoded()
        let sig = try (signer ?? key).sign(.settings, payload)
        return FabricSettingsView(version: version, defaults: false, settings: FabricDefaults.settings,
                                  signed: FabricSignedBody(payload: payload, deviceID: key.deviceID, signature: sig))
    }

    @Test func settingsAreBelievedOnlyAsSignedAndAnotherDevicesAfterALook() throws {
        let me = SoftwareDeviceKey()
        let other = SoftwareDeviceKey()
        #expect(SignedSettings.open(FabricSettingsView(version: 0, defaults: true, settings: FabricDefaults.settings), me: me, known: nil).base == FabricDefaults.settings)
        #expect(SignedSettings.open(FabricSettingsView(version: 3, defaults: false, settings: FabricDefaults.settings), me: me, known: nil).base == nil,
                "no signature, nothing to build on")

        var mine = FabricDefaults.settings
        mine.fabrics = [FabricDefinition(id: "f1", name: "release")]
        let signedByMe = try Self.view(mine, by: me, version: 1)
        guard case .trusted(let base, let payload) = SignedSettings.open(signedByMe, me: me, known: nil) else { Issue.record("mine verifies"); return }
        #expect(base.fabrics.map(\.name) == ["release"])
        #expect(signedByMe.settings.fabrics.isEmpty, "the server's JSON said otherwise, and is not what an edit starts from")
        // a forged signature in this phone's name
        guard case .invalid = SignedSettings.open(try Self.view(mine, by: me, version: 1, signer: other), me: me, known: nil) else { Issue.record("forged"); return }
        // a payload whose version is not the one the server names
        var wrong = signedByMe
        wrong.version = 2
        guard case .invalid = SignedSettings.open(wrong, me: me, known: nil) else { Issue.record("version"); return }

        // another device loosened a guardrail and a limit: shown, not built on until looked at
        var loosened = mine
        loosened.guardrails[0].enabled = false
        loosened.limits = FabricLimits(pairMessages: 40)
        let theirs = try Self.view(loosened, by: other, version: 2)
        guard case .unconfirmed(_, let theirPayload, let device, let changes) = SignedSettings.open(theirs, me: me, known: payload) else {
            Issue.record("unconfirmed"); return
        }
        #expect(device == other.deviceID)
        #expect(changes.contains { $0.contains(loosened.guardrails[0].label) && $0.hasSuffix("turned off") }, "\(changes)")
        #expect(changes.contains { $0.contains("12 \u{2192} 40") }, "\(changes)")
        #expect(SignedSettings.open(theirs, me: me, known: payload).base == nil)
        // once looked at here, it is what edits build on
        #expect(SignedSettings.open(theirs, me: me, known: theirPayload).base?.guardrails[0].enabled == false)
    }

    // MARK: the stream and the fleet

    @Test func oneUnreadableFabricEventIsSkippedAndTheStreamGoesOn() async throws {
        let server = MockServer { _ in
            .sse("id: 1\nevent: fleet\ndata: \(FleetFixture.live)\n\n",
                 "event: turn\ndata: {\"turn_id\": 5}\n\n",
                 "event: attention_cleared\ndata: {\"id\":\"a1\",\"runtime_id\":\"r\",\"session_id\":\"s\",\"resolution\":\"approved\"}\n\n")
        }
        let (signals, _) = await FleetTests.follow(server, stopAfter: 1)
        let events = signals.compactMap { if case .event(let e) = $0 { e } else { nil } }
        #expect(events.count == 1, "the bad turn skipped, the next event read: \(events)")
        #expect(!signals.contains { if case .failed = $0 { true } else { false } }, "the stream did not fail over it")
        #expect(FleetTests.changes(signals).count == 1)
    }

    @Test func anAttentionItemThisAppCannotReadLeavesTheFleet() throws {
        var json = FleetFixture.live.trimmingCharacters(in: .whitespacesAndNewlines)
        json.removeLast()
        json += #","attention":[{"id":5},{"id":"h1","source":"hub","runtime_id":"r","session_id":"s","address":"a/b","kind":"wake_offer","text":"","raised_at":1}]}"#
        let fleet = try FleetTests.decode(Fleet.self, json)
        #expect(fleet.sessions.count == 3 && fleet.attention.map(\.id) == ["h1"])
    }

    // MARK: a runtime's held state

    /// A runtime event exactly as a scratch hub sent it for real Workbox
    /// after Hold all (2026-11-21): six accounts with every figure unknown,
    /// a paired device whose counter is a millisecond clock.
    static let heldRuntime = #"""
{"id": "01a1fde0-eab2-7072-8121-a101667bb93a", "name": "workbox-mac", "created_at": 1795260738, "online": true, "connection": "online", "last_seen_at": 1795269621, "version": "Workbox 0.0.1 (dev)", "accounts": [{"name": "default", "session_resets_at": null, "session_used_pct": null, "week_resets_at": null, "week_used_pct": null}, {"name": "eng2", "session_resets_at": null, "session_used_pct": null, "week_resets_at": null, "week_used_pct": null}, {"name": "eng3", "session_resets_at": null, "session_used_pct": null, "week_resets_at": null, "week_used_pct": null}, {"name": "eng4", "session_resets_at": null, "session_used_pct": null, "week_resets_at": null, "week_used_pct": null}, {"name": "eng5", "session_resets_at": null, "session_used_pct": null, "week_resets_at": null, "week_used_pct": null}, {"name": "eng6", "session_resets_at": null, "session_used_pct": null, "week_resets_at": null, "week_used_pct": null}], "accounts_at": 1795267487, "held": true, "held_since": 1795268883, "settings_version": 0, "present": false, "public_key": "BBsNcMWKtrY2j9pSODAG3xGhJbHbYTMrF41kxVzxZs7DFX9q7q5BhJ04G+QOpOsDnr55HMhV/u40WsXwu+m2xkc=", "devices": [{"device_id": "6c7a411a916efff433dd836115989ee39fec07562f95ce5fa5bb28e62f87a975", "fingerprint": "6c7a 411a 916e fff4", "name": "fabric3-iphone", "public_key": "BHS731254IsRO+U17DCKsLa3BxldmDoVWzX0GjcJTVSWMBM52p/0Z7yICarQiPETVnNryMPPPDlNCuqSE9lCijk=", "paired_at": 1795268815, "counter": 1795268883183}]}
"""#

    @Test func aHeldRuntimeAsTheHubSendsItShowsHeld() throws {
        let decoded = try #require(try FleetChange.decode(SSEEvent(id: "319", event: "runtime", data: Self.heldRuntime)))
        guard case .runtime(let r) = decoded.change else { Issue.record("a runtime change"); return }
        #expect(r.held && r.heldSince == 1_795_268_883 && r.connection == .online)
        var before = r
        before.held = false
        before.heldSince = nil
        var fleet = Fleet(rev: 318, runtimes: [before])
        let applied = fleet.apply(decoded.change, rev: decoded.rev)
        #expect(applied)
        #expect(fleet.runtimes.first?.held == true)
    }
}
