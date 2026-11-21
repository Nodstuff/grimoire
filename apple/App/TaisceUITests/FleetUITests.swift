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

    func launch(_ fleet: [String: Any]?, tab: String = "agents") throws -> XCUIApplication {
        let server = try FleetStubServer(fleet: fleet)
        self.server = server
        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", server.url, "-tab", tab]
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
        let resting = app.descendants(matching: .any).matching(identifier: "agents.resting").firstMatch
        // below the fold on a phone
        if !(resting.exists && resting.isHittable) { row(app, order[2]).swipeUp() }
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
        // "Until tomorrow", or "Until this morning" in the small hours
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Until t'")).firstMatch.exists)
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

    /// A session's notification while the app is open: not presented with
    /// Agents in front, presented once Agents has been left (the screen
    /// must stop counting itself when it goes), and a tap lands on that
    /// session's row, marked. Needs notifications pushed from the Mac while
    /// it runs (`apple/scripts/push-fabric-session.sh <udid>`, and
    /// `TEST_RUNNER_TAISCE_UI_PUSHER=1` to say so); skipped otherwise. iPhone.
    func testASessionNotificationShowsOnceAgentsIsLeftAndATapLandsOnItsRow() throws {
        try XCTSkipIf(ProcessInfo.processInfo.environment["TAISCE_UI_PUSHER"] != "1", "nothing is pushing notifications to this simulator")
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom != .phone, "written for the phone's Today card")
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        // the last active row is the one the notification names, so it starts below the others
        var sessions = StubFleet.sessions
        sessions.swapAt(0, 3)
        let app = try launch(StubFleet.fleet(sessions: sessions), tab: "todos")
        // allow notifications (the To-dos card asks)
        let turnOn = app.buttons["Turn on"].firstMatch
        if turnOn.waitForExistence(timeout: 8) {
            turnOn.tap()
            let allow = springboard.buttons["Allow"].firstMatch
            if allow.waitForExistence(timeout: 8) { allow.tap() }
        }
        let banner = springboard.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS 'taisce/main'")).firstMatch
        func bannerShows(within seconds: TimeInterval) -> Bool {
            let end = Date().addingTimeInterval(seconds)
            while Date() < end {
                if banner.exists && banner.isHittable { return true }
                RunLoop.current.run(until: Date().addingTimeInterval(0.25))
            }
            return false
        }
        // on another screen a notification shows
        XCTAssertTrue(bannerShows(within: 20), "notifications are arriving and allowed")
        // Agents, from Today's card
        app.tabBars.buttons["Today"].tap()
        let card = app.descendants(matching: .any).matching(identifier: "today.agents").firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 15))
        card.tap()
        XCTAssertTrue(row(app, "taisce/main").waitForExistence(timeout: 15))
        // one pushed as the screen arrived may still come up, and a banner already
        // up goes by itself; after that, none comes while Agents is in front
        RunLoop.current.run(until: Date().addingTimeInterval(4))
        waitFor("the earlier banner has gone", timeout: 20) { !(banner.exists && banner.isHittable) }
        // (one is pushed every 6 seconds)
        XCTAssertFalse(bannerShows(within: 14), "what is on screen is not announced")
        // leave Agents: the next one shows again
        app.navigationBars.buttons.firstMatch.tap()
        XCTAssertTrue(card.waitForExistence(timeout: 10), "back on Today")
        XCTAssertTrue(bannerShows(within: 20), "a session notification shows once Agents has been left")
        // a tap opens that session, with Agents beneath it on that session's row, marked
        banner.tap()
        XCTAssertTrue(app.descendants(matching: .any).matching(identifier: "session.header").firstMatch.waitForExistence(timeout: 15), "the session opened")
        app.navigationBars.buttons.firstMatch.tap()
        let named = row(app, "taisce/main")
        XCTAssertTrue(named.waitForExistence(timeout: 15), "Agents beneath")
        waitFor("the row is marked", timeout: 6) { named.isSelected }
        XCTAssertFalse(row(app, "workbox/main").isSelected)
        // (seen at all, it lasted: it used to be set and cleared in the same instant.) Then it goes
        waitFor("the mark goes", timeout: 8) { !named.isSelected }
    }

    /// Where Agents sits. iPhone: no tab; a card on Today, after the pins
    /// while nothing needs you and first when something does, with Today's
    /// tab badge counting it; a snoozed session stops counting. iPad: the
    /// sidebar, no card; and no sidebar entry on a server with no fleet.
    func testAgentsOnTodayAndInTheSidebar() throws {
        let quiet = [
            StubFleet.session(4, "taisce/fabric", "working", tool: "Bash", note: "Building the app"),
            StubFleet.session(5, "notes/main", "idle"),
        ]
        let app = try launch(StubFleet.fleet(sessions: quiet), tab: "today")
        let card = app.descendants(matching: .any).matching(identifier: "today.agents").firstMatch
        let sidebar = app.descendants(matching: .any).matching(identifier: "sidebar.agents").firstMatch
        if UIDevice.current.userInterfaceIdiom == .pad {
            XCTAssertTrue(sidebar.waitForExistence(timeout: 20), "Agents is in the sidebar")
            server?.update(session: StubFleet.id(4)) { $0["state"] = "needs_you" }
            waitFor("the sidebar counts it") { sidebar.label.contains("1 need you") }
            XCTAssertFalse(card.exists, "the card is the phone's way in")
            shot(app, "today-sidebar-needs-you")
            app.terminate()
            // a server with no fleet (a LOCAL daemon): no dead entry
            let local = try launch(nil, tab: "today")
            XCTAssertTrue(local.descendants(matching: .any).matching(identifier: "sidebar.books").firstMatch.waitForExistence(timeout: 20))
            let entry = local.descendants(matching: .any).matching(identifier: "sidebar.agents").firstMatch
            waitFor("Agents is not offered") { !entry.exists }
            shot(local, "sidebar-no-fleet")
            return
        }
        XCTAssertTrue(card.waitForExistence(timeout: 20), "the card shows once the fleet has")
        XCTAssertFalse(app.tabBars.buttons["Agents"].exists, "no sixth tab")
        XCTAssertTrue(app.tabBars.buttons["Search"].exists, "Search keeps its place")
        let due = app.staticTexts["Due"].firstMatch
        let pinned = app.staticTexts["Pinned"].firstMatch
        let today = app.tabBars.buttons["Today"]
        func badge() -> String { today.value as? String ?? "" }
        // nothing needs you: after the pins, and no badge
        XCTAssertGreaterThan(card.frame.minY, pinned.frame.minY, "what's due keeps its place")
        XCTAssertTrue(card.label.contains("1 working"), card.label)
        XCTAssertFalse(badge().contains("1"), badge())
        shot(app, "today-agents-quiet")
        // a session comes to need you: first, and counted on the tab
        server?.update(session: StubFleet.id(4)) { $0["state"] = "needs_you"; $0["last_line"] = "Which branch?" }
        waitFor("the card moved to the top") { card.exists && card.frame.minY < due.frame.minY }
        XCTAssertTrue(card.label.contains("1 needs you"), card.label)
        waitFor("the tab counts it") { badge().contains("1") }
        shot(app, "today-agents-needs-you")
        // from another tab the badge still says so
        app.tabBars.buttons["Library"].tap()
        XCTAssertTrue(badge().contains("1"), badge())
        today.tap()
        // snoozed: it has stopped asking
        server?.update(session: StubFleet.id(4)) { $0["quiet_until"] = Int(Date().timeIntervalSince1970) + 3600 }
        waitFor("the card is back after the pins") { card.exists && card.frame.minY > pinned.frame.minY }
        waitFor("and the badge has gone") { !badge().contains("1") }
        // the card opens Agents, where it is still listed
        card.tap()
        XCTAssertTrue(row(app, "taisce/fabric").waitForExistence(timeout: 10))
        XCTAssertTrue(row(app, "taisce/fabric").label.contains("Snoozed until"))
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
