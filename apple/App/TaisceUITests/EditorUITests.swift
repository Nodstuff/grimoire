import XCTest

/// Edit a doc end to end against a LOCAL scratch daemon
/// (`TEST_RUNNER_TAISCE_UI_URL`, default 127.0.0.1:7518) in the live
/// surface: type, Return for a new block, Backspace-merge, `[[` and pick a
/// suggestion — then read the doc's markdown back from the daemon (autosave).
final class EditorUITests: XCTestCase {
    var base: URL { URL(string: ProcessInfo.processInfo.environment["TAISCE_UI_URL"] ?? "http://127.0.0.1:7518")! }

    // MARK: the daemon's API (seeding and checking)

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

    func makeDoc(_ title: String, markdown: String?) throws -> String {
        let doc = try XCTUnwrap(try call("/api/docs", ["title": title]) as? [String: Any])
        let id = try XCTUnwrap(doc["id"] as? String)
        if let markdown {
            _ = try call("/api/propose_markdown", ["doc_id": id, "base_epoch": 0, "markdown": markdown])
        }
        return id
    }

    func markdown(_ id: String) throws -> String {
        let m = try XCTUnwrap(try call("/api/doc/\(id)/markdown") as? [String: Any])
        return try XCTUnwrap(m["markdown"] as? String)
    }

    /// The doc's live blocks as (id, content), from the daemon's tree.
    func blocks(_ id: String) throws -> [(id: String, content: String)] {
        let tree = try XCTUnwrap(try call("/api/doc/\(id)") as? [String: Any])
        var out: [(String, String)] = []
        func walk(_ nodes: [[String: Any]]) {
            for n in nodes {
                if let b = n["block"] as? [String: Any], let bid = b["id"] as? String, let c = b["content"] as? String,
                   (b["deleted"] as? Bool) != true {
                    out.append((bid, c))
                }
                walk(n["children"] as? [[String: Any]] ?? [])
            }
        }
        walk(tree["roots"] as? [[String: Any]] ?? [])
        return out
    }

    /// Two SAVED blocks joined: Backspace at the second's start. The first
    /// keeps its id and takes the text; the second's id is gone.
    func testBackspaceJoinsTwoSavedBlocks() throws {
        let tag = String(UUID().uuidString.prefix(6))
        let title = "Join \(tag)"
        let id = try makeDoc(title, markdown: "First line\n\nSecond line")
        let before = try blocks(id)
        XCTAssertEqual(before.map(\.content), ["First line", "Second line"])
        let (firstID, secondID) = (before[0].id, before[1].id)

        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString, "-openDoc", title]
        app.launch()
        let surface = app.textViews["surface.text"]
        XCTAssertTrue(surface.waitForExistence(timeout: 20))
        func text() -> String { (surface.value as? String) ?? "" }
        XCTAssertEqual(text(), "First line\nSecond line\n")

        // the caret at the end, then to the start of "Second line" (⌘←)
        surface.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)).tap()
        surface.typeKey(.leftArrow, modifierFlags: .command)
        surface.typeText(XCUIKeyboardKey.delete.rawValue)
        XCTAssertEqual(text(), "First lineSecond line\n")

        // the daemon, after the autosave: exactly that, under the first id
        var got: [(id: String, content: String)] = []
        for _ in 0..<40 {
            got = try blocks(id)
            if got.count == 1 { break }
            Thread.sleep(forTimeInterval: 0.5)
        }
        XCTAssertEqual(got.map(\.content), ["First lineSecond line"])
        XCTAssertEqual(got.map(\.id), [firstID])
        XCTAssertFalse(got.contains { $0.id == secondID }, "the joined block is gone")
        XCTAssertEqual(try markdown(id), "First lineSecond line\n")
    }

    /// A point in the text column, `fraction` of the way down the surface,
    /// `dx` points into the column (iPhone: the column starts 20 pt in).
    func columnPoint(_ surface: XCUIElement, dx: CGFloat, fraction: CGFloat) -> XCUICoordinate {
        surface.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: fraction)).withOffset(CGVector(dx: 20 + dx, dy: 0))
    }

    /// As in Notes: a tap on a link follows it; no caret, no keyboard.
    func testTappingAWikilinkFollowsItWithoutTheKeyboard() throws {
        let tag = String(UUID().uuidString.prefix(6))
        let target = "Target \(tag)"
        _ = try makeDoc(target, markdown: "You followed the link.")
        // links fill every line, so the tap lands on one
        let line = Array(repeating: "[[\(target)]]", count: 12).joined(separator: " ")
        _ = try makeDoc("Links \(tag)", markdown: Array(repeating: line, count: 12).joined(separator: "\n\n"))

        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString, "-openDoc", "Links \(tag)"]
        app.launch()
        let surface = app.textViews["surface.text"]
        XCTAssertTrue(surface.waitForExistence(timeout: 20))
        columnPoint(surface, dx: 6, fraction: 0.55).tap()
        XCTAssertTrue(app.staticTexts[target].waitForExistence(timeout: 10), "the link was followed")
        XCTAssertEqual(app.keyboards.count, 0, "no keyboard")
    }

    /// A tick: no caret, no keyboard, and it's saved.
    func testTickingABoxDoesNotRaiseTheKeyboard() throws {
        let tag = String(UUID().uuidString.prefix(6))
        let title = "Ticks \(tag)"
        let items = (1...30).map { "- [ ] item \($0)" }.joined(separator: "\n")
        let id = try makeDoc(title, markdown: items)
        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString, "-openDoc", title]
        app.launch()
        let surface = app.textViews["surface.text"]
        XCTAssertTrue(surface.waitForExistence(timeout: 20))
        XCTAssertEqual(app.keyboards.count, 0, "no keyboard before")
        columnPoint(surface, dx: 8, fraction: 0.55).tap()
        XCTAssertTrue((surface.value as? String ?? "").contains("☑"), "a box was ticked")
        XCTAssertEqual(app.keyboards.count, 0, "no keyboard")
        var got = ""
        for _ in 0..<40 {
            got = try markdown(id)
            if got.contains("- [x]") { break }
            Thread.sleep(forTimeInterval: 0.5)
        }
        XCTAssertTrue(got.contains("- [x]"), "the tick was saved")
    }

    func testTypeSplitMergeLinkInTheLiveSurface() throws {
        let tag = String(UUID().uuidString.prefix(6))
        let title = "Editor \(tag)"
        let target = "Roadmap \(tag)"
        _ = try makeDoc(target, markdown: nil)
        let id = try makeDoc(title, markdown: "Hello world")

        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString, "-openDoc", title]
        app.launch()

        // no Edit button: the doc opens live
        let surface = app.textViews["surface.text"]
        XCTAssertTrue(surface.waitForExistence(timeout: 20), "the doc opened as one text surface")
        XCTAssertFalse(app.buttons["doc.edit"].exists, "there is no Edit mode")
        func text() -> String { (surface.value as? String) ?? "" }
        XCTAssertEqual(text(), "Hello world\n")

        // a click below the text puts the caret at the end; type
        surface.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)).tap()
        surface.typeText(" again")
        XCTAssertEqual(text(), "Hello world again\n")

        // Return: a new line, typed into
        surface.typeText("\nSecond")
        XCTAssertEqual(text(), "Hello world again\nSecond\n")

        // Backspace back over it, and at the empty line's start joins it up
        for _ in 0..<7 { surface.typeText(XCUIKeyboardKey.delete.rawValue) }
        XCTAssertEqual(text(), "Hello world again\n")

        // [[ and pick the suggestion
        surface.typeText(" [[\(tag)")
        let pick = app.buttons.matching(NSPredicate(format: "identifier == 'wiki.suggestion' AND label BEGINSWITH %@", target)).firstMatch
        XCTAssertTrue(pick.waitForExistence(timeout: 5), "the [[ popup offers the doc being typed")
        pick.tap()
        surface.typeText("end")
        XCTAssertEqual(text(), "Hello world again \(target) end\n")

        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "editing"
        shot.lifetime = .keepAlways
        add(shot)

        // the daemon has exactly what was typed (autosave, no Done)
        let want = "Hello world again [[\(target)]] end\n"
        var got = ""
        for _ in 0..<40 {
            got = try markdown(id)
            if got == want { break }
            Thread.sleep(forTimeInterval: 0.5)
        }
        XCTAssertEqual(got, want)
    }
}
