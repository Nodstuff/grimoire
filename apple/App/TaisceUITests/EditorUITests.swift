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

    func testTypeSplitMergeLinkInTheLiveSurface() throws {
        let tag = String(UUID().uuidString.prefix(6))
        let title = "Editor \(tag)"
        let target = "Roadmap \(tag)"
        _ = try makeDoc(target, markdown: nil)
        let id = try makeDoc(title, markdown: "Hello world\n\nLast line")

        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString, "-openDoc", title]
        app.launch()

        // no Edit button: the doc opens live
        let surface = app.textViews["surface.text"]
        XCTAssertTrue(surface.waitForExistence(timeout: 20), "the doc opened as one text surface")
        XCTAssertFalse(app.buttons["doc.edit"].exists, "there is no Edit mode")
        XCTAssertTrue((surface.value as? String)?.hasPrefix("Hello world\nLast line") == true)

        // click at the end of the first line, type
        surface.tap()
        surface.typeText(XCUIKeyboardKey.upArrow.rawValue)
        // ⌘← / ⌘→ are not on the iOS keyboard: the caret goes to the doc's start, then the line's end
        for _ in 0..<40 { surface.typeText(XCUIKeyboardKey.leftArrow.rawValue) }
        for _ in 0..<11 { surface.typeText(XCUIKeyboardKey.rightArrow.rawValue) }
        surface.typeText(" again")
        XCTAssertTrue((surface.value as? String)?.hasPrefix("Hello world again\nLast line") == true)

        // Return: a new line, typed into
        surface.typeText("\nSecond")
        XCTAssertTrue((surface.value as? String)?.hasPrefix("Hello world again\nSecond\nLast line") == true)

        // Backspace at its start joins it onto the first line
        for _ in 0..<6 { surface.typeText(XCUIKeyboardKey.leftArrow.rawValue) }
        surface.typeText(XCUIKeyboardKey.delete.rawValue)
        XCTAssertTrue((surface.value as? String)?.hasPrefix("Hello world againSecond\nLast line") == true)

        // [[ at the join, pick the suggestion
        surface.typeText(" [[\(tag)")
        let pick = app.buttons.matching(NSPredicate(format: "identifier == 'wiki.suggestion' AND label BEGINSWITH %@", target)).firstMatch
        XCTAssertTrue(pick.waitForExistence(timeout: 5), "the [[ popup offers the doc being typed")
        pick.tap()
        XCTAssertTrue((surface.value as? String)?.hasPrefix("Hello world again \(target) Second\nLast line") == true)

        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "editing"
        shot.lifetime = .keepAlways
        add(shot)

        // the daemon has exactly what was typed (autosave, no Done)
        let want = "Hello world again [[\(target)]] Second\n\nLast line\n"
        var got = ""
        for _ in 0..<40 {
            got = try markdown(id)
            if got == want { break }
            Thread.sleep(forTimeInterval: 0.5)
        }
        XCTAssertEqual(got, want)
    }
}
