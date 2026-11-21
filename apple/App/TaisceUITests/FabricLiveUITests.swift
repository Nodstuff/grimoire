import XCTest

/// Agent Fabric against a real scratch SERVER daemon and the server's
/// `fake_runtime` playing `demo-work-script.json`: sign in through the
/// relay, pair with the code the fake runtime printed, then answer its
/// question, approve its release, reply to a session and open Talk, each
/// order signed here and checked by the hub and the runtime. Skipped unless
/// `TEST_RUNNER_TAISCE_UI_URL` names the daemon, `TEST_RUNNER_TAISCE_UI_SIGNIN_RELAY`
/// its relay and `TEST_RUNNER_TAISCE_FABRIC_PAIR_CODE` the code. Never a real server.
final class FabricLiveUITests: XCTestCase {
    func env(_ k: String) -> String? { ProcessInfo.processInfo.environment[k].flatMap { $0.isEmpty ? nil : $0 } }

    func el(_ app: XCUIApplication, _ id: String) -> XCUIElement { app.descendants(matching: .any).matching(identifier: id).firstMatch }

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
            let device = UIDevice.current.userInterfaceIdiom == .pad ? "ipad" : "iphone"
            try? app.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(device)-live-\(name).png"))
        }
    }

    func testTheRealHubAndRuntime() throws {
        guard let url = env("TAISCE_UI_URL"), let relay = env("TAISCE_UI_SIGNIN_RELAY"), let code = env("TAISCE_FABRIC_PAIR_CODE") else {
            throw XCTSkip("needs a scratch daemon, its sign-in relay and a fake runtime's pairing code")
        }
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchEnvironment = ["TAISCE_UI_TEST": "1", "TAISCE_UI_SIGNIN_RELAY": relay]
        app.launchArguments = ["-serverURL", url, "-tab", "agents", "-fabricOwnerCheck", "pass"]
        app.launch()
        let taisce = el(app, "agents.row.taisce/main")
        XCTAssertTrue(taisce.waitForExistence(timeout: 60), "signed in; the fleet shows")
        shot(app, "agents")

        // pair
        el(app, "agents.settings").tap()
        el(app, "settings.fabric.pair").tap()
        let field = el(app, "pairing.code")
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.tap()
        field.typeText(code)
        el(app, "pairing.submit").tap()
        XCTAssertTrue(el(app, "pairing.paired").waitForExistence(timeout: 30), "the runtime opened the seal and confirmed")
        shot(app, "paired")
        app.navigationBars.buttons["Done"].firstMatch.tap()
        waitFor("Settings › Agents says paired") { self.el(app, "settings.fabric.mac.mbp").label.contains("Paired") }
        app.navigationBars.buttons.element(boundBy: 0).tap()

        // the question, answered by its choice
        el(app, "agents.row.qompass/mcp-dx").tap()
        let fix = el(app, "question.choice.fix")
        XCTAssertTrue(fix.waitForExistence(timeout: 20), "the question verified against the code's key")
        shot(app, "question")
        fix.tap()
        waitFor("the runtime answered it", timeout: 30) { !fix.exists }
        app.navigationBars.buttons.element(boundBy: 0).tap()

        // the approval, allowed once
        taisce.tap()
        let review = el(app, "attention.review")
        XCTAssertTrue(review.waitForExistence(timeout: 20))
        XCTAssertTrue(el(app, "turn.0196f3a0-0000-7000-8000-0000000000f2").waitForExistence(timeout: 20), "the turn the runtime reported")
        shot(app, "session")
        review.tap()
        XCTAssertTrue(el(app, "approval.command").waitForExistence(timeout: 10))
        shot(app, "approval")
        el(app, "approval.allow").tap()
        waitFor("approved and cleared", timeout: 30) { !review.exists }

        // a reply: held five seconds, then signed, and played back as a turn
        let composer = el(app, "session.composer")
        composer.tap()
        composer.typeText("Yes, open the pull request")
        el(app, "session.send").tap()
        XCTAssertTrue(el(app, "session.pending").waitForExistence(timeout: 3))
        waitFor("the runtime played the reply back as a turn", timeout: 60) {
            app.staticTexts.containing(NSPredicate(format: "label CONTAINS 'Yes, open the pull request'")).count > 0 && !self.el(app, "session.pending").exists
        }
        shot(app, "replied")
        app.navigationBars.buttons.element(boundBy: 0).tap()

        // Talk
        el(app, "agents.talk").tap()
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS 'workbox/main'")).firstMatch.waitForExistence(timeout: 20))
        shot(app, "talk")
    }
}
