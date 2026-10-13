import XCTest

/// Move a doc in the Library tree against a LOCAL scratch daemon
/// (`TEST_RUNNER_TAISCE_UI_URL`, default 127.0.0.1:7518): drag one row onto
/// another (it goes inside), and "Move to…" from the context menu; the
/// daemon must then have the new parent. The sidebar on iPad and the Mac,
/// the Library tab on iPhone (a long-press drag).
final class MoveDocUITests: XCTestCase {
    var base: URL { URL(string: ProcessInfo.processInfo.environment["TAISCE_UI_URL"] ?? "http://127.0.0.1:7518")! }

    override func setUp() {
        continueAfterFailure = false
    }

    /// A screenshot in the result bundle (what the screen showed when a check failed).
    func shot(_ name: String) {
        let a = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        a.name = name
        a.lifetime = .keepAlways
        add(a)
    }

    func call(_ path: String, _ body: [String: Any]? = nil) throws -> Any {
        var r = URLRequest(url: base.appending(path: path))
        r.timeoutInterval = 10
        if let body {
            r.httpMethod = "POST"
            r.setValue("application/json", forHTTPHeaderField: "Content-Type")
            r.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let done = expectation(description: path)
        var out: Data?
        URLSession.shared.dataTask(with: r) { d, _, _ in out = d; done.fulfill() }.resume()
        wait(for: [done], timeout: 15)
        return try JSONSerialization.jsonObject(with: try XCTUnwrap(out, "no answer from \(path)"))
    }

    /// A counter for order keys made in the same millisecond.
    private var nextKey = 0

    /// A top-level doc, placed at the top of the tree (other suites leave
    /// docs behind: appended, these would fall below the first screenful).
    func makeDoc(_ title: String) throws -> String {
        let doc = try XCTUnwrap(try call("/api/docs", ["title": title]) as? [String: Any])
        let id = try XCTUnwrap(doc["id"] as? String)
        // unique across tests and in creation order: "1" + base-36 time
        // (milliseconds) + a counter, ending in a non-zero digit
        let ms = Int(Date().timeIntervalSince1970 * 1000)
        let key = "1" + String(ms, radix: 36) + String(nextKey, radix: 36) + "i"
        nextKey += 1
        let moved = try call("/api/doc/\(id)/move", ["parent_id": NSNull(), "sort_key": key])
        XCTAssertEqual((moved as? [String: Any])?["ok"] as? Bool, true, "placed at the top: \(moved)")
        return id
    }

    /// The doc's parent on the daemon (nil at the top level).
    func parent(of id: String) throws -> String? {
        let tree = try XCTUnwrap(try call("/api/doc/\(id)") as? [String: Any])
        let doc = (tree["doc"] as? [String: Any]) ?? tree
        return doc["parent_id"] as? String
    }

    /// Waits for the daemon to report `id` under `parent` (the move goes
    /// through the outbox).
    func waitForParent(of id: String, _ parent: String, timeout: TimeInterval = 20) throws {
        let end = Date().addingTimeInterval(timeout)
        var last: String?
        while Date() < end {
            last = try self.parent(of: id)
            if last == parent { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.5))
        }
        XCTFail("the daemon has \(id) under \(last ?? "the top level"), not \(parent)")
    }

    func launch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString, "-library.expanded", ""]
        app.launch()
        // iPhone: the Library is a tab; iPad and the Mac show it in the sidebar
        let tab = app.tabBars.buttons["Library"]
        if tab.waitForExistence(timeout: 3) { tab.tap() }
        return app
    }

    /// A click on the Mac, a tap elsewhere (a touch-style tap doesn't reach Catalyst controls).
    func hit(_ e: XCUIElement) {
        #if targetEnvironment(macCatalyst)
        e.click()
        #else
        e.tap()
        #endif
    }

    /// A tree row (LibraryTree: one accessibility element per row).
    func row(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        app.descendants(matching: .any)["tree.\(title)"].firstMatch
    }

    /// Drag `from` onto `to` at a fraction of its height (0.5 = the middle:
    /// inside; 0.1 = its top edge: before it; 0.9 = after it), at a hand's
    /// pace, resting before letting go.
    func drag(_ from: XCUIElement, onto to: XCUIElement, at fraction: CGFloat) {
        let start = from.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let end = to.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: fraction))
        #if targetEnvironment(macCatalyst)
        // a mouse drag (a touch-style press doesn't start one on the Mac)
        start.click(forDuration: 0.3, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 1)
        #else
        // lift and move straight away, as a finger does (held still, the
        // long-press shows the row's menu instead)
        start.press(forDuration: 0.6, thenDragTo: end, withVelocity: .default, thenHoldForDuration: 1)
        #endif
    }

    /// The sort keys of `ids` under one parent on the daemon, in their order.
    func order(_ ids: [String]) throws -> [String] {
        let keys = try ids.map { id -> (String, String) in
            let tree = try XCTUnwrap(try call("/api/doc/\(id)") as? [String: Any])
            let doc = (tree["doc"] as? [String: Any]) ?? tree
            return (id, (doc["sort_key"] as? String) ?? "")
        }
        return keys.sorted { $0.1 < $1.1 }.map(\.0)
    }

    func testDragARowOntoAnotherNestsIt() throws {
        let n = Int.random(in: 1000...9999)
        let a = try makeDoc("Drag A \(n)"), b = try makeDoc("Drop B \(n)")

        let app = launch()
        let rowA = row(app, "Drag A \(n)"), rowB = row(app, "Drop B \(n)")
        XCTAssertTrue(rowA.waitForExistence(timeout: 20), "the Library lists A")
        XCTAssertTrue(rowB.waitForExistence(timeout: 5), "the Library lists B")

        drag(rowA, onto: rowB, at: 0.5)

        try waitForParent(of: a, b)
        // B opened to show its new child, listed under it
        XCTAssertTrue(rowA.waitForExistence(timeout: 10), "A is still in the tree, inside B")
        XCTAssertGreaterThan(rowA.frame.minY, rowB.frame.minY, "A shows under B")
        XCTAssertEqual(rowB.value as? String, "folder, open", "B is an open folder")
        XCTAssertFalse(app.alerts["Couldn't move"].exists, "no refusal")
    }

    func testDragBetweenRowsReorders() throws {
        let n = Int.random(in: 1000...9999)
        let x = try makeDoc("Order X \(n)"), y = try makeDoc("Order Y \(n)"), z = try makeDoc("Order Z \(n)")
        let app = launch()
        let rowX = row(app, "Order X \(n)"), rowZ = row(app, "Order Z \(n)")
        XCTAssertTrue(rowX.waitForExistence(timeout: 20), "the Library lists X")
        XCTAssertTrue(rowZ.waitForExistence(timeout: 5), "the Library lists Z")
        let before = try order([x, y, z])
        // Z dropped on X's top edge: before X
        drag(rowZ, onto: rowX, at: 0.1)
        let end = Date().addingTimeInterval(20)
        var now = before
        while Date() < end {
            now = try order([x, y, z])
            if now.first == z { break }
            RunLoop.current.run(until: Date().addingTimeInterval(0.5))
        }
        XCTAssertEqual(now.first, z, "Z went before X (was \(before), now \(now))")
        XCTAssertNil(try parent(of: z), "Z stays at the top level")
        XCTAssertLessThan(rowZ.frame.minY, rowX.frame.minY, "Z shows above X")
        XCTAssertFalse(app.alerts["Couldn't move"].exists, "no refusal")
    }

    func testDragOutOfAFolderToTheTopLevel() throws {
        let n = Int.random(in: 1000...9999)
        let f = try makeDoc("Box F \(n)")
        let k = try XCTUnwrap(try call("/api/docs", ["title": "Kid K \(n)", "parent_doc_id": f]) as? [String: Any])["id"] as? String
        let kid = try XCTUnwrap(k)
        let app = launch()
        let rowF = row(app, "Box F \(n)")
        XCTAssertTrue(rowF.waitForExistence(timeout: 20), "the Library lists F")
        // open F with its chevron (12 pt in from the row's leading edge)
        let chevron = rowF.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0.5)).withOffset(CGVector(dx: 10, dy: 0))
        #if targetEnvironment(macCatalyst)
        chevron.click()
        #else
        chevron.tap()
        #endif
        let rowK = row(app, "Kid K \(n)")
        if !rowK.waitForExistence(timeout: 5) { shot("F not open") }
        XCTAssertTrue(rowK.exists, "F opened to show K")
        // K dropped on F's top edge: just before F, F's sibling at the top
        // level (F's bottom edge, F being open, would mean first inside F)
        drag(rowK, onto: rowF, at: 0.08)
        let end = Date().addingTimeInterval(20)
        var p: String? = f
        while Date() < end {
            p = try parent(of: kid)
            if p == nil { break }
            RunLoop.current.run(until: Date().addingTimeInterval(0.5))
        }
        XCTAssertNil(p, "K is at the top level now")
        XCTAssertFalse(app.alerts["Couldn't move"].exists, "no refusal")
    }

    func testMoveToFromTheContextMenu() throws {
        let n = Int.random(in: 1000...9999)
        let c = try makeDoc("Moved C \(n)"), d = try makeDoc("Folder D \(n)")

        let app = launch()
        let rowC = row(app, "Moved C \(n)")
        XCTAssertTrue(rowC.waitForExistence(timeout: 20), "the Library lists C")
        XCTAssertTrue(row(app, "Folder D \(n)").waitForExistence(timeout: 5), "the Library lists D")
        #if targetEnvironment(macCatalyst)
        app.activate()
        rowC.rightClick()
        let menuItem = app.menus.menuItems["Move to\u{2026}"].firstMatch
        #else
        rowC.press(forDuration: 1.2)
        let menuItem = app.buttons["Move to\u{2026}"].firstMatch
        #endif
        XCTAssertTrue(menuItem.waitForExistence(timeout: 5), "the context menu offers Move to…")
        hit(menuItem)

        // narrow the picker to D (a long Library pushes it out of view)
        let find = app.searchFields["Find a doc"].firstMatch
        if find.waitForExistence(timeout: 5) {
            hit(find)
            find.typeText("Folder D \(n)")
        }
        // the picker's row, not the tree's row behind the sheet
        let option = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@ AND NOT (identifier BEGINSWITH 'tree.')", "Folder D \(n)")).firstMatch
        if !option.waitForExistence(timeout: 5) { shot("picker") }
        XCTAssertTrue(option.exists, "the picker offers D")
        hit(option)

        try waitForParent(of: c, d)
        XCTAssertFalse(app.alerts["Couldn't move"].exists, "no refusal")
    }

    /// A long Library: a drag held at the bottom edge scrolls it, so a doc
    /// off screen can be reached.
    func testDragNearTheBottomEdgeScrolls() throws {
        let n = Int.random(in: 1000...9999)
        var made: [String] = []
        for i in 0..<45 { made.append(try makeDoc(String(format: "Fill %02d %d", i, n))) }
        let lastTitle = "Zed last \(n)"
        made.append(try makeDoc(lastTitle))
        // the next tests' docs must not land below a screenful of these
        defer { for id in made { _ = try? call("/api/doc/\(id)/delete", [:]) } }
        let app = launch()
        let top = row(app, String(format: "Fill %02d %d", 0, n))
        XCTAssertTrue(top.waitForExistence(timeout: 20), "the Library lists the docs")
        let last = row(app, lastTitle)
        XCTAssertTrue(last.waitForExistence(timeout: 10), "the last doc is in the tree")
        let window = app.windows.firstMatch.frame
        XCTAssertGreaterThan(last.frame.minY, window.maxY, "the last doc starts off screen")
        // hold a drag just above the window's bottom edge for a few seconds
        let start = top.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        // just inside the visible bottom: above the tab bar on iPhone
        let tabs = app.tabBars.firstMatch
        // (on the Mac, above the sidebar's sync bar)
        let visibleBottom = tabs.exists ? tabs.frame.minY : window.maxY - 60
        let bottom = app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
            .withOffset(CGVector(dx: top.frame.midX - window.minX, dy: visibleBottom - window.minY - 30))
        #if targetEnvironment(macCatalyst)
        start.click(forDuration: 0.3, thenDragTo: bottom, withVelocity: .slow, thenHoldForDuration: 6)
        #else
        start.press(forDuration: 0.6, thenDragTo: bottom, withVelocity: .default, thenHoldForDuration: 6)
        #endif
        XCTAssertLessThan(last.frame.maxY, visibleBottom + 1, "the tree scrolled down to the last doc")
    }

    /// The bottom edge of an OPEN folder: the line draws where its first
    /// child is, and the doc goes there (first inside), not after the folder.
    func testDropOnAnOpenFoldersBottomEdgeGoesFirstInside() throws {
        let n = Int.random(in: 1000...9999)
        let f = try makeDoc("Open F \(n)")
        let kidID = try XCTUnwrap(try XCTUnwrap(try call("/api/docs", ["title": "First kid \(n)", "parent_doc_id": f]) as? [String: Any])["id"] as? String)
        let x = try makeDoc("Mover X \(n)")
        let app = launch()
        let rowF = row(app, "Open F \(n)"), rowX = row(app, "Mover X \(n)")
        XCTAssertTrue(rowF.waitForExistence(timeout: 20) && rowX.waitForExistence(timeout: 5))
        let chevron = rowF.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0.5)).withOffset(CGVector(dx: 10, dy: 0))
        #if targetEnvironment(macCatalyst)
        chevron.click()
        #else
        chevron.tap()
        #endif
        if !row(app, "First kid \(n)").waitForExistence(timeout: 5) { shot("F not open (first inside)") }
        XCTAssertTrue(row(app, "First kid \(n)").exists, "F is open")
        drag(rowX, onto: rowF, at: 0.92)
        try waitForParent(of: x, f)
        XCTAssertEqual(try order([x, kidID]), [x, kidID], "X went in first, before F's first child")
    }

    /// The Mac sidebar: after a click on a row, the arrow keys move the selection.
    func testArrowKeysMoveTheSidebarSelection() throws {
        #if !targetEnvironment(macCatalyst)
        throw XCTSkip("hardware-keyboard sidebar: the Mac")
        #else
        let n = Int.random(in: 1000...9999)
        _ = try makeDoc("Key A \(n)"); _ = try makeDoc("Key B \(n)")
        let app = launch()
        let rowA = row(app, "Key A \(n)"), rowB = row(app, "Key B \(n)")
        XCTAssertTrue(rowA.waitForExistence(timeout: 20) && rowB.waitForExistence(timeout: 5))
        rowA.click()
        XCTAssertTrue(rowA.isSelected, "a click selects A")
        app.typeKey(.downArrow, modifierFlags: [])
        let end = Date().addingTimeInterval(5)
        while !rowB.isSelected, Date() < end { RunLoop.current.run(until: Date().addingTimeInterval(0.2)) }
        XCTAssertTrue(rowB.isSelected, "B is selected")
        XCTAssertFalse(rowA.isSelected, "the down arrow moved the selection to B")
        #endif
    }

    /// The sidebar's workspace switcher opens its sheets (they must have the
    /// app model: Move to… crashed without it).
    func testWorkspaceSheetsOpenFromTheSidebar() throws {
        #if !targetEnvironment(macCatalyst)
        throw XCTSkip("the sidebar switcher: the Mac and iPad")
        #else
        _ = try call("/api/workspaces", ["name": "Ws \(Int.random(in: 1000...9999))"])
        let app = launch()
        let switcher = app.buttons.matching(NSPredicate(format: "label CONTAINS 'Unsorted' OR label BEGINSWITH 'Ws '")).firstMatch
        XCTAssertTrue(switcher.waitForExistence(timeout: 20), "the switcher shows")
        for item in ["New workspace\u{2026}", "Manage workspaces"] {
            switcher.click()
            let menuItem = app.menuItems[item].firstMatch
            XCTAssertTrue(menuItem.waitForExistence(timeout: 5), "the switcher offers \(item)")
            menuItem.click()
            sleep(2)
            XCTAssertEqual(app.state, .runningForeground, "\(item) opened without a crash")
            app.typeKey(.escape, modifierFlags: [])
            sleep(1)
        }
        #endif
    }
}
