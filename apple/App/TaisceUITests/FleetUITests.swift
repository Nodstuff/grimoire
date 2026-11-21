import XCTest

/// Agents (Agent Fabric, Slice 1: Watch) against a stand-in server in the
/// runner (`FleetStubServer`): no daemon to start. Screenshots are
/// attachments, and files when `TEST_RUNNER_TAISCE_UI_SHOTS` names a folder.
final class FleetUITests: XCTestCase {
    var server: FleetStubServer?

    override func setUpWithError() throws {
        #if targetEnvironment(macCatalyst)
        // the Mac's runner is sandboxed: it cannot listen
        throw XCTSkip("the stand-in server needs a runner that may listen (a simulator)")
        #else
        continueAfterFailure = false
        if UIDevice.current.userInterfaceIdiom == .pad { XCUIDevice.shared.orientation = .landscapeLeft }
        #endif
    }

    func launch(_ fleet: [String: Any]?) throws -> XCUIApplication {
        let server = try FleetStubServer(fleet: fleet)
        self.server = server
        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", server.url, "-tab", "agents"]
        app.launch()
        return app
    }

    func row(_ app: XCUIApplication, _ address: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: "agents.row.\(address)").firstMatch
    }

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
            let device = UIDevice.current.userInterfaceIdiom == .pad ? "ipad" : "iphone"
            try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
            try? app.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(device)-\(name).png"))
        }
    }

    func testTheFleetRendersInTheServersAttentionOrderAndRowsHoldStill() throws {
        let app = try launch(StubFleet.fleet())
        let order = ["taisce/main", "qompass/mcp-dx", "workbox/main", "taisce/fabric"]
        XCTAssertTrue(row(app, order[0]).waitForExistence(timeout: 20), "the fleet shows")
        let tops = order.map { row(app, $0).frame.minY }
        XCTAssertEqual(tops, tops.sorted(), "needs you, stalled, finished, working: as the server sent them")
        XCTAssertTrue(row(app, "taisce/main").label.contains("Needs you"))
        XCTAssertTrue(row(app, "taisce/main").label.contains("Shall I open the pull request?"), "the question is on the row")
        XCTAssertTrue(row(app, "qompass/mcp-dx").label.contains("Stalled"))
        XCTAssertTrue(row(app, "workbox/main").label.contains("Finished"))
        XCTAssertTrue(row(app, "taisce/fabric").label.contains("Working \u{00B7} Bash \u{00B7} 2 h"), row(app, "taisce/fabric").label)
        XCTAssertTrue(row(app, "taisce/fabric").label.contains("personal \u{00B7} main \u{00B7} context 61%"))
        // the Mac and each account's headroom
        let mac = app.descendants(matching: .any).matching(identifier: "agents.runtime.mbp").firstMatch
        XCTAssertTrue(mac.label.contains("Mac awake"), mac.label)
        let work = app.descendants(matching: .any).matching(identifier: "agents.account.work").firstMatch
        XCTAssertTrue(work.label.contains("session 8% left") && work.label.contains("nearly spent"), work.label)
        // what is doing nothing is folded under a count
        XCTAssertFalse(row(app, "notes/main").exists)
        shot(app, "agents-populated")
        // a change redraws the row where it stands: the last one comes to need you and stays last
        server?.update(session: StubFleet.id(4)) { $0["state"] = "needs_you"; $0["tool"] = ""; $0["last_line"] = "Which branch?" }
        waitFor("the row changed") { self.row(app, "taisce/fabric").label.contains("Needs you") }
        let after = order.map { row(app, $0).frame.minY }
        XCTAssertEqual(after, after.sorted(), "no row moved")
        // the fold opens
        app.swipeUp()
        let resting = app.descendants(matching: .any).matching(identifier: "agents.resting").firstMatch
        XCTAssertTrue(resting.waitForExistence(timeout: 5))
        resting.tap()
        XCTAssertTrue(row(app, "notes/main").waitForExistence(timeout: 5))
        XCTAssertTrue(row(app, "notes/main").label.contains("Idle"))
    }

    func testAnOfflineRuntimeSaysMacAsleepAndItsSessionsAreLastKnown() throws {
        let app = try launch(StubFleet.fleet(online: false, seenAgo: 1740))
        let mac = app.descendants(matching: .any).matching(identifier: "agents.runtime.mbp").firstMatch
        XCTAssertTrue(mac.waitForExistence(timeout: 20))
        XCTAssertTrue(mac.label.contains("Mac asleep since"), mac.label)
        XCTAssertTrue(mac.label.contains("sessions shown as last known"), mac.label)
        // still listed, as last known: no clock runs on a Mac that is away
        XCTAssertTrue(row(app, "taisce/fabric").label.contains("Working \u{00B7} Bash \u{00B7} last known"), row(app, "taisce/fabric").label)
        XCTAssertTrue(row(app, "taisce/main").exists)
        shot(app, "agents-mac-asleep")
        // the Mac wakes: said at once
        server?.updateRuntime { $0["online"] = true; $0["last_seen_at"] = Int(Date().timeIntervalSince1970) }
        waitFor("awake again") { mac.label.contains("Mac awake") }
        XCTAssertTrue(row(app, "taisce/fabric").label.contains("2 h"))
    }

    func testSnoozeMarksTheRowAndUnsnoozeClearsIt() throws {
        let app = try launch(StubFleet.fleet())
        let asks = row(app, "taisce/main")
        XCTAssertTrue(asks.waitForExistence(timeout: 20))
        XCTAssertFalse(asks.label.contains("Snoozed"))
        asks.swipeLeft()
        app.buttons["Snooze"].firstMatch.tap()
        let hour = app.buttons["For an hour"].firstMatch
        XCTAssertTrue(hour.waitForExistence(timeout: 5), "the two durations are offered")
        XCTAssertTrue(app.buttons["Until tomorrow"].firstMatch.exists)
        hour.tap()
        waitFor("the row says it is snoozed") { self.row(app, "taisce/main").label.contains("Snoozed until") }
        // still listed, still needing you
        XCTAssertTrue(row(app, "taisce/main").label.contains("Needs you"))
        let posts = server?.fabricRequests.filter { $0.method == "POST" } ?? []
        XCTAssertEqual(posts.map(\.path), ["/api/fabric/sessions/\(StubFleet.runtime)/\(StubFleet.id(1))/snooze"])
        let until = try XCTUnwrap(posts.first?.body["until"] as? Int)
        XCTAssertEqual(Double(until), Date().timeIntervalSince1970 + 3600, accuracy: 120, "an hour from now")
        shot(app, "agents-snoozed")
        row(app, "taisce/main").swipeLeft()
        app.buttons["Unsnooze"].firstMatch.tap()
        waitFor("the mark is gone") { !self.row(app, "taisce/main").label.contains("Snoozed") }
        XCTAssertEqual(server?.fabricRequests.filter { $0.method == "DELETE" }.count, 1)
    }

    func testEmptyStatesSayWhatToDo() throws {
        // a fleet with no runtime: how one is enrolled
        var app = try launch(["rev": 0, "runtimes": [], "sessions": []])
        let none = app.descendants(matching: .any).matching(identifier: "agents.empty.noRuntime").firstMatch
        XCTAssertTrue(none.waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS 'taisce fabric runtime add'")).firstMatch.exists)
        shot(app, "agents-no-runtime")
        app.terminate()
        // a Mac enrolled, nothing running
        app = try launch(["rev": 1, "runtimes": [StubFleet.mac(online: true)], "sessions": []])
        XCTAssertTrue(app.descendants(matching: .any).matching(identifier: "agents.empty.noSessions").firstMatch.waitForExistence(timeout: 20))
        XCTAssertTrue(app.descendants(matching: .any).matching(identifier: "agents.runtime.mbp").firstMatch.exists)
        shot(app, "agents-no-sessions")
        app.terminate()
        // a server with no fleet (a LOCAL daemon answers 404): said, not an error
        app = try launch(nil)
        XCTAssertTrue(app.descendants(matching: .any).matching(identifier: "agents.unavailable").firstMatch.waitForExistence(timeout: 20))
        XCTAssertTrue(app.staticTexts["Agents live on your Taisce server"].exists)
        shot(app, "agents-not-here")
    }
}
