import TaisceKit
import XCTest

/// Agent Fabric Slices 2–5 against the stand-in server (`FleetStubServer`
/// with `FabricStub`, a hub and one runtime in the runner): pairing by
/// code, a session's turns, a reply that waits out its undo window and goes
/// signed, a working session's Stop and queue, an approval shown from the
/// Mac's signature and allowed once, a forged one refused, a question
/// answered by its choice, Talk's continue and wake, signed settings, Hold
/// all. Every order the app posts is checked by the stub as a runtime would
/// (paired key, canonical bytes, request hash). Screenshots are attachments,
/// and files when `TEST_RUNNER_TAISCE_UI_SHOTS` names a folder.
final class FabricUITests: XCTestCase {
    var server: FleetStubServer?
    var stub: FabricStub?

    override func setUpWithError() throws {
        #if targetEnvironment(macCatalyst)
        throw XCTSkip("the stand-in server needs a runner that may listen (a simulator)")
        #else
        continueAfterFailure = false
        if UIDevice.current.userInterfaceIdiom == .pad { XCUIDevice.shared.orientation = .landscapeLeft }
        #endif
    }

    var isPad: Bool { UIDevice.current.userInterfaceIdiom == .pad }

    // MARK: the fleet these tests use

    static let approvalText = "./release.sh --tag v2.14.0"

    static func session(_ n: Int, _ address: String, _ state: String, fabric: String, account: String = "personal", turn: String = "",
                        queue: [String] = [], tool: String = "", note: String = "", last: String = "", context: Int = 44, ago: Int = 120,
                        project: String? = nil) -> [String: Any] {
        var s = StubFleet.session(n, address, state, tool: tool, note: note, last: last, account: account, context: context, ago: ago)
        s["fabric"] = fabric
        s["project"] = project ?? String(address.split(separator: "/").first ?? "")
        s["worktree"] = String(address.split(separator: "/").last ?? "")
        s["branch"] = n == 1 ? "agent-fabric" : "main"
        s["turn_id"] = turn
        s["queue"] = queue
        s["queue_paused"] = ""
        s["following"] = false
        s["text_off"] = false
        s["fabric_id"] = ""
        return s
    }

    static var sessions: [[String: Any]] {
        [
            session(3, "workbox/main", "needs_you", fabric: "release", account: "work", last: "Tests pass, notarised. Releasing."),
            session(1, "taisce/main", "needs_you", fabric: "runtime-channel", turn: "t3", last: "Shall I open the pull request?"),
            session(2, "qompass/mcp-dx", "working", fabric: "qompass", account: "work", turn: "w2", queue: ["q1"], tool: "Bash",
                    note: "go test ./internal/mcp/...", context: 63, ago: 360),
            session(4, "qompass/nimbus", "finished", fabric: "qompass", account: "work", last: "PR #412 opened, CI green", ago: 1200),
            session(5, "notes/main", "idle", fabric: "notes"),
        ]
    }

    func launch(attention: (FabricStub) -> [[String: Any]] = { _ in [] }, sessions: [[String: Any]] = FabricUITests.sessions,
                tab: String = "agents", setup: (FabricStub) -> Void = { _ in }) throws -> (XCUIApplication, FabricStub) {
        let server = try FleetStubServer(fleet: StubFleet.fleet(sessions: sessions))
        let stub = FabricStub(server: server)
        server.mutate { f in
            var runtimes = f["runtimes"] as? [[String: Any]] ?? []
            runtimes[0]["held"] = false
            runtimes[0]["devices"] = []
            runtimes[0]["settings_version"] = 0
            runtimes[0]["present"] = false
            runtimes[0]["public_key"] = stub.runtimeKey.publicKey.base64EncodedString()
            f["runtimes"] = runtimes
            f["attention"] = attention(stub)
        }
        stub.turns[StubFleet.id(1)] = [
            FabricStub.turn(1, address: "taisce/main", id: "t1", prompt: "Why does the stream die after an hour?",
                            reply: "The heartbeat deadline moves on every recheck tick, so a dead stream looks alive. I'll make it move only when a message arrives.",
                            tools: 4, ago: 3600),
            FabricStub.turn(1, address: "taisce/main", id: "t2", prompt: "Do it, and keep the recheck.",
                            reply: "Done in `grpc.rs`. The recheck still runs every 5 s.", tools: 6, added: 6, removed: 2, files: 1, ago: 1800),
            FabricStub.turn(1, address: "taisce/main", id: "t3", prompt: "Fix the heartbeat deadline and rerun the tests",
                            reply: "The deadline now moves only when a message arrives, so the recheck tick can't keep a dead stream alive. All 12 suites pass.\n\nShall I open the pull request?",
                            tools: 9, added: 6, removed: 2, files: 1, commits: 1, ago: 120, end: "question"),
        ]
        stub.turns[StubFleet.id(2)] = [
            FabricStub.turn(2, address: "qompass/mcp-dx", id: "w1", prompt: "Add the style_guide tool and its isolation test",
                            reply: "I'll add it next to read_doc and pin both cases in the isolation test.", tools: 5, added: 48, removed: 3, files: 2, ago: 400,
                            source: "order"),
        ]
        stub.seedOrder("q1", session: 2, text: "Then open a draft PR and link the ticket")
        setup(stub)
        self.server = server
        self.stub = stub
        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", server.url, "-tab", tab, "-fabricOwnerCheck", "pass"]
        app.launch()
        return (app, stub)
    }

    // MARK: helpers

    func el(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    func row(_ app: XCUIApplication, _ address: String) -> XCUIElement { el(app, "agents.row.\(address)") }

    func waitFor(_ what: String, timeout: TimeInterval = 15, _ check: () -> Bool) {
        let end = Date().addingTimeInterval(timeout)
        while !check(), Date() < end { RunLoop.current.run(until: Date().addingTimeInterval(0.2)) }
        XCTAssertTrue(check(), what)
    }

    func shot(_ app: XCUIApplication, _ name: String) {
        let a = XCTAttachment(screenshot: app.screenshot())
        a.name = name
        a.lifetime = .keepAlways
        add(a)
        if let dir = ProcessInfo.processInfo.environment["TAISCE_UI_SHOTS"] {
            let device = isPad ? "ipad" : "iphone"
            try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
            try? app.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(device)-\(name).png"))
        }
    }

    /// Pair the app with the stub's runtime: Agents › settings › Pair with a
    /// Mac, the code pasted, the fingerprint shown, then confirmed "at the Mac".
    func pair(_ app: XCUIApplication, _ stub: FabricStub, shoot: Bool = false) {
        XCTAssertTrue(row(app, "taisce/main").waitForExistence(timeout: 20), "the fleet shows")
        el(app, "agents.settings").tap()
        let pairButton = el(app, "settings.fabric.pair")
        XCTAssertTrue(pairButton.waitForExistence(timeout: 10))
        pairButton.tap()
        let field = el(app, "pairing.code")
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.tap()
        field.typeText(stub.pairingCode)
        if shoot { shot(app, "pairing-code") }
        el(app, "pairing.submit").tap()
        let fingerprint = el(app, "pairing.fingerprint")
        XCTAssertTrue(fingerprint.waitForExistence(timeout: 10), "the fingerprint to compare at the Mac")
        if shoot { shot(app, "pairing-compare") }
        XCTAssertTrue(el(app, "pairing.paired").waitForExistence(timeout: 15), "confirmed at the Mac")
        if shoot { shot(app, "pairing-paired") }
        app.navigationBars.buttons["Done"].firstMatch.tap()
        let mac = el(app, "settings.fabric.mac.mbp")
        waitFor("Settings › Agents says paired") { mac.label.contains("Paired") }
        if shoot { shot(app, "settings-agents") }
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(row(app, "taisce/main").waitForExistence(timeout: 10))
    }

    func open(_ app: XCUIApplication, _ address: String) {
        let r = row(app, address)
        XCTAssertTrue(r.waitForExistence(timeout: 15))
        r.tap()
        XCTAssertTrue(el(app, "session.header").waitForExistence(timeout: 10), "\(address) opened")
    }

    // MARK: tests

    func testPairingThenAReplyWaitsOutItsUndoAndGoesSigned() throws {
        let (app, stub) = try launch()
        XCTAssertTrue(row(app, "taisce/main").waitForExistence(timeout: 20))
        XCTAssertTrue(el(app, "agents.talk").exists, "Talk is on the list")
        shot(app, "agents-list")
        // the fabric filter
        let release = el(app, "agents.fabric.release")
        if release.exists {
            release.tap()
            waitFor("only the release fabric") { !self.row(app, "taisce/main").exists }
            XCTAssertTrue(row(app, "workbox/main").exists)
            shot(app, "agents-filtered")
            release.tap()
            XCTAssertTrue(row(app, "taisce/main").waitForExistence(timeout: 5))
        }
        pair(app, stub, shoot: true)

        open(app, "taisce/main")
        XCTAssertTrue(el(app, "turn.t3").waitForExistence(timeout: 10), "the turns load")
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS 'Shall I open the pull request?'")).firstMatch.exists)
        XCTAssertFalse(el(app, "session.pairNote").exists, "paired: no note")
        shot(app, "session-needs-you")

        // a quick reply, taken back within its five seconds
        let quick = el(app, "session.quick.Yes, go ahead")
        XCTAssertTrue(quick.waitForExistence(timeout: 5))
        quick.tap()
        let undo = el(app, "session.undo")
        XCTAssertTrue(undo.waitForExistence(timeout: 3))
        shot(app, "session-undo")
        undo.tap()
        let composer = el(app, "session.composer")
        waitFor("the words are back in the box") { (composer.value as? String)?.contains("Yes, go ahead") == true }
        RunLoop.current.run(until: Date().addingTimeInterval(6))
        XCTAssertTrue(stub.postedOrders.isEmpty, "nothing was sent")

        // typed and sent: after the window, one signed order
        composer.tap()
        composer.typeText(" and link the ticket")
        el(app, "session.send").tap()
        XCTAssertTrue(el(app, "session.pending").waitForExistence(timeout: 3))
        waitFor("posted after five seconds", timeout: 12) { !stub.postedOrders.isEmpty }
        let order = try XCTUnwrap(stub.lastOrder)
        guard case .command(let c) = order.what else { return XCTFail("a command") }
        XCTAssertEqual(c.verb, .send)
        XCTAssertEqual(c.text, "Yes, go ahead and link the ticket")
        XCTAssertEqual(c.afterTurn, "t3", "the turn being read")
        XCTAssertFalse(c.queued, "a reply to a waiting session is not queued")
        XCTAssertEqual(order.runtimeID, StubFleet.runtime)
        XCTAssertGreaterThan(order.expiresAt, order.issuedAt)
    }

    func testAWorkingSessionStopsAndItsQueuedMessageCancels() throws {
        let (app, stub) = try launch()
        pair(app, stub)
        open(app, "qompass/mcp-dx")
        XCTAssertTrue(el(app, "session.running").waitForExistence(timeout: 10), "running now")
        let queued = el(app, "session.queued")
        XCTAssertTrue(queued.waitForExistence(timeout: 10))
        waitFor("the queued text") { app.staticTexts["Then open a draft PR and link the ticket"].exists }
        XCTAssertEqual(el(app, "session.composer").placeholderValue, "Queue a message")
        shot(app, "session-working")
        // a queued message is cancelled with a signed order naming it
        el(app, "session.queued.cancel").tap()
        waitFor("a cancel was posted") { stub.postedOrders.contains { if case .command(let c) = $0.what { c.verb == .cancel && c.orderIDs == ["q1"] } else { false } } }
        waitFor("the card goes") { !queued.exists }
        // Stop
        el(app, "session.stop").tap()
        waitFor("an interrupt was posted") { stub.postedOrders.contains { if case .command(let c) = $0.what { c.verb == .interrupt } else { false } } }
        // a new message queues behind the running turn
        let composer = el(app, "session.composer")
        composer.tap()
        composer.typeText("Then open a draft PR")
        el(app, "session.send").tap()
        waitFor("queued, after its undo window", timeout: 12) { stub.postedOrders.contains { if case .command(let c) = $0.what { c.verb == .send && c.queued } else { false } } }
    }

    func testAnApprovalIsShownFromTheMacsSignatureAndAllowedOnce() throws {
        let (app, stub) = try launch(attention: { stub in
            [stub.attention(1, session: 3, address: "workbox/main", kind: .approval, text: FabricUITests.approvalText, label: "Release and deploy scripts")]
        })
        let r = row(app, "workbox/main")
        XCTAssertTrue(r.waitForExistence(timeout: 20))
        XCTAssertTrue(r.label.contains("Wants approval"), r.label)
        pair(app, stub)
        open(app, "workbox/main")
        let review = el(app, "attention.review")
        XCTAssertTrue(review.waitForExistence(timeout: 10))
        shot(app, "session-approval-card")
        review.tap()
        let command = el(app, "approval.command")
        XCTAssertTrue(command.waitForExistence(timeout: 10), "verified against the key from the pairing code")
        XCTAssertTrue(command.label.contains(Self.approvalText) || app.staticTexts[Self.approvalText].exists)
        XCTAssertTrue(el(app, "approval.expiry").label.contains("Expires in"))
        shot(app, "approval")
        el(app, "approval.allow").tap()
        waitFor("an approve was posted") { stub.postedOrders.contains { if case .command(let c) = $0.what { c.verb == .approve } else { false } } }
        // the stub took it only because the hash named the bytes it signed; the item clears
        waitFor("the sheet closes") { !command.exists }
        waitFor("the card goes") { !review.exists }
    }

    func testAForgedApprovalIsNotShown() throws {
        let forger = SoftwareDeviceKey()
        let (app, stub) = try launch(attention: { stub in
            [stub.attention(1, session: 3, address: "workbox/main", kind: .approval, text: "rm -rf ~/", label: "Release and deploy scripts", forger: forger)]
        })
        pair(app, stub)
        open(app, "workbox/main")
        el(app, "attention.review").tap()
        XCTAssertTrue(el(app, "approval.unverified").waitForExistence(timeout: 10), "not signed by the Mac: not shown")
        XCTAssertFalse(el(app, "approval.allow").exists)
        shot(app, "approval-forged")
    }

    func testAQuestionIsAnsweredByItsChoice() throws {
        let (app, stub) = try launch(attention: { stub in
            [stub.attention(2, session: 1, address: "taisce/main", kind: .question,
                            text: "The retry test is flaky on CI. Fix the test, or skip it and file a ticket?",
                            choices: [FabricChoice(id: "fix", label: "Fix the test"), FabricChoice(id: "skip", label: "Skip it, file a ticket")])]
        })
        pair(app, stub)
        open(app, "taisce/main")
        let fix = el(app, "question.choice.fix")
        XCTAssertTrue(fix.waitForExistence(timeout: 10))
        shot(app, "session-question")
        fix.tap()
        waitFor("an answer was posted") { stub.postedOrders.contains { if case .command(let c) = $0.what { c.verb == .answer } else { false } } }
        guard case .command(let c)? = stub.lastOrder?.what else { return XCTFail() }
        XCTAssertEqual(c.choiceID, "fix")
        XCTAssertEqual(c.text, "Fix the test", "the label the Mac signed is what is typed")
        waitFor("the card goes") { !fix.exists }
    }

    func testTalkSettlesAPausedPairAndAWakeOffer() throws {
        let (app, stub) = try launch(attention: { _ in
            [
                FabricStub.hubItem("h1", kind: "wake_offer", session: 4, address: "qompass/nimbus", from: "qompass/mcp-dx",
                                   text: "Which IAM role does the nimbus deploy assume? I need the same one for the MCP smoke test.", conversation: "c2"),
                FabricStub.hubItem("h2", kind: "pair_paused", session: 1, address: "taisce/main", from: "workbox/main",
                                   text: "Should Accounts carry reset times?", conversation: "c1"),
            ]
        }, setup: { stub in
            let now = Int(Date().timeIntervalSince1970)
            stub.conversations = [
                ["id": "c1", "a": FabricStub.party(3, "workbox/main"), "b": FabricStub.party(1, "taisce/main"), "count": 12, "total": 14, "last_at": now - 40, "paused": true, "unanswered_asks": 1],
                ["id": "c2", "a": FabricStub.party(2, "qompass/mcp-dx"), "b": FabricStub.party(4, "qompass/nimbus"), "count": 1, "total": 1, "last_at": now - 300, "paused": false, "unanswered_asks": 1],
            ]
            stub.messages["c1"] = [
                ["id": "m2", "conversation_id": "c1", "from": FabricStub.party(1, "taisce/main"), "to": FabricStub.party(3, "workbox/main"), "kind": "reply",
                 "body": "Send both. The phone shows \u{201C}resets 14:00\u{201D} under the bar; omit a field you don't know.", "doc": "", "reply_to": "m1", "sent_at": now - 50,
                 "read_at": NSNull(), "state": "unread", "bounce_reason": NSNull(), "answered": false],
                ["id": "m1", "conversation_id": "c1", "from": FabricStub.party(3, "workbox/main"), "to": FabricStub.party(1, "taisce/main"), "kind": "ask",
                 "body": NSNull(), "doc": "", "reply_to": "", "sent_at": now - 90, "read_at": now - 85, "state": "read", "bounce_reason": NSNull(), "answered": true],
            ]
        })
        pair(app, stub)
        el(app, "agents.talk").tap()
        let wake = el(app, "talk.item.wake_offer")
        XCTAssertTrue(wake.waitForExistence(timeout: 10))
        XCTAssertTrue(el(app, "talk.item.pair_paused").exists)
        shot(app, "talk")
        el(app, "talk.continue").tap()
        waitFor("continue was posted") { stub.calls.contains { $0.method == "POST" && $0.path.hasSuffix("/conversations/c1/continue") } }
        waitFor("the paused card goes") { !self.el(app, "talk.item.pair_paused").exists }
        el(app, "talk.wake").tap()
        waitFor("a wake order was posted") { stub.postedOrders.contains { if case .command(let c) = $0.what { c.verb == .wake } else { false } } }
        // the conversation
        let conversation = el(app, "talk.row.workbox/main~taisce/main")
        XCTAssertTrue(conversation.waitForExistence(timeout: 10))
        conversation.tap()
        XCTAssertTrue(el(app, "message.m2").waitForExistence(timeout: 10))
        XCTAssertTrue(el(app, "message.m1").label.contains("text deleted"), "a read message has no text")
        shot(app, "conversation")
    }

    func testGuardrailsAndAFabricAreSignedAsTheNextVersion() throws {
        let (app, stub) = try launch()
        pair(app, stub)
        el(app, "agents.settings").tap()
        let force = el(app, "guardrail.force-push")
        XCTAssertTrue(force.waitForExistence(timeout: 10), "the defaults, before anything is signed")
        // a new fabric
        let add = el(app, "fabric.add")
        while !add.isHittable { app.swipeUp() }
        add.tap()
        let name = el(app, "fabricEditor.name")
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        name.tap()
        name.typeText("release-train")
        el(app, "fabricEditor.purpose").tap()
        el(app, "fabricEditor.purpose").typeText("Ship Workbox 2.14 and the app together.")
        shot(app, "fabric-editor")
        el(app, "fabricEditor.done").tap()
        let save = el(app, "settings.fabric.save")
        XCTAssertTrue(save.waitForExistence(timeout: 5))
        shot(app, "settings-unsaved")
        save.tap()
        waitFor("version 1 was put, signed") { !stub.settingsSaved.isEmpty }
        let saved = try XCTUnwrap(stub.settingsSaved.last)
        XCTAssertEqual(saved.version, 1)
        XCTAssertEqual(saved.fabrics.map(\.name), ["release-train"])
        XCTAssertEqual(saved.guardrails.count, 12, "the defaults kept")
        // a guardrail's editor
        app.swipeDown()
        let g = el(app, "guardrail.push-main")
        while !g.isHittable { app.swipeDown() }
        app.staticTexts["Push to main"].firstMatch.tap()
        XCTAssertTrue(el(app, "guardrail.pattern").waitForExistence(timeout: 5))
        shot(app, "guardrail-editor")
    }

    func testHoldAllIsOneSignedOrderPerMac() throws {
        let (app, stub) = try launch()
        pair(app, stub)
        el(app, "agents.hold").tap()
        let hold = app.buttons.matching(NSPredicate(format: "label == %@", "Hold all")).lastMatch
        XCTAssertTrue(hold.waitForExistence(timeout: 5))
        hold.tap()
        waitFor("a hold was posted") { stub.postedOrders.contains { $0.what == .hold(true) } }
        XCTAssertTrue(el(app, "agents.held").waitForExistence(timeout: 10), "the list says so")
        shot(app, "agents-held")
        el(app, "agents.hold").tap()
        waitFor("released") { stub.postedOrders.contains { $0.what == .hold(false) } }
        waitFor("the banner goes") { !self.el(app, "agents.held").exists }
    }
}

extension XCUIElementQuery {
    var lastMatch: XCUIElement { element(boundBy: max(0, count - 1)) }
}
