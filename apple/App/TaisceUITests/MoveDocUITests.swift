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
        #if !targetEnvironment(macCatalyst)
        // iPad: the tree is the sidebar's, which portrait hides; landscape
        // keeps it beside the detail (iPhone: portrait, the Library tab)
        XCUIDevice.shared.orientation = UIDevice.current.userInterfaceIdiom == .pad ? .landscapeLeft : .portrait
        #endif
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

    /// This test's start (ms) and a counter, for order keys.
    private lazy var testStamp = Int(Date().timeIntervalSince1970 * 1000)
    private var nextKey = 0

    /// A top-level doc, placed at the top of the tree (other suites leave
    /// docs behind: appended, these would fall below the first screenful).
    func makeDoc(_ title: String) throws -> String {
        let doc = try XCTUnwrap(try call("/api/docs", ["title": title]) as? [String: Any])
        let id = try XCTUnwrap(doc["id"] as? String)
        // a later test's docs sort before an earlier one's (its leftovers
        // stay put below), and within a test in creation order: "1" + the
        // time to a far-off moment (shrinks as tests go on, fixed width) +
        // a counter, ending in a non-zero digit
        let key = nextTopKey()
        let moved = try call("/api/doc/\(id)/move", ["parent_id": NSNull(), "sort_key": key])
        XCTAssertEqual((moved as? [String: Any])?["ok"] as? Bool, true, "placed at the top: \(moved)")
        return id
    }

    /// The next of this test's top-of-the-tree order keys (see makeDoc).
    func nextTopKey() -> String {
        let left = 99_999_999_999_999 - testStamp
        let pad = { (v: Int, w: Int) -> String in let t = String(v, radix: 36); return String(repeating: "0", count: max(0, w - t.count)) + t }
        defer { nextKey += 1 }
        return "1" + pad(left, 10) + pad(nextKey, 3) + "i"
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

    /// Scroll the tree toward its top: the whole screen on iPhone (the
    /// Library tab), the sidebar on iPad (a swipe in the middle would scroll
    /// the detail).
    func scrollTreeTowardTop(_ app: XCUIApplication) {
        #if targetEnvironment(macCatalyst)
        // no swipes on the Mac: a scroll wheel over the sidebar
        app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.12, dy: 0.5)).scroll(byDeltaX: 0, deltaY: 300)
        #else
        if UIDevice.current.userInterfaceIdiom == .pad {
            let from = app.coordinate(withNormalizedOffset: CGVector(dx: 0.12, dy: 0.35))
            from.press(forDuration: 0.05, thenDragTo: app.coordinate(withNormalizedOffset: CGVector(dx: 0.12, dy: 0.85)))
        } else {
            app.swipeDown()
        }
        #endif
    }

    /// A tree row (LibraryTree: one accessibility element per row). On the
    /// Mac a row scrolled out of the sidebar isn't in the accessibility
    /// tree: scroll the sidebar until it is (a new doc lands at the bottom).
    func row(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        let r = app.descendants(matching: .any)["tree.\(title)"].firstMatch
        #if targetEnvironment(macCatalyst)
        revealInSidebar(app, r)
        #endif
        return r
    }

    #if targetEnvironment(macCatalyst)
    func revealInSidebar(_ app: XCUIApplication, _ r: XCUIElement) {
        guard app.windows.firstMatch.waitForExistence(timeout: 10), !r.waitForExistence(timeout: 3) else { return }
        let sidebar = app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.12, dy: 0.6))
        for _ in 0..<25 where !r.exists {
            sidebar.scroll(byDeltaX: 0, deltaY: -240)
            RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        }
    }
    #endif

    /// Drag `from` onto `to` at a fraction of its height (0.5 = the middle:
    /// inside; 0.1 = its top edge: before it; 0.9 = after it), at a hand's
    /// pace, resting before letting go.
    func drag(_ from: XCUIElement, onto to: XCUIElement, at fraction: CGFloat) {
        #if targetEnvironment(macCatalyst)
        let start = from.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let end = to.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: fraction))
        // a mouse drag (a touch-style press doesn't start one on the Mac)
        start.click(forDuration: 0.3, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 1)
        #else
        // lift and move, as a finger does. A synthetic press sometimes ends
        // as the row's menu instead (iOS: held, not moved, it's the menu):
        // then the menu is dismissed and the drag tried again.
        let app = XCUIApplication()
        for _ in 0..<3 {
            let start = from.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
            let end = to.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: fraction))
            start.press(forDuration: 0.6, thenDragTo: end, withVelocity: .default, thenHoldForDuration: 1)
            let menu = app.buttons["Pin to Today"].firstMatch
            guard menu.exists || app.buttons["Unpin from Today"].firstMatch.exists else { return }
            // dismiss the menu (a tap well away from it) and try again
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.06)).tap()
            RunLoop.current.run(until: Date().addingTimeInterval(1))
        }
        #endif
    }

    /// Scroll the tree (a slow pan on the window, not a row press) until
    /// `rows` sit in the middle of the screen: a row low on screen is in the
    /// bottom auto-scroll band (it scrolls the tree away while a drag rests
    /// there), one under the status bar takes a press as scroll-to-top.
    func bringUp(_ rows: [XCUIElement], in app: XCUIApplication) {
        #if !targetEnvironment(macCatalyst)
        let h = app.windows.firstMatch.frame.height
        for _ in 0..<5 {
            let top = rows.map { $0.frame.minY }.min() ?? 0, bottom = rows.map { $0.frame.maxY }.max() ?? 0
            var by: CGFloat = 0
            if top < h * 0.22 { by = h * 0.3 - top }            // content down
            else if bottom > h * 0.65 { by = -min(bottom - h * 0.55, top - h * 0.3) } // content up
            guard abs(by) > 8 else { return }
            let from = app.coordinate(withNormalizedOffset: CGVector(dx: 0.94, dy: by > 0 ? 0.35 : 0.7))
            from.press(forDuration: 0.01, thenDragTo: from.withOffset(CGVector(dx: 0, dy: by)), withVelocity: .slow, thenHoldForDuration: 0.3)
            RunLoop.current.run(until: Date().addingTimeInterval(0.6))
        }
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

    /// A workspace section's menu opens the workspace sheets (they must have
    /// the app model: Move to… crashed without it).
    func testWorkspaceSheetsOpenFromTheSidebar() throws {
        #if !targetEnvironment(macCatalyst)
        throw XCTSkip("the sidebar switcher: the Mac and iPad")
        #else
        let made = try call("/api/workspaces", ["name": "Ws \(Int.random(in: 1000...9999))"]) as? [String: Any]
        // the next suites' docs are Unsorted: an empty workspace left behind
        // (and chosen in the switcher) would hide every one of them
        if let id = made?["id"] as? String {
            addTeardownBlock { [base] in
                var r = URLRequest(url: base.appending(path: "/api/workspaces/\(id)"))
                r.httpMethod = "DELETE"
                let gone = DispatchSemaphore(value: 0)
                URLSession.shared.dataTask(with: r) { _, _, _ in gone.signal() }.resume()
                _ = gone.wait(timeout: .now() + 10)
            }
        }
        let app = launch()
        // each workspace is a section of the sidebar; a header's menu has the sheets
        let header = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'sidebar.workspace.'")).firstMatch
        XCTAssertTrue(header.waitForExistence(timeout: 20), "the workspace sections show")
        for item in ["New workspace\u{2026}", "Manage workspaces"] {
            header.rightClick()
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

    /// The doc with this title on the daemon (id, parent, epoch).
    func docNamed(_ title: String) throws -> (id: String, parent: String?, epoch: Int)? {
        var r = URLRequest(url: base.appending(path: "/api/docs"))
        r.timeoutInterval = 10
        let done = expectation(description: "docs")
        var out: Data?
        URLSession.shared.dataTask(with: r) { d, _, _ in out = d; done.fulfill() }.resume()
        wait(for: [done], timeout: 15)
        let list = try XCTUnwrap(try JSONSerialization.jsonObject(with: try XCTUnwrap(out)) as? [[String: Any]])
        guard let d = list.first(where: { $0["title"] as? String == title }), let id = d["id"] as? String else { return nil }
        return (id, d["parent_id"] as? String, (d["current_epoch"] as? Int) ?? -1)
    }

    /// Name the folder in the New folder prompt and create it.
    func nameFolder(_ app: XCUIApplication, _ name: String) {
        #if targetEnvironment(macCatalyst)
        // Catalyst shows the alert as a sheet (its field loses the identifier)
        let nameField = app.sheets["New folder"].textFields.firstMatch
        XCTAssertTrue(nameField.waitForExistence(timeout: 5), "New folder asks for a name")
        nameField.click()
        nameField.typeText(name)
        app.buttons["newfolder.create"].firstMatch.click()
        #else
        let alert = app.alerts["New folder"].firstMatch
        if !alert.waitForExistence(timeout: 5) { shot("no New folder prompt") }
        XCTAssertTrue(alert.exists, "New folder asks for a name")
        let field = alert.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 3), "the prompt has a name field")
        hit(field)
        field.typeText(name)
        hit(alert.buttons["Create"].firstMatch)
        #endif
    }

    /// New folder at the top level: an empty folder (not opened as a doc),
    /// a click opens it, and a doc dropped on its "Empty" line goes in.
    func testNewFolderIsAnEmptyFolderToDropInto() throws {
        let n = Int.random(in: 1000...9999)
        let x = try makeDoc("Loose X \(n)")
        let app = launch()
        let rowX = row(app, "Loose X \(n)")
        XCTAssertTrue(rowX.waitForExistence(timeout: 20))
        #if targetEnvironment(macCatalyst)
        // the current workspace's header shows New folder under the pointer
        let header = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'sidebar.workspace.'")).firstMatch
        if header.waitForExistence(timeout: 5) { header.hover() }
        let button = app.buttons["tree.newFolder"].firstMatch
        #else
        let button = app.buttons["New folder"].firstMatch
        #endif
        XCTAssertTrue(button.waitForExistence(timeout: 5), "there is a New folder button")
        hit(button)
        nameFolder(app, "Box \(n)")
        let folder = row(app, "Box \(n)")
        XCTAssertTrue(folder.waitForExistence(timeout: 15), "the folder is in the tree")
        XCTAssertEqual(folder.value as? String, "folder, closed", "it shows as a folder")
        let made = try XCTUnwrap(try docNamed("Box \(n)"))
        XCTAssertEqual(made.epoch, 0, "nothing written in it")
        XCTAssertNil(made.parent, "at the top level")
        XCTAssertFalse(app.textViews.firstMatch.exists, "it didn't open as a doc")
        // a new folder goes to the bottom of the top level, X to the top:
        // with other suites' docs between them, both can't be on screen.
        // Put the folder right under X, then back to the top of the tree.
        let placed = try call("/api/doc/\(made.id)/move", ["parent_id": NSNull(), "sort_key": nextTopKey()])
        XCTAssertEqual((placed as? [String: Any])?["ok"] as? Bool, true, "the folder placed under X: \(placed)")
        let settle = Date().addingTimeInterval(20)
        while Date() < settle, !(rowX.isHittable && folder.isHittable && folder.frame.minY > rowX.frame.minY) {
            scrollTreeTowardTop(app)
            RunLoop.current.run(until: Date().addingTimeInterval(0.5))
        }
        XCTAssertTrue(rowX.isHittable && folder.isHittable, "X and the folder are both on screen")
        // its chevron opens it (a click on the row opens its page, as for any doc)
        let chevron = folder.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0.5)).withOffset(CGVector(dx: 10, dy: 0))
        #if targetEnvironment(macCatalyst)
        chevron.click()
        #else
        chevron.tap()
        #endif
        let empty = app.descendants(matching: .any)["tree.empty"].firstMatch
        XCTAssertTrue(empty.waitForExistence(timeout: 5), "an open empty folder says so")
        XCTAssertEqual(folder.value as? String, "folder, open")
        shot("before the drop")
        drag(rowX, onto: empty, at: 0.5)
        shot("after the drop")
        try waitForParent(of: x, made.id)
    }

    /// New folder here (the row's menu): made inside that doc.
    func testNewFolderHereFromTheMenu() throws {
        let n = Int.random(in: 1000...9999)
        let p = try makeDoc("Parent P \(n)")
        let app = launch()
        let rowP = row(app, "Parent P \(n)")
        XCTAssertTrue(rowP.waitForExistence(timeout: 20))
        #if targetEnvironment(macCatalyst)
        rowP.rightClick()
        let item = app.menuItems["New folder here"].firstMatch
        #else
        rowP.press(forDuration: 1.2)
        let item = app.buttons["New folder here"].firstMatch
        #endif
        XCTAssertTrue(item.waitForExistence(timeout: 5), "the menu offers New folder here")
        hit(item)
        nameFolder(app, "Inner \(n)")
        XCTAssertTrue(row(app, "Inner \(n)").waitForExistence(timeout: 15), "the new folder shows, its parent opened")
        let made = try XCTUnwrap(try docNamed("Inner \(n)"))
        XCTAssertEqual(made.parent, p, "made inside P")
    }

    /// The tree shows a drop at once, whatever the server is doing: the app
    /// talks to a slow server (a proxy holding each request 3 s, :7520);
    /// within a second of the drop the doc is where it was put.
    func testTheTreeMovesAtOnceOnASlowServer() throws {
        let slow = ProcessInfo.processInfo.environment["TAISCE_SLOW_URL"] ?? "http://127.0.0.1:7520"
        let n = Int.random(in: 1000...9999)
        _ = try makeDoc("Fast A \(n)"); _ = try makeDoc("Fast B \(n)")
        let x = try makeDoc("Fast X \(n)"); _ = try makeDoc("Fast Z \(n)")
        _ = x
        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", slow, "-library.expanded", ""]
        app.launch()
        let tab = app.tabBars.buttons["Library"]
        if tab.waitForExistence(timeout: 5) { tab.tap() }
        let rowA = row(app, "Fast A \(n)"), rowB = row(app, "Fast B \(n)")
        let rowX = row(app, "Fast X \(n)"), rowZ = row(app, "Fast Z \(n)")
        XCTAssertTrue(rowA.waitForExistence(timeout: 60) && rowB.exists && rowX.exists && rowZ.exists, "the Library lists them")
        sleep(4) // let the first sync settle
        // into B: shown inside B at once
        drag(rowA, onto: rowB, at: 0.5)
        let t0 = Date()
        var inside = false
        while Date().timeIntervalSince(t0) < 1.0 {
            if rowB.value as? String == "folder, open", rowA.exists, rowA.frame.minY > rowB.frame.minY { inside = true; break }
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        if !inside { shot("not moved at once") }
        XCTAssertTrue(inside, "A shows inside B within a second of the drop")
        // and it stays there while the server catches up
        RunLoop.current.run(until: Date().addingTimeInterval(8))
        XCTAssertGreaterThan(rowA.frame.minY, rowB.frame.minY, "A is still inside B after the server answered")
        XCTAssertEqual(rowB.value as? String, "folder, open")
        // a reorder: Z before X, at once
        drag(rowZ, onto: rowX, at: 0.1)
        let t1 = Date()
        var above = false
        while Date().timeIntervalSince(t1) < 1.0 {
            if rowZ.frame.minY < rowX.frame.minY { above = true; break }
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        if !above { shot("not reordered at once") }
        XCTAssertTrue(above, "Z shows above X within a second of the drop")
        RunLoop.current.run(until: Date().addingTimeInterval(8))
        XCTAssertLessThan(rowZ.frame.minY, rowX.frame.minY, "Z is still above X after the server answered")
    }

    /// Two moves in a row, each a different doc: the second moves the doc
    /// in hand, not the one dragged before it (on the Mac a new drag
    /// re-fired the previous drag's handler and the drop moved that doc).
    func testTwoDragsInARowEachMoveTheirOwnDoc() throws {
        let n = Int.random(in: 1000...9999)
        // a few rows apart: on iPhone a press that moves one row (44 pt)
        // can turn into the row's menu rather than a drag
        let a = try makeDoc("First A \(n)"), b = try makeDoc("Second B \(n)")
        for i in 0..<3 { _ = try makeDoc("Spacer \(i) \(n)") }
        let f = try makeDoc("Target F \(n)")
        let app = launch()
        let rowF = row(app, "Target F \(n)"), rowA = row(app, "First A \(n)"), rowB = row(app, "Second B \(n)")
        XCTAssertTrue(rowF.waitForExistence(timeout: 20) && rowA.exists && rowB.exists)
        // the tree settles first (the docs were made and placed just before launch)
        let settle = Date().addingTimeInterval(10)
        while !(rowA.frame.minY < rowB.frame.minY && rowB.frame.minY < rowF.frame.minY), Date() < settle {
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        }
        RunLoop.current.run(until: Date().addingTimeInterval(2))
        bringUp([rowA, rowF], in: app)
        drag(rowA, onto: rowF, at: 0.5)
        try waitForParent(of: a, f)
        // the tree settles again: F open with A inside, nothing still moving
        let open = Date().addingTimeInterval(10)
        var last = rowB.frame
        while Date() < open {
            RunLoop.current.run(until: Date().addingTimeInterval(0.4))
            let now = rowB.frame
            if rowA.exists, rowA.frame.minY > rowF.frame.minY, now == last { break }
            last = now
        }
        bringUp([rowB, rowF], in: app)
        drag(rowB, onto: rowF, at: 0.5)
        try waitForParent(of: b, f)
        XCTAssertEqual(try parent(of: a), f, "A stayed where it was put")
    }
}
