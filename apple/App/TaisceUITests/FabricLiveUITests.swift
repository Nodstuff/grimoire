import XCTest

/// Agent Fabric against a real scratch SERVER daemon and the server's
/// `fake_runtime` playing a work script (`demo-work-script.json`, plus a
/// session that keeps working): sign in through the relay, pair with the
/// code the fake runtime printed, read a session's turns, approve its
/// release with Face ID, reply, queue and Stop, answer a question, Hold all
/// and Release, open Talk. Every order is signed here and checked by the hub
/// and the runtime. Skipped unless `TEST_RUNNER_TAISCE_UI_URL` names the
/// daemon, `TEST_RUNNER_TAISCE_UI_SIGNIN_RELAY` its relay and
/// `TEST_RUNNER_TAISCE_FABRIC_PAIR_CODE` the code. Never a real server.
///
/// Face ID: with `TEST_RUNNER_TAISCE_FABRIC_FACEID=<dir>` the approval asks
/// the simulator's enrolled face for real; the test drops `faceid-request`
/// in that directory when the Face ID sheet is up, and the harness answers
/// with `simctl spawn <udid> notifyutil -p com.apple.BiometricKit_Sim.pearl.match`
/// (enroll first: `notifyutil -s com.apple.BiometricKit.enrollmentChanged 1`
/// then `-p` the same name). Without it, `-fabricOwnerCheck pass` stands in.
final class FabricLiveUITests: XCTestCase {
    static let taisceMain = "taisce/main"
    static let question = "qompass/mcp-dx"
    /// a session the work script leaves working, to queue to and Stop
    static let working = "taisce/fabric-ui"

    func env(_ k: String) -> String? { ProcessInfo.processInfo.environment[k].flatMap { $0.isEmpty ? nil : $0 } }

    func el(_ app: XCUIApplication, _ id: String) -> XCUIElement { app.descendants(matching: .any).matching(identifier: id).firstMatch }

    func text(_ app: XCUIApplication, containing s: String) -> XCUIElement {
        app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", s)).firstMatch
    }

    func waitFor(_ what: String, timeout: TimeInterval = 20, _ check: () -> Bool) {
        let end = Date().addingTimeInterval(timeout)
        while !check(), Date() < end { RunLoop.current.run(until: Date().addingTimeInterval(0.25)) }
        XCTAssertTrue(check(), what)
    }

    func shot(_ app: XCUIApplication, _ name: String) {
        let a = XCTAttachment(screenshot: app.screenshot())
        a.name = name
        a.lifetime = .keepAlways
        add(a)
        if let dir = env("TAISCE_UI_SHOTS") {
            try? app.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("live-\(name).png"))
        }
    }

    func back(_ app: XCUIApplication) { app.navigationBars.buttons.element(boundBy: 0).tap() }

    func open(_ app: XCUIApplication, _ address: String) {
        let row = el(app, "agents.row.\(address)")
        if !row.waitForExistence(timeout: 10), el(app, "agents.resting").exists { el(app, "agents.resting").tap() }
        XCTAssertTrue(row.waitForExistence(timeout: 10), "\(address) is in the fleet")
        row.tap()
        // a long session opens at its newest turn: the header may be above, not drawn
        waitFor("\(address) opened", timeout: 10) { ["session.header", "session.composer", "session.resume"].contains { self.el(app, $0).exists } }
    }

    /// Allow once: the system asks for Face ID (and, the first time, whether
    /// Taisce may use it); the harness plays the enrolled face's match.
    func confirmWithFaceID(_ app: XCUIApplication, signal dir: String) {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let permit = springboard.buttons["OK"]
        if permit.waitForExistence(timeout: 4) { permit.tap() }
        let request = URL(fileURLWithPath: dir).appendingPathComponent("faceid-request")
        // the Face ID sheet is the system's; give it a moment to be up before the match
        RunLoop.current.run(until: Date().addingTimeInterval(1.5))
        shot(app, "05-approval-faceid")
        FileManager.default.createFile(atPath: request.path, contents: Data("match".utf8))
        waitFor("the harness played the match", timeout: 20) { !FileManager.default.fileExists(atPath: request.path) }
    }

    func testTheRealHubAndRuntime() throws {
        guard let url = env("TAISCE_UI_URL"), let relay = env("TAISCE_UI_SIGNIN_RELAY"), let code = env("TAISCE_FABRIC_PAIR_CODE") else {
            throw XCTSkip("needs a scratch daemon, its sign-in relay and a fake runtime's pairing code")
        }
        continueAfterFailure = false
        let faceID = env("TAISCE_FABRIC_FACEID")
        let app = XCUIApplication()
        app.launchEnvironment = ["TAISCE_UI_TEST": "1", "TAISCE_UI_SIGNIN_RELAY": relay]
        app.launchArguments = ["-serverURL", url, "-tab", "agents"] + (faceID == nil ? ["-fabricOwnerCheck", "pass"] : [])
        app.launch()
        XCTAssertTrue(el(app, "agents.row.\(Self.taisceMain)").waitForExistence(timeout: 90), "signed in; the fleet shows")
        shot(app, "01-agents")

        // pair: paste the code, compare the fingerprint, the Mac confirms
        el(app, "agents.settings").tap()
        el(app, "settings.fabric.pair").tap()
        let field = el(app, "pairing.code")
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.tap()
        field.typeText(code)
        el(app, "pairing.submit").tap()
        XCTAssertTrue(el(app, "pairing.fingerprint").waitForExistence(timeout: 15), "this phone's fingerprint to compare at the Mac")
        shot(app, "02-pairing-fingerprint")
        XCTAssertTrue(el(app, "pairing.paired").waitForExistence(timeout: 30), "the runtime opened the seal and confirmed")
        shot(app, "03-paired")
        app.navigationBars.buttons["Done"].firstMatch.tap()
        let mac = el(app, "settings.fabric.mac.mbp")
        waitFor("Settings › Agents says paired") { mac.exists && mac.label.contains("Paired") }
        back(app)

        // a session's turns, as the runtime reported them
        open(app, Self.taisceMain)
        XCTAssertTrue(el(app, "turn.0196f3a0-0000-7000-8000-0000000000f2").waitForExistence(timeout: 20), "the turn the runtime reported")
        shot(app, "04-session-turns")

        // the approval: verified against the code's key, allowed once with Face ID
        let review = el(app, "attention.review")
        XCTAssertTrue(review.waitForExistence(timeout: 20))
        review.tap()
        XCTAssertTrue(el(app, "approval.command").waitForExistence(timeout: 10), "the Mac-signed command, verified")
        shot(app, "05-approval")
        el(app, "approval.allow").tap()
        if let faceID { confirmWithFaceID(app, signal: faceID) }
        waitFor("approved and cleared", timeout: 30) { !review.exists }
        shot(app, "06-approved")

        // a reply: held five seconds, then signed, and played back as the turn it started
        let composer = el(app, "session.composer")
        composer.tap()
        composer.typeText("Yes, open the pull request")
        el(app, "session.send").tap()
        XCTAssertTrue(el(app, "session.pending").waitForExistence(timeout: 3), "held with Undo")
        shot(app, "07-reply-held")
        waitFor("the runtime played the reply back as a turn", timeout: 60) {
            self.text(app, containing: "done: Yes, open the pull request").exists && !self.el(app, "session.pending").exists
        }
        shot(app, "08-reply-turn")
        back(app)

        // the question, answered by its choice
        open(app, Self.question)
        let fix = el(app, "question.choice.fix")
        XCTAssertTrue(fix.waitForExistence(timeout: 20), "the question verified against the code's key")
        shot(app, "09-question")
        fix.tap()
        waitFor("the runtime answered it", timeout: 30) { !fix.exists }
        XCTAssertTrue(text(app, containing: "going with").waitForExistence(timeout: 30), "its answer played back as a turn")
        shot(app, "10-answered")
        back(app)

        // queue behind a working session, then Stop it
        open(app, Self.working)
        let queueBox = el(app, "session.composer")
        queueBox.tap()
        queueBox.typeText("Then run the UI tests")
        let queue = el(app, "session.send")
        XCTAssertEqual(queue.label, "Queue", "a working session queues")
        queue.tap()
        XCTAssertTrue(el(app, "session.pending").waitForExistence(timeout: 3))
        shot(app, "11-queued")
        let stop = el(app, "session.stop")
        XCTAssertTrue(stop.exists, "Stop while it works")
        stop.tap()
        waitFor("the queued message went and came back as a turn", timeout: 60) {
            self.text(app, containing: "done: Then run the UI tests").exists && !self.el(app, "session.pending").exists
        }
        shot(app, "12-stopped-and-queued-turn")
        back(app)

        // Hold all, then Release: one signed order per Mac each way
        el(app, "agents.hold").tap()
        let hold = app.buttons.matching(NSPredicate(format: "label == %@", "Hold all")).lastMatch
        XCTAssertTrue(hold.waitForExistence(timeout: 5))
        hold.tap()
        XCTAssertTrue(el(app, "agents.held").waitForExistence(timeout: 30), "the Mac reported itself held")
        shot(app, "13-held")
        el(app, "agents.hold").tap()
        waitFor("released", timeout: 30) { !self.el(app, "agents.held").exists }
        shot(app, "14-released")

        // Talk: the agent's ask to another
        el(app, "agents.talk").tap()
        let conversation = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'talk.row.'")).firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 20), "a conversation")
        XCTAssertTrue(conversation.identifier.contains("workbox/main") && conversation.identifier.contains(Self.taisceMain), conversation.identifier)
        shot(app, "15-talk")
        conversation.tap()
        let message = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'message.'")).firstMatch
        XCTAssertTrue(message.waitForExistence(timeout: 20), "the ask")
        shot(app, "16-conversation")
    }
}
