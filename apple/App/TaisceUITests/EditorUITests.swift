import XCTest

/// Edit a doc end to end against a LOCAL scratch daemon
/// (`TEST_RUNNER_TAISCE_UI_URL`, default 127.0.0.1:7518): type, Return
/// for a new block, Backspace-merge, `[[` and pick a suggestion, Done —
/// then read the doc's markdown back from the daemon.
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

    func testEditTypeSplitMergeLinkAndDone() throws {
        let tag = String(UUID().uuidString.prefix(6))
        let title = "Editor \(tag)"
        let target = "Roadmap \(tag)"
        _ = try makeDoc(target, markdown: nil)
        let id = try makeDoc(title, markdown: "Hello world")

        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString, "-openDoc", title]
        app.launch()

        let edit = app.buttons["doc.edit"]
        XCTAssertTrue(edit.waitForExistence(timeout: 20), "the doc opened and can be edited")
        edit.tap()
        let blocks = app.textViews.matching(identifier: "editor.block")
        let first = blocks.element(boundBy: 0)
        XCTAssertTrue(first.waitForExistence(timeout: 10), "edit mode shows the block")
        XCTAssertEqual(first.value as? String, "Hello world")

        // type at the end of the block
        first.coordinate(withNormalizedOffset: CGVector(dx: 0.97, dy: 0.5)).tap()
        first.typeText(" again")
        XCTAssertEqual(first.value as? String, "Hello world again")

        // Return: a new block below, with the caret in it
        first.typeText("\n")
        XCTAssertTrue(blocks.element(boundBy: 1).waitForExistence(timeout: 5), "Return made a new block")
        app.typeText("Second")
        let second = blocks.element(boundBy: 1)
        XCTAssertEqual(second.value as? String, "Second")

        // Backspace at its start joins it onto the first block
        second.coordinate(withNormalizedOffset: CGVector(dx: 0.0, dy: 0.5)).withOffset(CGVector(dx: 1, dy: 0)).tap()
        app.typeText(XCUIKeyboardKey.delete.rawValue)
        for _ in 0..<50 where blocks.count != 1 { Thread.sleep(forTimeInterval: 0.1) }
        XCTAssertEqual(blocks.count, 1, "Backspace merged the blocks")
        XCTAssertEqual(blocks.element(boundBy: 0).value as? String, "Hello world againSecond")

        // [[ at the join, pick the suggestion
        app.typeText(" [[\(tag)")
        let suggestion = app.buttons.matching(identifier: "wiki.suggestion").firstMatch
        XCTAssertTrue(suggestion.waitForExistence(timeout: 5), "the [[ popup offers docs")
        let pick = app.buttons.matching(NSPredicate(format: "identifier == 'wiki.suggestion' AND label BEGINSWITH %@", target)).firstMatch
        XCTAssertTrue(pick.waitForExistence(timeout: 5), "and the one being typed")
        pick.tap()
        XCTAssertEqual(blocks.element(boundBy: 0).value as? String, "Hello world again \(target) Second")

        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "editing"
        shot.lifetime = .keepAlways
        add(shot)

        app.buttons["editor.done"].tap()
        XCTAssertTrue(app.buttons["doc.edit"].waitForExistence(timeout: 10), "back to reading")

        // the daemon has exactly what was typed
        let want = "Hello world again [[\(target)]] Second\n"
        var got = ""
        for _ in 0..<40 {
            got = try markdown(id)
            if got == want { break }
            Thread.sleep(forTimeInterval: 0.5)
        }
        XCTAssertEqual(got, want)

        let reading = XCTAttachment(screenshot: app.screenshot())
        reading.name = "reading after Done"
        reading.lifetime = .keepAlways
        add(reading)
    }
}
