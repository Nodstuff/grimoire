import Foundation
import Synchronization
import Testing
import TaisceKit
import UserNotifications
@testable import Taisce

/// A scripted fabric server: the fleet, and every Slice 2–5 route recorded.
private final class FakeFabric: FabricService, @unchecked Sendable {
    struct State {
        var fleet: Fleet
        var calls: [String] = []
        var posted: [FabricSignedBody] = []
        var settingsPuts: [FabricSignedBody] = []
        var settings = FabricSettingsView(version: 0, defaults: true, settings: FabricDefaults.settings)
        var refuse: FabricError?
        var pairing: FabricPairingView?
        var turns: [FabricTurn] = []
        /// a server from before `after=`: it answers the newest page, newest first
        var ignoresAfter = false
        /// every turns request fails
        var turnsFail = false
        var attention: [FabricAttentionItem] = []
    }

    let state: Mutex<State>
    init(_ fleet: Fleet) { state = Mutex(State(fleet: fleet)) }
    var calls: [String] { state.withLock { $0.calls } }
    var posted: [FabricOrder] { state.withLock { $0.posted.compactMap { try? FabricOrder.decode($0.payload) } } }
    var postedBodies: [FabricSignedBody] { state.withLock { $0.posted } }

    private func log(_ c: String) { state.withLock { $0.calls.append(c) } }

    func fleet() async throws -> Fleet { log("fleet"); return state.withLock { $0.fleet } }
    func fleetStream() -> AsyncThrowingStream<SSEOutput, any Error> { AsyncThrowingStream { _ in } }
    func snoozeSession(_ key: FleetSessionKey, until: Date) async throws -> FleetSession { throw APIError.notFound("") }
    func unsnoozeSession(_ key: FleetSessionKey) async throws -> FleetSession { throw APIError.notFound("") }

    func turns(_ session: FleetSessionKey, before: Int?, limit: Int) async throws -> FabricTurnPage {
        log("turns \(before.map(String.init) ?? "-")")
        if state.withLock({ $0.turnsFail }) { throw APIError.http(status: 503) }
        let all = state.withLock { $0.turns }.filter { before == nil || $0.ended < before! }.sorted { $0.ended > $1.ended }
        return FabricTurnPage(turns: Array(all.prefix(limit)), more: all.count > limit)
    }
    func turns(_ session: FleetSessionKey, after: Int, limit: Int) async throws -> FabricTurnPage {
        if state.withLock({ $0.ignoresAfter }) { return try await turns(session, before: nil, limit: limit) }
        log("turns after \(after)")
        if state.withLock({ $0.turnsFail }) { throw APIError.http(status: 503) }
        let all = state.withLock { $0.turns }.filter { $0.ended > after }.sorted { $0.ended < $1.ended }
        return FabricTurnPage(turns: Array(all.prefix(limit)), more: all.count > limit)
    }
    func turns(since: Int, limit: Int) async throws -> [FabricTurn] { [] }
    func attention() async throws -> [FabricAttentionItem] { log("attention"); return state.withLock { $0.attention } }
    func orders(session: FleetSessionKey?, limit: Int) async throws -> [FabricOrderView] { log("orders"); return [] }
    func order(_ id: String) async throws -> FabricOrderView { throw APIError.notFound("") }
    func postOrder(_ body: FabricSignedBody) async throws -> FabricOrderView {
        log("post")
        if let e = state.withLock({ s -> FabricError? in defer { s.refuse = nil }; return s.refuse }) { throw e }
        state.withLock { $0.posted.append(body) }
        let o = try FabricOrder.decode(body.payload)
        return FabricOrderView(id: o.orderID, runtimeID: o.runtimeID, sessionID: nil, deviceID: body.deviceID, verb: nil, state: .pending)
    }
    func withdrawOrder(_ id: String) async throws -> FabricOrderView {
        log("withdraw \(id)")
        return FabricOrderView(id: id, runtimeID: "r1", sessionID: nil, verb: "send", state: .withdrawn)
    }
    func settings() async throws -> FabricSettingsView { log("settings"); return state.withLock { $0.settings } }
    func putSettings(_ body: FabricSignedBody) async throws -> FabricSettingsView {
        log("put settings")
        if let e = state.withLock({ s -> FabricError? in defer { s.refuse = nil }; return s.refuse }) { throw e }
        let s = try FabricSettings.decode(body.payload)
        return state.withLock { st in
            st.settingsPuts.append(body)
            st.settings = FabricSettingsView(version: s.version, defaults: false, settings: s, signed: body)
            return st.settings
        }
    }
    func projects() async throws -> [FabricProject] { [FabricProject(name: "taisce")] }
    func setProject(_ name: String, textOff: Bool?, privateNotifications: Bool?) async throws -> FabricProject {
        log("project \(name) \(textOff.map(String.init) ?? "-") \(privateNotifications.map(String.init) ?? "-")")
        return FabricProject(name: name, textOff: textOff ?? false, privateNotifications: privateNotifications ?? false)
    }
    func pair(runtimeID: String, pairingID: String, sealed: Data) async throws -> FabricPairingView {
        log("pair \(runtimeID)")
        return FabricPairingView(pairingID: pairingID, runtimeID: runtimeID, state: .sent)
    }
    func pairing(_ id: String) async throws -> FabricPairingView { state.withLock { $0.pairing } ?? FabricPairingView(pairingID: id, runtimeID: "", state: .sent) }
    func follow(_ session: FleetSessionKey, _ on: Bool) async throws -> FleetSession {
        log("follow \(on)")
        var s = state.withLock { $0.fleet.sessions[0] }
        s.following = on
        return s
    }
    func dismissAttention(_ id: String) async throws { log("dismiss \(id)") }
    func conversations() async throws -> [FabricConversation] { [] }
    func conversation(_ id: String, limit: Int) async throws -> FabricConversationPage {
        FabricConversationPage(conversation: FabricConversation(id: id, a: FabricSessionRef(runtimeID: "r1", sessionID: "a", address: "a/x"),
                                                                b: FabricSessionRef(runtimeID: "r1", sessionID: "b", address: "b/x")), messages: [])
    }
    func continueConversation(_ id: String) async throws { log("continue \(id)") }
    func stopConversation(_ id: String) async throws { log("stop \(id)") }
}

/// Slices 2–5 in the app: the command center's undo, signing and approvals,
/// the stores the stream feeds, the session screen's models, settings and
/// pairing, Talk, and the notifications.
@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct FabricAppTests {
    static let now = Date(timeIntervalSince1970: 1_795_176_600)
    static var nowS: Int { Int(now.timeIntervalSince1970) }
    static let runtimeKey = SoftwareDeviceKey()
    static let deviceKey = SoftwareDeviceKey()

    static func session(_ id: String, _ address: String, _ state: SessionState, turn: String = "t9", queue: [String] = [], fabric: String = "taisce") -> FleetSession {
        FleetSession(sessionID: id, runtimeID: "r1", address: address, fabric: fabric, project: "taisce", branch: "main", account: "personal", state: state,
                     contextUsedPct: 44, since: nowS - 120, updatedAt: nowS - 120, turnID: turn, queue: queue)
    }

    static func fleet(paired: Bool = true, held: Bool = false, sessions: [FleetSession]? = nil, attention: [FabricAttentionItem] = []) -> Fleet {
        let devices = paired ? [FabricPairedDevice(deviceID: deviceKey.deviceID, name: "phone", counter: 5)] : []
        return Fleet(rev: 3, runtimes: [FleetRuntime(id: "r1", name: "mbp", online: true, held: held, devices: devices)],
                     sessions: sessions ?? [session("s1", "taisce/main", .needsYou), session("s2", "qompass/mcp-dx", .working, queue: ["q1"])],
                     attention: attention)
    }

    static func approval(signer: any FabricDeviceKey = runtimeKey, expiresIn: Int64 = 600) -> FabricAttentionItem {
        let r = AttentionRequest(runtimeID: "r1", sessionID: "s1", requestID: "a1", kind: .approval, text: "./release.sh", tool: "Bash",
                                 guardrailLabel: "Release", raisedAt: Int64(Date().timeIntervalSince1970), expiresAt: Int64(Date().timeIntervalSince1970) + expiresIn)
        let bytes = r.encoded()
        return FabricAttentionItem(id: "a1", source: .runtime, runtimeID: "r1", sessionID: "s1", address: "taisce/main", kind: .approval,
                                   text: "./release.sh", guardrailLabel: "Release", raisedAt: nowS, expiresAt: Int(r.expiresAt),
                                   signed: SignedAttention(request: bytes, signature: try! signer.sign(.attention, bytes)))
    }

    struct Owner: OwnerCheck {
        let answer: Bool
        func confirm(_ reason: String) async -> Bool { answer }
    }

    private func store(_ fake: FakeFabric, owner: Bool = true, trusted: Bool = true) -> FleetStore {
        let s = FleetStore()
        s.serviceOverride = fake
        s.command.identity = FabricDeviceIdentity(key: Self.deviceKey)
        s.command.counters = MemoryCounterStore()
        s.command.trust = MemoryTrustStore(trusted ? ["r1": Self.runtimeKey.publicKey] : [:])
        s.command.ownerCheck = Owner(answer: owner)
        s.command.undoWindow = .milliseconds(150)
        s.config.pairingPoll = .milliseconds(10)
        s.config.secrets = MemorySecrets()
        // the fleet as the stream would have delivered it
        s.take(.change(.fleet(fake.state.withLock { $0.fleet }), rev: 3))
        return s
    }

    func until(_ what: String, _ check: @MainActor () -> Bool) async {
        for _ in 0..<400 where !check() { try? await Task.sleep(for: .milliseconds(5)) }
        #expect(check(), "\(what)")
    }

    // MARK: sending

    @Test func aSendWaitsOutItsUndoWindowThenGoesSigned() async throws {
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        let session = try #require(s.fleet.sessions.first)
        #expect(s.command.send("Yes, open it", to: session, queued: false) == nil)
        #expect(s.command.pending.count == 1 && fake.posted.isEmpty, "held, not sent")
        await until("posted after the window") { !fake.posted.isEmpty }
        let order = try #require(fake.posted.first)
        guard case .command(let c) = order.what else { Issue.record("a command"); return }
        #expect(c.verb == .send && c.text == "Yes, open it" && c.afterTurn == "t9" && !c.queued && c.sessionID == "s1")
        #expect(order.counter > 5, "above what the runtime last took from this device")
        let body = try #require(fake.postedBodies.first)
        #expect(FabricSignature.verify(publicKey: Self.deviceKey.publicKey, context: .order, payload: body.payload, signature: body.signature))
        // it stays held (its words safe) until the post answers, then goes
        await until("gone once posted") { s.command.pending.isEmpty }
    }

    @Test func theKeyboardsTrailingSpaceIsNotSent() async throws {
        // Claude Code trims it, so Workbox never found "Ok this is good " in
        // the prompt and reported a delivered message as not delivered
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        s.command.send("Ok this is good \n", to: try #require(s.fleet.sessions.first), queued: false)
        await until("posted after the window") { !fake.posted.isEmpty }
        guard case .command(let c)? = fake.posted.first?.what else { Issue.record("sent"); return }
        #expect(c.text == "Ok this is good")
    }

    @Test func undoTakesItBackAndNothingIsSent() async throws {
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        let session = try #require(s.fleet.sessions.first)
        s.command.send("Not yet", to: session, queued: false)
        let id = try #require(s.command.pending.first?.id)
        #expect(s.command.undo(id) == "Not yet", "its words, for the box")
        try await Task.sleep(for: .milliseconds(300))
        #expect(fake.posted.isEmpty && s.command.pending.isEmpty)
    }

    @Test func leavingTheAppSendsWhatIsHeldAtOnce() async throws {
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        s.command.undoWindow = .seconds(30)
        s.command.send("queued for later", to: try #require(s.fleet.sessions.last), queued: true)
        await s.command.flush()
        guard case .command(let c)? = fake.posted.first?.what else { Issue.record("sent"); return }
        #expect(c.queued && c.text == "queued for later")
    }

    @Test func wordsThatWouldRunSomethingAreRefusedBeforeTheUndo() throws {
        let s = store(FakeFabric(Self.fleet()))
        let session = try #require(s.fleet.sessions.first)
        #expect(s.command.send("/clear", to: session, queued: false)?.contains("can't start with ! or /") == true)
        #expect(s.command.send("!rm -rf ~", to: session, queued: false) != nil)
        #expect(s.command.pending.isEmpty)
    }

    @Test func aRefusalIsWordedAndARefusedOrderOfThisPhoneIsSaidOnce() async throws {
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        fake.state.withLock { $0.refuse = FabricError(status: 422, code: "unknown_device", message: "x") }
        let failure = await s.command.command(.interrupt, try #require(s.fleet.sessions.first).key)
        #expect(failure?.contains("isn't paired") == true)
        // the runtime refused one of ours later: said once
        let view = FabricOrderView(id: "o9", runtimeID: "r1", sessionID: "s1", deviceID: Self.deviceKey.deviceID, verb: "send", state: .refused, reason: "wrong_turn")
        s.take(.order(view))
        #expect(s.command.notice?.text == "taisce/main: It moved on before your reply arrived.")
        s.command.notice = nil
        s.take(.order(view))
        #expect(s.command.notice == nil, "not again for the same state")
    }

    @Test func holdAllIsOneOrderPerMac() async throws {
        let fake = FakeFabric(Fleet(rev: 1, runtimes: [
            FleetRuntime(id: "r1", name: "mbp", online: true), FleetRuntime(id: "r2", name: "studio", online: false),
        ]))
        let s = store(fake)
        #expect(await s.command.hold(true) == nil)
        #expect(fake.posted.map(\.runtimeID) == ["r1", "r2"] && fake.posted.allSatisfy { $0.what == .hold(true) })
    }

    // MARK: approvals

    @Test func anApprovalNeedsTheOwnerAndCarriesTheRequestsHash() async throws {
        let item = Self.approval()
        let fake = FakeFabric(Self.fleet(attention: [item]))
        let s = store(fake, owner: false)
        guard case .success(let v) = s.command.verify(item) else { Issue.record("verifies"); return }
        #expect(await s.command.approve(v) == "", "Face ID cancelled: nothing said, nothing sent")
        #expect(fake.posted.isEmpty)
        s.command.ownerCheck = Owner(answer: true)
        #expect(await s.command.approve(v) == nil)
        let order = try #require(fake.posted.first)
        guard case .command(let c) = order.what else { Issue.record("a command"); return }
        #expect(c.verb == .approve && c.requestID == "a1" && c.requestHash == FabricSignature.requestHash(item.signed!.request))
        #expect(order.expiresAt <= Int64(Date().timeIntervalSince1970) + 600, "no longer than the request waits")
    }

    @Test func anApprovalFromAnUnpairedMacOrAForgerIsNotVerified() {
        let s = store(FakeFabric(Self.fleet()), trusted: false)
        guard case .failure(.notPaired) = s.command.verify(Self.approval()) else { Issue.record("not paired"); return }
        s.command.trust = MemoryTrustStore(["r1": Self.runtimeKey.publicKey])
        guard case .failure(.unverified) = s.command.verify(Self.approval(signer: SoftwareDeviceKey())) else { Issue.record("forged"); return }
    }

    // MARK: the stream

    @Test func theStreamsEventsLandWhereTheyBelong() {
        let s = store(FakeFabric(Self.fleet()))
        let turn = FabricTurn(runtimeID: "r1", sessionID: "s1", address: "taisce/main", turnID: "t10", prompt: "p", reply: "r", started: 1, ended: 2)
        s.take(.event(.turn(turn)))
        s.take(.event(.turn(turn)))
        #expect(s.liveTurns[turn.session] == [turn], "once each")
        s.take(.event(.attention(Self.approval())))
        #expect(s.fleet.attention(for: turn.session).count == 1)
        s.take(.event(.attentionCleared(FabricAttentionCleared(id: "a1"))))
        #expect(s.fleet.attention.isEmpty)
        s.take(.event(.settings(FabricSettingsView(version: 4, defaults: false, settings: FabricSettings(version: 4)))))
        s.take(.event(.settings(FabricSettingsView(version: 2, defaults: false, settings: FabricSettings(version: 2)))))
        #expect(s.config.view?.version == 4, "an older settings never replaces a newer")
        s.reset()
        #expect(s.liveTurns.isEmpty && s.config.view == nil && s.command.orders.isEmpty)
    }

    // MARK: settings and pairing

    @Test func settingsAreSignedAsTheNextVersionAndABadPatternNeverIs() async throws {
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        await s.config.load()
        var next = s.config.settings
        next.fabrics.append(FabricDefinition(id: "f1", name: "release-train", purpose: "Ship it"))
        #expect(await s.config.save(next) == nil)
        let put = try #require(fake.state.withLock { $0.settingsPuts.first })
        #expect(FabricSignature.verify(publicKey: Self.deviceKey.publicKey, context: .settings, payload: put.payload, signature: put.signature))
        let saved = try FabricSettings.decode(put.payload)
        #expect(saved.version == 1 && saved.fabrics.map(\.name) == ["release-train"] && saved.guardrails.count == 12)
        #expect(s.config.view?.version == 1)

        var bad = s.config.settings
        bad.guardrails[0].pattern = "(unclosed"
        #expect(await s.config.save(bad)?.contains("doesn't compile") == true)
        var slash = s.config.settings
        slash.fabrics[0].name = "a/b"
        #expect(await s.config.save(slash)?.contains("can't contain") == true)
        #expect(fake.state.withLock { $0.settingsPuts.count } == 1, "neither was signed")

        // a Mac that doesn't know this phone: name it, and say pair first
        fake.state.withLock { $0.refuse = FabricError(status: 422, code: "not_paired_everywhere", message: "pair this device with every runtime first; not with: studio") }
        let unpaired = Fleet(rev: 4, runtimes: fake.state.withLock { $0.fleet }.runtimes + [FleetRuntime(id: "r2", name: "studio", online: true)])
        s.take(.change(.fleet(unpaired), rev: 4))
        #expect(s.config.unpairedMacs() == ["studio"])
        let words = await s.config.save(s.config.settings)
        #expect(words?.contains("Pair this phone with studio first") == true, "\(words ?? "")")
        #expect(FabricWords.notPairedEverywhere([], server: "not with: mini").contains("not with: mini"), "the server's names when the fleet has none")

        // another device saved first: reload, and say so
        fake.state.withLock { $0.refuse = FabricError(status: 409, code: "stale_settings", message: "stale", version: 2) }
        #expect(await s.config.save(s.config.settings)?.contains("another device") == true)
    }

    @Test func pairingSealsForTheRuntimeAndTrustsItsKeyOncePaired() async throws {
        let fake = FakeFabric(Self.fleet(paired: false))
        let s = store(fake, trusted: false)
        let code = "taisce-pair:?v=1&r=0193a5b2-7c4d-7e8f-9a0b-1c2d3e4f5a6b&p=0193a5b3-0000-7000-8000-00000000d001&k=BPgjrv1ubl5Rr5-YiAEzAI7CxA9o7d4uqvCEwxeiOwV191bSoD4cbuIpSo5r2djqK1aTj52H6AobZ37X3VXGtcg&s=twtU5RcvvkNJoGIL4XmambwlRRx2b7ddTxeQGxWz47U&h=taisce.example"
        if case .failure(.words(let w)) = await s.config.pair(code: code, deviceName: "phone", serverHost: "taisce.null.ie") {
            #expect(w.contains("reports to taisce.example"))
        } else { Issue.record("another server's code is refused") }
        #expect(!fake.calls.contains { $0.hasPrefix("pair") })
        guard case .success(let id) = await s.config.pair(code: code, deviceName: "phone", serverHost: "taisce.example") else { Issue.record("pairs"); return }
        // read at the Mac: its key is kept as the code is used, not only once the Mac confirms
        #expect(s.command.trust.runtimeKey("0193a5b2-7c4d-7e8f-9a0b-1c2d3e4f5a6b") == (try PairingCode(code)).runtimeKey)
        s.take(.event(.pairing(FabricPairingView(pairingID: id, runtimeID: "0193a5b2-7c4d-7e8f-9a0b-1c2d3e4f5a6b", state: .paired))))
        #expect(s.config.pairings[id]?.state == .paired)
        #expect(FabricConfigStore.sameHost("127.0.0.1:7633", "127.0.0.1:7633") && FabricConfigStore.sameHost("taisce.null.ie", "https://taisce.null.ie"))
    }

    // MARK: Talk

    @Test func talkSettlesItsItems() async throws {
        let wake = FabricAttentionItem(id: "h1", source: .hub, runtimeID: "r1", sessionID: "s3", address: "qompass/nimbus", kind: .wakeOffer, text: "", raisedAt: 1)
        let pair = FabricAttentionItem(id: "h2", source: .hub, runtimeID: "r1", sessionID: "s1", address: "taisce/main", kind: .pairPaused, text: "", raisedAt: 1, conversationID: "c1")
        var sessions = Self.fleet().sessions
        sessions.append(Self.session("s3", "qompass/nimbus", .asleep))
        let fake = FakeFabric(Self.fleet(sessions: sessions, attention: [wake, pair]))
        let s = store(fake)
        #expect(s.talk.waiting == 2)
        #expect(await s.talk.continuePair("c1") == nil)
        #expect(fake.calls.contains("continue c1") && s.talk.waiting == 1)
        // asleep: waking it is resuming it
        #expect(await s.talk.wake(wake) == nil)
        guard case .command(let c)? = fake.posted.last?.what else { Issue.record("an order"); return }
        #expect(c.verb == .resume && c.sessionID == "s3")
        #expect(fake.calls.contains("dismiss h1") && s.talk.waiting == 0)
    }

    // MARK: the session screen's models

    @Test func theReplyBoxSaysWhatItWillDo() {
        let mac = FleetRuntime(id: "r1", name: "mbp", online: true)
        #expect(ComposerMode(Self.session("s", "a/b", .needsYou), runtime: mac) == .reply)
        #expect(ComposerMode(Self.session("s", "a/b", .idle, queue: ["q"]), runtime: mac) == .queue, "a queue that is not empty queues")
        #expect(ComposerMode(Self.session("s", "a/b", .working), runtime: mac) == .queue)
        #expect(ComposerMode(Self.session("s", "a/b", .asleep), runtime: mac) == .asleep)
        #expect(ComposerMode(Self.session("s", "a/b", .exited), runtime: mac) == .gone)
        #expect(ComposerMode(Self.session("s", "a/b", .needsYou), runtime: FleetRuntime(id: "r1", name: "mbp", online: false)) == .offline)
        #expect(ComposerMode(Self.session("s", "a/b", .needsYou), runtime: FleetRuntime(id: "r1", name: "mbp", online: true, held: true)) == .held)
        #expect(ComposerMode.queue.queues && !ComposerMode.reply.queues && !ComposerMode.held.canType)
        // the Mac closed its runtime cleanly: awake, the words wait at the server
        let idle = FleetRuntime(id: "r1", name: "mbp", online: false, connection: .idle, lastSeenAt: Self.nowS - 600)
        #expect(ComposerMode(Self.session("s", "a/b", .needsYou), runtime: idle) == .idle && ComposerMode.idle.canType)
    }

    @Test func anIdleMacIsAwakeWithNoSessionsNeverAsleep() {
        let idle = FleetRuntime(id: "r1", name: "mbp", online: false, connection: .idle, lastSeenAt: Self.nowS - 600)
        let note = FleetRuntimeNote(idle, now: Self.now)
        #expect(note.title == "Mac awake \u{00B7} no sessions" && note.awake && !note.online)
        #expect(!note.title.contains("asleep"))
        let away = FleetRuntimeNote(FleetRuntime(id: "r1", name: "mbp", online: false, connection: .offline, lastSeenAt: Self.nowS - 600), now: Self.now)
        #expect(away.title.hasPrefix("Mac asleep") && !away.awake)
        #expect(FleetCard(Fleet(rev: 1, runtimes: [idle]), available: true)?.subtitle == "No sessions")
        #expect(FleetCard(Fleet(rev: 1, runtimes: [idle]), available: true)?.attention == false, "idle never alerts")
        #expect(SessionHeader(Self.session("s1", "taisce/main", .idle), runtime: idle, now: Self.now).place == "on mbp \u{00B7} mac awake \u{00B7} no sessions")
    }

    @Test func theFeedMergesPagesAndTheStreamAndCountsWhatIsNew() async {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        UserDefaults.standard.removeObject(forKey: "fabric.lastLooked")
        func t(_ id: String, _ ended: Int, reply: String = "r") -> FabricTurn {
            FabricTurn(runtimeID: "r1", sessionID: "s1", address: "taisce/main", turnID: id, prompt: "p", reply: reply, started: ended - 1, ended: ended)
        }
        LastLooked.shared.look(key, upTo: 20)
        let feed = SessionFeed(key: key)
        let fake = FakeFabric(Self.fleet())
        fake.state.withLock { $0.turns = (1...35).map { t("t\($0)", $0) } }
        await feed.load(fake)
        #expect(feed.turns.count == 30 && feed.more && feed.turns.first?.turnID == "t6" && feed.turns.last?.turnID == "t35")
        #expect(feed.rows.count == SessionFeed.window && feed.rows.first?.id == "t16", "the newest window drawn")
        #expect(feed.firstNew == "t21" && feed.newCount == 15, "found once, among the drawn")
        await feed.showEarlier(fake)
        #expect(feed.rows.count == 30 && fake.calls.filter { $0.hasPrefix("turns") }.count == 1, "the held ones first, without the network")
        await feed.showEarlier(fake)
        #expect(feed.turns.count == 35 && !feed.more && feed.rows.count == 35)
        #expect(feed.newCount == 15 && feed.firstNew == "t21")
        // a live turn, and a Text off route's copy of one the stream carried in full
        feed.merge([t("t36", 36), t("t35", 35, reply: "")])
        #expect(feed.turns.count == 36 && feed.turns.first { $0.turnID == "t35" }?.reply == "r")
        feed.merge([FabricTurn(runtimeID: "r1", sessionID: "other", address: "x", turnID: "z", prompt: "", reply: "", started: 0, ended: 99)])
        #expect(feed.turns.count == 36, "another session's turn is not this feed's")
        feed.looked()
        #expect(LastLooked.shared.seen(key) == 36)
    }

    /// The render fix: a feed of 30 long replies parses each reply's Markdown
    /// once, when it arrives, and a redraw of the screen reuses the rows. Before,
    /// `TurnView.body` parsed every reply on every redraw (and the 30 s tick
    /// redrew them all). Prints what a redraw's worth of rows costs each way.
    @Test func longRepliesAreParsedOnceNotOnEveryRedraw() throws {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        let para = "Here is **what changed** in `crates/store/src/fabric/work.rs`: the [route](https://example.org) now reads `after=` too. "
        let turns = (1...30).map { i in
            FabricTurn(runtimeID: "r1", sessionID: "s1", address: "taisce/main", turnID: "t\(i)", prompt: "Do step \(i)",
                       reply: String(repeating: para, count: 40), started: i * 10 - 5, ended: i * 10)
        }
        let feed = SessionFeed(key: key)
        feed.now = { Self.now }
        feed.merge(turns)
        #expect(feed.rows.count == SessionFeed.window, "only the window is built")
        #expect(feed.rows.allSatisfy { $0.long && !$0.replyNodes.isEmpty })
        let clock = ContinuousClock()
        let redraws = 10
        // before: every redraw built every row (and parsed its reply) again
        let before = clock.measure {
            for _ in 0..<redraws { for t in turns { _ = TurnRow(t, now: Self.now) } }
        }
        // after: a redraw reads the feed's rows; a merge reuses unchanged ones
        let first = feed.rows
        let after = clock.measure {
            for _ in 0..<redraws { feed.merge([turns[29]]) }
        }
        #expect(feed.rows == first)
        print("TurnRender: 30 long replies x \(redraws) redraws: rebuilt every redraw \(before), kept rows \(after)")
        #expect(after < before)
        // a live event's fuller reply rebuilds that one row only
        var fuller = turns[29]
        fuller.reply += " And one more line."
        feed.merge([fuller])
        #expect(feed.rows[19].reply.hasSuffix("one more line.") && feed.rows[18] == first[18])
        // the whole reply is parsed only when asked for, once
        #expect(feed.expanded.isEmpty)
        feed.expand("t30", true)
        let whole = try #require(feed.expanded["t30"])
        #expect(whole.count > feed.rows[19].replyNodes.count || whole != feed.rows[19].replyNodes)
        feed.expand("t30", false)
        #expect(feed.expanded["t30"] == nil)
    }

    // MARK: the turn cache

    private static func turn(_ id: String, _ ended: Int, session: String = "s1", reply: String = "r", textOff: Bool = false) -> FabricTurn {
        FabricTurn(runtimeID: "r1", sessionID: session, address: "taisce/main", turnID: id, prompt: "p \(id)", reply: reply,
                   started: ended - 1, ended: ended, textOff: textOff)
    }

    @Test func aSessionOpensFromItsCacheThenFetchesOnlyWhatIsNewer() async throws {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        let cache = try Cache.inMemory()
        let fake = FakeFabric(Self.fleet())
        fake.state.withLock { $0.turns = (1...40).map { Self.turn("t\($0)", $0 * 10) } }
        // first open: nothing cached, the newest page, which is kept
        let first = SessionFeed(key: key)
        await first.load(fake, cache: cache)
        #expect(first.turns.count == 30 && first.more && !first.fromCache)
        #expect(try await cache.fabricTurns(key).turns.map(\.turnID) == (11...40).map { "t\($0)" })
        // five more end meanwhile; the server is down when the screen opens again
        fake.state.withLock { $0.turns += (41...45).map { Self.turn("t\($0)", $0 * 10) }; $0.turnsFail = true }
        let offline = SessionFeed(key: key)
        await offline.load(fake, cache: cache)
        #expect(offline.fromCache && offline.turns.count == 30 && offline.loadError == nil && offline.more, "the cache, before (and without) the network")
        // up again: only what ended after the newest held, oldest first
        fake.state.withLock { $0.turnsFail = false; $0.calls = [] }
        let second = SessionFeed(key: key)
        await second.load(fake, cache: cache)
        #expect(fake.calls == ["turns after 399"], "from a second before the newest held: \(fake.calls)")
        #expect(second.turns.map(\.turnID) == (11...45).map { "t\($0)" })
        #expect(try await cache.fabricTurns(key).turns.count == 35)
        // "Earlier turns" still pages older from the server, and keeps them
        await earliest(second, fake, cache)
        #expect(second.turns.count == 45 && !second.more)
        #expect(try await cache.fabricTurns(key).older == false)
    }

    @Test func topUpPagesUntilTheServerHasNoMore() async throws {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        let cache = try Cache.inMemory()
        try await cache.cacheFabricTurns([Self.turn("t1", 10)], olderBeyond: false)
        let fake = FakeFabric(Self.fleet())
        fake.state.withLock { $0.turns = (1...150).map { Self.turn("t\($0)", $0 * 10) } }
        let feed = SessionFeed(key: key)
        await feed.load(fake, cache: cache)
        #expect(feed.turns.count == 150 && feed.turns.last?.turnID == "t150")
        #expect(fake.calls.filter { $0.hasPrefix("turns after") }.count == 3, "30, then 100, then the rest: \(fake.calls)")
        let held = try await cache.fabricTurns(key)
        #expect(held.turns.count == Cache.fabricTurnsKept && held.turns.first?.turnID == "t51" && held.older, "the newest 100, the rest on the server")
    }

    @Test func anOldServerThatIgnoresAfterFallsBackWithoutCorruptingTheCache() async throws {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        // it reaches back to what was held: merged, gap-free
        let cache = try Cache.inMemory()
        try await cache.cacheFabricTurns((1...10).map { Self.turn("t\($0)", $0 * 10) }, olderBeyond: false)
        let fake = FakeFabric(Self.fleet())
        fake.state.withLock { $0.turns = (1...20).map { Self.turn("t\($0)", $0 * 10) }; $0.ignoresAfter = true }
        let feed = SessionFeed(key: key)
        await feed.load(fake, cache: cache)
        #expect(feed.turns.map(\.turnID) == (1...20).map { "t\($0)" } && !feed.more)
        #expect(try await cache.fabricTurns(key).turns.count == 20)
        // forty more ended since: its newest 30 do not reach back, so what lies
        // between is unknown: the feed and the cache start again from that page
        fake.state.withLock { $0.turns += (21...60).map { Self.turn("t\($0)", $0 * 10) } }
        let later = SessionFeed(key: key)
        await later.load(fake, cache: cache)
        #expect(later.turns.map(\.turnID) == (31...60).map { "t\($0)" } && later.more, "today's behaviour: the newest 30, Earlier turns pages back")
        let held = try await cache.fabricTurns(key)
        #expect(held.turns.map(\.turnID) == (31...60).map { "t\($0)" } && held.older)
        await earliest(later, fake, cache)
        #expect(later.turns.count == 60 && later.turns.first?.turnID == "t1")
    }

    @Test func theStreamsTurnsAreCachedAndTheCacheFollowsTheFleet() async throws {
        let cache = try Cache.inMemory()
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        s.turnCacheOverride = cache
        let s1 = FleetSessionKey(runtimeID: "r1", id: "s1"), s2 = FleetSessionKey(runtimeID: "r1", id: "s2")
        s.take(.turn(Self.turn("live1", 100)))
        s.take(.turn(Self.turn("live2", 110, session: "s2", reply: "secret words", textOff: true)))
        try await waitUntil { try await cache.fabricTurnCount() == 2 }
        #expect(try await cache.fabricTurns(s1).turns.map(\.turnID) == ["live1"])
        let quiet = try await cache.fabricTurns(s2).turns
        #expect(quiet.first?.reply == "" && quiet.first?.prompt == "", "Text off: the turn, never its words")
        // s1 exits: its turns go; s2's runtime goes: so do its
        var exited = Self.session("s1", "taisce/main", .exited)
        exited.updatedAt += 1
        s.take(.change(.session(exited), rev: 4))
        try await waitUntil { try await cache.fabricTurns(s1).turns.isEmpty }
        s.take(.change(.sessionGone(s2), rev: 5))
        try await waitUntil { try await cache.fabricTurnCount() == 0 }
    }

    /// "Earlier turns" until nothing older is held or on the server.
    private func earliest(_ feed: SessionFeed, _ fake: FakeFabric, _ cache: Cache?) async {
        for _ in 0..<20 where feed.hasEarlier { await feed.showEarlier(fake, cache: cache) }
    }

    private func waitUntil(_ check: () async throws -> Bool) async throws {
        for _ in 0..<200 {
            if try await check() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        Issue.record("timed out")
    }

    @Test func turnsAndRowsAreWorded() {
        let t = FabricTurn(runtimeID: "r1", sessionID: "s1", address: "a", turnID: "t", prompt: "p", reply: "r", toolCount: 9,
                           outcome: GitOutcome(filesChanged: 1, linesAdded: 6, linesRemoved: 2, commits: 1, pullRequestNumber: 412),
                           started: Self.nowS - 100, ended: Self.nowS - 60, end: .question, source: .order)
        let row = TurnRow(t, now: Self.now)
        #expect(row.activity == "9 tools \u{00B7} 1 file \u{00B7} +6 \u{2212}2 \u{00B7} 1 commit \u{00B7} PR #412")
        #expect(row.source == "from your phone" && row.ending == "Asked you" && row.endTone == .yellow)
        let item = Self.approval()
        let fleet = Self.fleet(attention: [item])
        let board = FleetBoard(fleet, now: Self.now)
        let asks = board.rows.first { $0.session.sessionID == "s1" }
        #expect(asks?.waiting == "Wants approval \u{00B7} Release" && asks?.detail == "./release.sh")
        #expect(board.rows.first { $0.session.sessionID == "s2" }?.queued == 1)
        var other = fleet
        other.sessions[1].fabric = "qompass"
        let b2 = FleetBoard(other, now: Self.now)
        #expect(b2.fabrics == ["taisce", "qompass"] && b2.only("qompass").rows.map(\.session.sessionID) == ["s2"])
        #expect(FabricWords.countdown(to: Self.nowS + 892, now: Self.now) == "15 min left")
        #expect(FabricWords.countdown(to: Self.nowS + 52, now: Self.now) == "52 s left")
        #expect(FabricWords.countdown(to: Self.nowS - 1, now: Self.now) == "Expired")
    }

    // MARK: the session screen's polish (2026-11-22)

    /// A live turn from this app run no longer skips the cache: the feed
    /// reads what is held first, merges the stream's, then asks only after
    /// the watermark (before, any live turn meant a network page and a wait).
    @Test func aSessionOpensFromItsCacheEvenWithLiveTurnsHeld() async throws {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        let cache = try Cache.inMemory()
        try await cache.cacheFabricTurns((1...10).map { Self.turn("t\($0)", $0 * 10) }, olderBeyond: false, syncedThrough: 100)
        let fake = FakeFabric(Self.fleet())
        fake.state.withLock { $0.turns = (1...12).map { Self.turn("t\($0)", $0 * 10) }; $0.turnsFail = true }
        let feed = SessionFeed(key: key)
        // the stream's turn reached the store (and this feed) before the screen opened
        feed.merge([Self.turn("t12", 120)])
        await feed.load(fake, cache: cache, live: [Self.turn("t12", 120)])
        #expect(feed.fromCache && feed.opened, "the cache was read though a live turn was held")
        #expect(feed.turns.map(\.turnID) == (1...10).map { "t\($0)" } + ["t12"])
        #expect(fake.calls == ["turns after 99"], "from the watermark, not the newest held: \(fake.calls)")
        #expect(feed.loadError == nil && feed.problem != nil, "a failed top-up with turns showing is a toast, not an empty state")
        fake.state.withLock { $0.turnsFail = false; $0.calls = [] }
        await feed.refresh(fake, cache: cache)
        #expect(feed.turns.map(\.turnID) == (1...12).map { "t\($0)" }, "t11, which the stream missed, is filled in")
    }

    /// The hole: turns that end while the app is away are never fetched by
    /// the stream; a turn the stream did carry is cached above them. After a
    /// relaunch the top-up asks from the watermark, so the hole fills and
    /// does not persist (before, it asked after the newest held: never).
    @Test func aHoleTheStreamLeftIsFilledAfterARelaunch() async throws {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        let cache = try Cache.inMemory()
        let fake = FakeFabric(Self.fleet())
        fake.state.withLock { $0.turns = (1...5).map { Self.turn("t\($0)", $0 * 10) } }
        let s = store(fake)
        s.turnCacheOverride = cache
        let first = s.feed(key)
        await first.load(fake, cache: cache)
        #expect(try await cache.fabricTurns(key).synced == 50)
        // away: t6 and t7 end unseen; the stream comes back in time for t8
        fake.state.withLock { $0.turns += (6...8).map { Self.turn("t\($0)", $0 * 10) } }
        s.take(.turn(Self.turn("t8", 80)))
        try await waitUntil { try await cache.fabricTurns(key).turns.count == 6 }
        let held = try await cache.fabricTurns(key)
        #expect(held.turns.map(\.turnID) == ["t1", "t2", "t3", "t4", "t5", "t8"] && held.synced == 50, "the stream's turn does not pass the watermark")
        // relaunch: a new feed from the cache
        fake.state.withLock { $0.calls = [] }
        let relaunched = SessionFeed(key: key)
        await relaunched.load(fake, cache: cache)
        #expect(fake.calls == ["turns after 49"], "\(fake.calls)")
        #expect(relaunched.turns.map(\.turnID) == (1...8).map { "t\($0)" }, "t6 and t7 are there")
        let after = try await cache.fabricTurns(key)
        #expect(after.turns.count == 8 && after.synced == 80, "and kept, gap-free")
    }

    /// The window: a session with 100 cached turns draws its newest 20;
    /// Earlier turns shows the next 20 from memory at once (no network),
    /// and the network only once nothing older is held.
    @Test func theWindowPagesFromWhatIsHeldBeforeTheNetwork() async throws {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        let cache = try Cache.inMemory()
        try await cache.cacheFabricTurns((1...100).map { Self.turn("t\($0)", $0 * 10) }, olderBeyond: true, syncedThrough: 1000)
        let fake = FakeFabric(Self.fleet())
        fake.state.withLock { $0.turnsFail = true }
        let feed = SessionFeed(key: key)
        await feed.open(cache)
        #expect(feed.rows.count == 20 && feed.rows.first?.id == "t81" && feed.rows.last?.id == "t100")
        #expect(feed.hiddenCount == 80 && feed.hasEarlier)
        for expected in [40, 60, 80, 100] {
            await feed.showEarlier(fake, cache: cache)
            #expect(feed.rows.count == expected)
        }
        #expect(fake.calls.isEmpty, "all of it from memory: \(fake.calls)")
        #expect(feed.hiddenCount == 0 && feed.more, "the server still has older")
        // reopened (kept by the store): the window starts again at the newest
        await feed.open(cache)
        #expect(feed.rows.count == 20 && feed.rows.last?.id == "t100")
        // a live turn joins the drawn ones; the window does not slide past what was drawn
        feed.merge([Self.turn("t101", 1010)])
        #expect(feed.rows.count == 21 && feed.rows.first?.id == "t81")
    }

    /// `before` is exclusive on `ended`: Earlier turns asks from a second
    /// after the oldest held, so a turn that ended in the same second as it
    /// is not skipped.
    @Test func earlierTurnsDoesNotSkipATurnThatEndedInTheSameSecond() async throws {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        let fake = FakeFabric(Self.fleet())
        var twin = Self.turn("twin", 100)
        twin.started = 98   // sorts before t10, which ended in the same second
        fake.state.withLock { $0.turns = (1...10).map { Self.turn("t\($0)", $0 * 10) } + [twin] }
        let feed = SessionFeed(key: key)
        feed.merge([Self.turn("t10", 100)])
        await feed.showEarlier(fake)   // nothing hidden, `more` unknown: a feed that knows of no more does nothing
        #expect(fake.calls.isEmpty)
        let cache = try Cache.inMemory()
        try await cache.cacheFabricTurns([Self.turn("t10", 100)], olderBeyond: true, syncedThrough: 100)
        let held = SessionFeed(key: key)
        await held.open(cache)
        await held.showEarlier(fake, cache: cache)
        #expect(fake.calls == ["turns 101"], "\(fake.calls)")
        #expect(held.turns.contains { $0.turnID == "twin" } && held.turns.count == 11 && !held.more)
    }

    @Test func aBackgroundTaskOrAWakeIsALineNotYourWords() {
        let block = """
        <task-notification>
        <task-id>a1b2</task-id>
        <status>completed</status>
        <summary>Agent &quot;Review Apple UI views&quot; finished</summary>
        <result>Found 3 issues: &lt;List&gt; reuse…</result>
        </task-notification>
        """
        let only = PromptDisplay(prompt: block, source: .mac)
        #expect(only.words.isEmpty && only.notes.map(\.text) == ["Background task: Agent \"Review Apple UI views\" finished"])
        // two at once, and words of the person's after them
        let two = PromptDisplay(prompt: block + "\n" + block.replacingOccurrences(of: "Review Apple UI views", with: "Tests") + "\nThen merge it", source: .mac)
        #expect(two.notes.count == 2 && two.notes[1].text.contains("\"Tests\"") && two.words == "Then merge it")
        // cut short (truncated to fit): the summary still reads, unclosed
        let cut = PromptDisplay(prompt: "<task-notification>\n<task-id>x</task-id>\n<summary>Agent \"Deploy\" fini", source: .mac)
        #expect(cut.words.isEmpty && cut.notes.map(\.text) == ["Background task: Agent \"Deploy\" fini"])
        let bare = PromptDisplay(prompt: "<task-notification><status>failed</status></task-notification>", source: .mac)
        #expect(bare.notes.map(\.text) == ["Background task failed"])
        // the mail wake line, by its words or its source
        let wake = PromptDisplay(prompt: PromptDisplay.wakeLine, source: .mac)
        #expect(wake.words.isEmpty && wake.notes.first?.symbol == "envelope")
        #expect(PromptDisplay(prompt: "anything", source: .wake).words.isEmpty)
        // anything else is the person's, untouched
        let plain = PromptDisplay(prompt: "Use <b> tags here", source: .order)
        #expect(plain.words == "Use <b> tags here" && plain.notes.isEmpty)
        // the row: no "woken for mail" under a bubble that is not there
        let row = TurnRow(FabricTurn(runtimeID: "r1", sessionID: "s1", address: "a", turnID: "w", prompt: PromptDisplay.wakeLine, reply: "Read it.",
                                     started: 1, ended: 2, source: .wake), now: Self.now)
        #expect(row.source == nil && row.promptDisplay.notes.count == 1)
    }

    @Test func repliesRenderBlockMarkdown() {
        let nodes = ReplyMarkdown.parse("""
        ## What changed

        The **deadline** moves. See `grpc.rs` and [the PR](https://example.org/1).

        1. First
        2. Second
           - nested one
           - nested two

        - [x] done

        ```swift
        let x = 1
        ```

        > quoted

        | a | b |
        |---|--:|
        | 1 | 2 |

        ---
        """)
        guard nodes.count == 8 else { Issue.record("\(nodes.count) nodes: \(nodes)"); return }
        guard case .heading(2, let h) = nodes[0] else { Issue.record("heading"); return }
        #expect(String(h.characters) == "What changed")
        guard case .paragraph(let p) = nodes[1] else { Issue.record("paragraph"); return }
        #expect(String(p.characters) == "The deadline moves. See grpc.rs and the PR.", "inline marks are styles, not characters")
        #expect(p.runs.contains { $0.link?.absoluteString == "https://example.org/1" })
        #expect(p.runs.contains { $0.inlinePresentationIntent?.contains(.stronglyEmphasized) == true })
        #expect(p.runs.contains { $0.inlinePresentationIntent?.contains(.code) == true })
        guard case .list(true, 1, let items) = nodes[2], items.count == 2 else { Issue.record("ordered list"); return }
        guard case .list(false, _, let nested) = items[1].children.last, nested.count == 2 else { Issue.record("nested list"); return }
        guard case .list(false, _, let tasks) = nodes[3], tasks.first?.checked == true else { Issue.record("task list"); return }
        guard case .code("swift", "let x = 1") = nodes[4] else { Issue.record("code: \(nodes[4])"); return }
        guard case .quote(let q) = nodes[5], case .paragraph = q.first else { Issue.record("quote"); return }
        guard case .table(let t) = nodes[6], t.header.count == 2, t.rows.count == 1, t.alignments.last == .trailing else { Issue.record("table"); return }
        guard case .rule = nodes[7] else { Issue.record("rule"); return }
    }

    /// The collapsed start of a long reply is cut where a block ends: never
    /// inside `**`, a code span or a fence.
    @Test func aLongReplyIsCutAtABlockBoundary() {
        let para = "This paragraph has **bold words** and `code` in it, and goes on for a while to fill the space. "
        let reply = String(repeating: para, count: 4) + "\n\n" + String(repeating: para, count: 4) + "\n\n" + String(repeating: para, count: 8)
        let start = try! #require(ReplyMarkdown.preview(reply, limit: 900))
        #expect(start.count <= 900 && start.hasSuffix("fill the space."), "at the end of the second paragraph")
        #expect(start.components(separatedBy: "**").count % 2 == 1 && start.components(separatedBy: "`").count % 2 == 1)
        // the limit inside a fence: cut before the fence (it starts past a third of the way)
        let fenced = String(repeating: para, count: 4) + "\n\n```go\n" + String(repeating: "fmt.Println(\"hello world\")\n", count: 60) + "```\n"
        let beforeFence = try! #require(ReplyMarkdown.preview(fenced, limit: 900))
        #expect(!beforeFence.contains("```"))
        // a reply that is all one code block: its whole lines, the fence closed again
        let code = "```go\n" + String(repeating: "fmt.Println(\"hello world\")\n", count: 60) + "```\n"
        let codeStart = try! #require(ReplyMarkdown.preview(code, limit: 900))
        #expect(codeStart.hasPrefix("```go\n") && codeStart.hasSuffix("\n```") && codeStart.components(separatedBy: "```").count == 3)
        if case .code("go", let body)? = ReplyMarkdown.parse(codeStart).first { #expect(body.hasSuffix("fmt.Println(\"hello world\")")) } else { Issue.record("a code block") }
        // one long paragraph: at a word, an open mark dropped, an ellipsis
        let run = String(repeating: "word ", count: 170) + "**never closed " + String(repeating: "x ", count: 100)
        let runStart = try! #require(ReplyMarkdown.preview(run, limit: 900))
        #expect(runStart.hasSuffix("\u{2026}") && !runStart.contains("**"))
        #expect(ReplyMarkdown.preview("short", limit: 900) == nil)
    }

    /// A second tap while the first is on its way does nothing: the action runs once.
    @Test func aSecondTapWhileTheFirstIsOnItsWayDoesNothing() async {
        let busy = InFlight()
        var runs = 0
        let gate = AsyncStream<Void>.makeStream()
        let first = Task { @MainActor in
            await busy.run("answer") {
                runs += 1
                for await _ in gate.stream { break }
                return nil
            }
        }
        await until("the first is running") { busy.isRunning("answer") }
        let second = await busy.run("answer") { runs += 1; return nil }
        #expect(second == .none, "not run")
        #expect(await busy.run("stop") { "other" } == .some("other"), "another action is its own")
        gate.continuation.yield()
        _ = await first.value
        #expect(runs == 1 && !busy.isRunning("answer"))
        #expect(await busy.run("answer") { runs += 1; return nil } == .some(nil), "free again once done")
        #expect(runs == 2)
    }

    /// A send whose post fails after the undo window keeps its words (the
    /// composer was cleared when it was held): shown not sent, with Retry.
    @Test func aFailedSendKeepsItsWordsAndRetries() async throws {
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        fake.state.withLock { $0.refuse = FabricError(status: 503, code: "unavailable", message: "down") }
        let session = try #require(s.fleet.sessions.first)
        #expect(s.command.send("Ship it after the tests", to: session, queued: false) == nil)
        await until("the post failed") { s.command.pending.first?.failure != nil }
        let failed = try #require(s.command.pending.first)
        #expect(failed.text == "Ship it after the tests" && fake.posted.isEmpty)
        #expect(s.command.notice?.text.contains("not sent") == true)
        // flushing as the app leaves does not post it behind the person's back
        await s.command.flush()
        #expect(fake.posted.isEmpty)
        await s.command.retry(failed.id)
        #expect(s.command.pending.isEmpty && fake.posted.count == 1)
        #expect(fake.posted.first?.orderID == failed.id, "the same order id: the hub keeps one order per id")
        // Edit instead: the words come back
        fake.state.withLock { $0.refuse = FabricError(status: 503, code: "unavailable", message: "down") }
        s.command.send("Second thoughts", to: session, queued: false)
        await until("failed again") { s.command.pending.first?.failure != nil }
        #expect(s.command.undo(try #require(s.command.pending.first).id) == "Second thoughts" && s.command.pending.isEmpty)
    }

    /// Verification is memoised: a screen redrawing (and the approval sheet
    /// ticking every second) reads the Keychain and verifies once per item,
    /// until trust changes.
    @Test func verificationIsMemoisedUntilTrustChanges() {
        let s = store(FakeFabric(Self.fleet()))
        let trust = CountingTrust(["r1": Self.runtimeKey.publicKey])
        s.command.trust = trust
        let item = Self.approval()
        for _ in 0..<50 {
            #expect(s.command.verifiedKind(item) == .approval)
            guard case .success = s.command.verify(item) else { Issue.record("verifies"); return }
        }
        #expect(trust.loads == 1 && s.command.verifications == 1)
        // another item (other bytes) verifies on its own
        _ = s.command.verify(Self.approval(expiresIn: 300))
        #expect(s.command.verifications == 2 && trust.loads == 1)
        // trust changed (a pairing, a forgotten Mac): verified afresh
        s.command.trustChanged()
        _ = s.command.verify(item)
        #expect(trust.loads == 2 && s.command.verifications == 3)
        // a forged item never verifies, memoised or not
        let forged = Self.approval(signer: SoftwareDeviceKey())
        for _ in 0..<3 { guard case .failure = s.command.verify(forged) else { Issue.record("forged verified"); return } }
    }

    // MARK: the review of 2026-11-21

    @Test func holdingIsOneTapAndReleasingAsksForFaceID() async throws {
        let fake = FakeFabric(Self.fleet(held: true))
        let refused = store(fake, owner: false)
        #expect(await refused.command.hold(false) == "", "Face ID cancelled: said as nothing")
        #expect(fake.posted.isEmpty, "no release signed without the owner")
        #expect(await refused.command.hold(true) == nil)
        #expect(fake.posted.last?.what == .hold(true), "holding needs no Face ID")
        let owner = store(fake, owner: true)
        #expect(await owner.command.hold(false) == nil)
        #expect(fake.posted.last?.what == .hold(false))
    }

    static func question(serverSays kind: FabricAttentionKind = .question, labels: [String] = ["Fix the test", "Skip it"]) -> FabricAttentionItem {
        let r = AttentionRequest(runtimeID: "r1", sessionID: "s1", requestID: "q1", kind: .question, text: "Fix it or skip it?",
                                 choices: [FabricChoice(id: "fix", label: labels[0]), FabricChoice(id: "skip", label: labels[1])], raisedAt: Int64(nowS))
        let bytes = r.encoded()
        return FabricAttentionItem(id: "q1", source: .runtime, runtimeID: "r1", sessionID: "s1", address: "taisce/main", kind: kind,
                                   text: "Fix it or skip it?", choices: r.choices, raisedAt: nowS,
                                   signed: SignedAttention(request: bytes, signature: try! runtimeKey.sign(.attention, bytes)))
    }

    @Test func aNotificationButtonAnswersOnlyWithTheLabelItShowed() async throws {
        let item = Self.question()
        let fake = FakeFabric(Self.fleet(attention: [item]))
        fake.state.withLock { $0.attention = [item] }
        let s = store(fake)
        let p = FabricPush.Pointer(kind: "fabric_attention", session: item.session, itemID: "q1", attentionKind: .question)
        // the push titled the "skip" button "Fix the test": nothing is signed, the app opens on it
        let lie = QuestionActions.identifier(choiceID: "skip", title: "Fix the test")
        #expect(await FabricPush.perform(p, action: lie, in: s)?.contains("Nothing was sent") == true)
        #expect(fake.posted.isEmpty)
        #expect(s.openRequest != nil, "opened in the app instead")
        // the button said what the Mac signed: answered with that label
        #expect(await FabricPush.perform(p, action: QuestionActions.identifier(choiceID: "fix", title: "Fix the test"), in: s) == nil)
        guard case .command(let c)? = fake.posted.last?.what else { Issue.record("an order"); return }
        #expect(c.verb == .answer && c.text == "Fix the test" && c.choiceID == "fix")
        // "Answer…" never signs words from the notification: it opens the app
        _ = s.takeOpenRequest()
        #expect(await FabricPush.perform(p, action: FabricPush.Action.answer, in: s) == nil)
        #expect(fake.posted.count == 1 && s.openRequest != nil)
        // a pointer naming another session than the item's
        let elsewhere = FabricPush.Pointer(kind: "fabric_attention", session: FleetSessionKey(runtimeID: "r1", id: "s2"), itemID: "q1", attentionKind: .question)
        #expect(await FabricPush.perform(elsewhere, action: QuestionActions.identifier(choiceID: "fix", title: "Fix the test"), in: s) != nil)
        #expect(fake.posted.count == 1)
        // a pointer naming no session at all signs nothing either
        let nowhere = FabricPush.Pointer(kind: "fabric_attention", session: nil, itemID: "q1", attentionKind: .question)
        #expect(await FabricPush.perform(nowhere, action: QuestionActions.identifier(choiceID: "fix", title: "Fix the test"), in: s) != nil)
        #expect(await FabricPush.perform(nowhere, action: FabricPush.Action.deny, in: s) != nil)
        #expect(fake.posted.count == 1)
    }

    @Test func theVerbsFollowWhatTheMacSignedNotWhatTheServerSays() async throws {
        // the server calls a question an approval
        let item = Self.question(serverSays: .approval)
        let fake = FakeFabric(Self.fleet(attention: [item]))
        let s = store(fake)
        #expect(s.command.verifiedKind(item) == .question)
        guard case .success(let v) = s.command.verify(item) else { Issue.record("verifies"); return }
        #expect(await s.command.approve(v) == FabricWords.problem(.wrongKind))
        #expect(await s.command.deny(v) == FabricWords.problem(.wrongKind))
        #expect(fake.posted.isEmpty, "nothing approved or denied")
        // a push calling it an approval opens its session, not the approval sheet
        let push = FabricPush.Pointer(kind: "fabric_attention", session: item.session, itemID: "q1", attentionKind: .approval)
        #expect(!FabricPush.opensApproval(push, in: s))
        let approval = Self.approval()
        let withApproval = store(FakeFabric(Self.fleet(attention: [approval])))
        #expect(FabricPush.opensApproval(FabricPush.Pointer(kind: "fabric_attention", session: approval.session, itemID: approval.id, attentionKind: .question), in: withApproval),
                "the Mac signed an approval: the sheet opens whatever the push says")
        // and a hub item is never taken for a runtime's
        let hub = FabricAttentionItem(id: "h1", source: .hub, runtimeID: "r1", sessionID: "s1", address: "taisce/main", kind: .approval, text: "", raisedAt: 1)
        #expect(s.command.verifiedKind(hub) == nil)
    }

    @Test func aSendTheAppWasAwayPastIsNeverSentLate() async throws {
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        s.command.undoWindow = .seconds(60)
        let session = Self.fleet().sessions[0]
        #expect(s.command.send("Yes, go ahead", to: session, queued: false) == nil)
        let held = try #require(s.command.pending.first)
        // the app was suspended through its deadline and comes back 31 s past it
        await s.command.flush(now: held.deadline.addingTimeInterval(PendingSend.grace + 1))
        #expect(fake.posted.isEmpty, "not sent hours later")
        #expect(s.command.pending.first?.lapsed == true, "kept, shown as not sent")
        // within the grace it would have gone; a lapsed one goes back to the box
        #expect(s.command.undo(held.id) == "Yes, go ahead" && s.command.pending.isEmpty)
        #expect(s.command.send("Yes, go ahead", to: session, queued: false) == nil)
        await s.command.flush(now: try #require(s.command.pending.first).deadline.addingTimeInterval(5))
        #expect(fake.posted.count == 1)
    }

    @Test func settingsAnotherDeviceSignedAreLookedAtBeforeBuiltOn() async throws {
        let fake = FakeFabric(Self.fleet())
        let s = store(fake)
        let other = SoftwareDeviceKey()
        var loosened = FabricDefaults.settings
        loosened.guardrails[0].enabled = false
        loosened.version = 4
        let payload = loosened.encoded()
        fake.state.withLock {
            $0.settings = FabricSettingsView(version: 4, defaults: false, settings: FabricDefaults.settings,
                                             signed: FabricSignedBody(payload: payload, deviceID: other.deviceID, signature: try! other.sign(.settings, payload)))
        }
        await s.config.load()
        let review = try #require(s.config.review)
        #expect(review.contains { $0.contains(loosened.guardrails[0].label) }, "\(review)")
        #expect(s.config.settings.guardrails[0].enabled == false, "shown as signed, not as the server's JSON")
        #expect(await s.config.save(s.config.settings)?.contains("another device") == true)
        #expect(fake.state.withLock { $0.settingsPuts.isEmpty }, "nothing signed on top unseen")
        s.config.confirmReview()
        #expect(s.config.review == nil)
        #expect(await s.config.save(s.config.settings) == nil)
        let put = try #require(fake.state.withLock { $0.settingsPuts.first })
        #expect(try FabricSettings.decode(put.payload).version == 5)
        // the server's JSON alone (no signature) is never built on
        fake.state.withLock { $0.settings = FabricSettingsView(version: 6, defaults: false, settings: FabricDefaults.settings) }
        await s.config.load()
        #expect(s.config.problem != nil)
        let refusal = await s.config.save(FabricDefaults.settings)
        #expect(refusal != nil, "nothing signed on top of unsigned settings")
    }

    // MARK: notifications

    @Test func everyCategoryTheServerSendsHasActionsHere() {
        let ids = Set(FabricPush.categories.map(\.identifier))
        for c in ["FABRIC_SESSION", "FABRIC_FINISHED", "FABRIC_APPROVAL", "FABRIC_QUESTION", "FABRIC_WAKE", "FABRIC_PAIR", "FABRIC_ASK", "FABRIC_ORDER"] {
            #expect(ids.contains(c), "\(c)")
        }
        let approval = FabricPush.categories.first { $0.identifier == "FABRIC_APPROVAL" }
        #expect(approval?.actions.map(\.identifier) == [FabricPush.Action.review, FabricPush.Action.deny], "approving opens the app; never a background button")
        #expect(approval?.actions.first?.options.contains(.foreground) == true)
    }

    @Test func aPushIsAPointer() {
        let p = FabricPush.pointer(from: ["kind": "fabric_attention", "runtime_id": "R1", "session_id": "S1", "request_id": "A1", "attention_kind": "approval"])
        #expect(p == FabricPush.Pointer(kind: "fabric_attention", session: FleetSessionKey(runtimeID: "r1", id: "s1"), itemID: "a1", attentionKind: .approval))
        #expect(FabricPush.pointer(from: ["kind": "fabric_order", "order_id": "o1", "runtime_id": "r1", "session_id": "s1"])?.itemID == "o1")
        #expect(FabricPush.pointer(from: ["kind": "share_comment"]) == nil)
    }

    @Test func aNotificationActionVerifiesBeforeItAnswers() async throws {
        let item = Self.approval()
        let fake = FakeFabric(Self.fleet(attention: [item]))
        fake.state.withLock { $0.attention = [item] }
        let s = store(fake)
        let pointer = FabricPush.Pointer(kind: "fabric_attention", session: item.session, itemID: "a1", attentionKind: .approval)
        // the act path reads the item back from the server first, then denies what the Mac signed
        await s.refreshAttention()
        guard case .success(let v) = s.command.verify(try #require(s.fleet.attention.first)) else { Issue.record("verifies"); return }
        #expect(await s.command.deny(v) == nil)
        guard case .command(let c)? = fake.posted.last?.what else { Issue.record("an order"); return }
        #expect(c.verb == .deny && c.requestHash == FabricSignature.requestHash(item.signed!.request))
        #expect(pointer.itemID == item.id)
    }
}

/// A trust list that counts its Keychain-like reads.
private final class CountingTrust: FabricTrustStore, @unchecked Sendable {
    private let inner: MemoryTrustStore
    private let count = Mutex(0)
    init(_ keys: [String: Data]) { inner = MemoryTrustStore(keys) }
    var loads: Int { count.withLock { $0 } }
    func runtimeKey(_ runtimeID: String) -> Data? { inner.runtimeKey(runtimeID) }
    func trust(_ runtimeID: String, key: Data) throws { inner.trust(runtimeID, key: key) }
    func forget(_ runtimeID: String) throws { try inner.forget(runtimeID) }
    func trusted() -> [String: Data] { inner.trusted() }
    func load() throws -> [String: Data] {
        count.withLock { $0 += 1 }
        return try inner.load()
    }
}
