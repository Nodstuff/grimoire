import XCTest

/// Agent Fabric end to end against a real scratch SERVER daemon and a real
/// Workbox (Debug Preview, live) hosting real Claude Code sessions: sign in
/// through the relay, pair with the code Workbox showed (the harness confirms
/// the fingerprint at the Mac), open a real session and read its turns, reply
/// and see the session answer, have it run a command the force-push guardrail
/// holds and allow it once, Hold all and Release, and open Talk.
/// Skipped unless `TEST_RUNNER_TAISCE_UI_URL`, `TEST_RUNNER_TAISCE_UI_SIGNIN_RELAY`,
/// `TEST_RUNNER_TAISCE_FABRIC_PAIR_CODE` and `TEST_RUNNER_TAISCE_FABRIC_SESSION`
/// (the address of an idle real session) are set. Never a real server.
final class FabricWorkboxLiveUITests: XCTestCase {
    func env(_ k: String) -> String? { ProcessInfo.processInfo.environment[k].flatMap { $0.isEmpty ? nil : $0 } }

    func el(_ app: XCUIApplication, _ id: String) -> XCUIElement { app.descendants(matching: .any).matching(identifier: id).firstMatch }

    func text(_ app: XCUIApplication, containing s: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", s)).firstMatch
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
            try? app.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("wb-\(name).png"))
        }
    }

    func back(_ app: XCUIApplication) { app.navigationBars.buttons.element(boundBy: 0).tap() }

    /// An idle session sits under the folded "Idle, asleep, exited" group.
    func row(_ app: XCUIApplication, _ address: String, timeout: TimeInterval) -> XCUIElement {
        let row = el(app, "agents.row.\(address)")
        if row.waitForExistence(timeout: 3) { return row }
        // unfold the resting group once (a second tap folds it again)
        let resting = el(app, "agents.resting")
        if resting.waitForExistence(timeout: 5) { resting.tap() }
        // a lazy list builds rows only on screen: scroll down until it appears
        let end = Date().addingTimeInterval(timeout)
        var swipes = 0
        while !row.exists, Date() < end {
            if swipes < 8 { app.swipeUp(); swipes += 1 }
            RunLoop.current.run(until: Date().addingTimeInterval(1))
        }
        return row
    }

    func send(_ app: XCUIApplication, _ words: String) {
        let composer = el(app, "session.composer")
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap()
        composer.typeText(words)
        el(app, "session.send").tap()
    }

    func testARealWorkboxSession() throws {
        guard let url = env("TAISCE_UI_URL"), let relay = env("TAISCE_UI_SIGNIN_RELAY"),
              let code = env("TAISCE_FABRIC_PAIR_CODE"), let session = env("TAISCE_FABRIC_SESSION") else {
            throw XCTSkip("needs a scratch daemon, its relay, a live Workbox's pairing code and a session address")
        }
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchEnvironment = ["TAISCE_UI_TEST": "1", "TAISCE_UI_SIGNIN_RELAY": relay]
        app.launchArguments = ["-serverURL", url, "-tab", "agents", "-fabricOwnerCheck", "pass"]
        app.launch()
        XCTAssertTrue(el(app, "agents.settings").waitForExistence(timeout: 90), "signed in; Agents shows")
        var sessionRow = row(app, session, timeout: 30)
        XCTAssertTrue(sessionRow.exists, "the real session is in the fleet")
        shot(app, "01-agents")

        // pair: paste Workbox's code; the harness confirms the fingerprint at the Mac
        el(app, "agents.settings").tap()
        el(app, "settings.fabric.pair").tap()
        let field = el(app, "pairing.code")
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.tap()
        field.typeText(code)
        el(app, "pairing.submit").tap()
        XCTAssertTrue(el(app, "pairing.fingerprint").waitForExistence(timeout: 15), "this phone's fingerprint to compare at the Mac")
        shot(app, "02-pairing-fingerprint")
        XCTAssertTrue(el(app, "pairing.paired").waitForExistence(timeout: 60), "Workbox opened the seal and the Mac confirmed")
        shot(app, "03-paired")
        app.navigationBars.buttons["Done"].firstMatch.tap()
        back(app)

        // a reply from the phone, typed into the real pane; the session answers
        sessionRow = row(app, session, timeout: 15)
        XCTAssertTrue(sessionRow.exists)
        sessionRow.tap()
        XCTAssertTrue(el(app, "session.header").waitForExistence(timeout: 10))
        shot(app, "04-session")
        send(app, "Reply with just the word PEARTREE and nothing else.")
        waitFor("the real session answered the phone's reply", timeout: 120) {
            self.text(app, containing: "PEARTREE").exists && !self.el(app, "session.pending").exists
        }
        // let the turn settle so the next send is not queued behind it
        RunLoop.current.run(until: Date().addingTimeInterval(8))
        shot(app, "05-reply-answered")

        // the force-push guardrail holds a command; allowed once from the phone
        send(app, "Run exactly this shell command with the Bash tool and nothing else: echo git push --force origin demo")
        let review = el(app, "attention.review")
        XCTAssertTrue(review.waitForExistence(timeout: 120), "the guardrail's approval, verified against Workbox's key")
        review.tap()
        XCTAssertTrue(el(app, "approval.command").waitForExistence(timeout: 10), "the Mac-signed command")
        shot(app, "06-approval")
        el(app, "approval.allow").tap()
        waitFor("approved and cleared", timeout: 60) { !review.exists }
        waitFor("the held command ran and the turn ended", timeout: 120) {
            self.text(app, containing: "git push --force origin demo").exists && !self.el(app, "session.pending").exists
        }
        RunLoop.current.run(until: Date().addingTimeInterval(5))
        shot(app, "07-approved-and-ran")
        back(app)

        // Hold all, then Release (Release asks the owner; -fabricOwnerCheck stands in)
        el(app, "agents.hold").tap()
        let hold = app.buttons.matching(NSPredicate(format: "label == %@", "Hold all")).lastMatch
        XCTAssertTrue(hold.waitForExistence(timeout: 5))
        hold.tap()
        XCTAssertTrue(el(app, "agents.held").waitForExistence(timeout: 30), "Workbox reported itself held")
        shot(app, "08-held")
        el(app, "agents.hold").tap()
        waitFor("released", timeout: 30) { !self.el(app, "agents.held").exists }
        shot(app, "09-released")

        // Talk: the two real agents' ask and reply from the earlier run
        el(app, "agents.talk").tap()
        let conversation = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'talk.row.'")).firstMatch
        XCTAssertTrue(conversation.waitForExistence(timeout: 20), "a conversation between the real agents")
        shot(app, "10-talk")
        conversation.tap()
        RunLoop.current.run(until: Date().addingTimeInterval(2))
        shot(app, "11-conversation")
    }
}
