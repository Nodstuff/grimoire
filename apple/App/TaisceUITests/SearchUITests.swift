import XCTest

/// Search, then open a result, against a LOCAL scratch daemon
/// (`TEST_RUNNER_TAISCE_UI_URL`, default 127.0.0.1:7518): a root doc and one
/// inside a collapsed folder, which has no row in the sidebar.
final class SearchUITests: XCTestCase {
    var base: URL { URL(string: ProcessInfo.processInfo.environment["TAISCE_UI_URL"] ?? "http://127.0.0.1:7518")! }

    func call(_ path: String, _ body: [String: Any]) throws -> [String: Any] {
        var r = URLRequest(url: base.appending(path: path))
        r.timeoutInterval = 10
        r.httpMethod = "POST"
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = try JSONSerialization.data(withJSONObject: body)
        let done = expectation(description: path)
        var out: Data?
        URLSession.shared.dataTask(with: r) { d, _, _ in out = d; done.fulfill() }.resume()
        wait(for: [done], timeout: 15)
        return try XCTUnwrap(try JSONSerialization.jsonObject(with: try XCTUnwrap(out, "no answer from \(path)")) as? [String: Any])
    }

    func makeDoc(_ title: String, markdown: String, parent: String? = nil) throws -> String {
        var body: [String: Any] = ["title": title]
        if let parent { body["parent_id"] = parent }
        let id = try XCTUnwrap(try call("/api/docs", body)["id"] as? String)
        _ = try call("/api/propose_markdown", ["doc_id": id, "base_epoch": 0, "markdown": markdown])
        return id
    }

    func openFromSearch(word: String, heading: String) {
        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString, "-tab", "search", "-searchQuery", word]
        app.launch()
        let result = app.buttons.containing(NSPredicate(format: "label CONTAINS %@", word)).firstMatch
        XCTAssertTrue(result.waitForExistence(timeout: 20), "the search lists the seeded doc")
        result.tap()
        XCTAssertTrue(app.staticTexts[heading].waitForExistence(timeout: 10), "the result opened")
    }

    func testOpensARootDocFromSearch() throws {
        let word = "quokka\(Int.random(in: 1000...9999))"
        _ = try makeDoc("Root \(word)", markdown: "# Root heading \(word)\n\nThe \(word) lives here.")
        openFromSearch(word: word, heading: "Root heading \(word)")
    }

    func testOpensANestedDocFromSearch() throws {
        let word = "wombat\(Int.random(in: 1000...9999))"
        let folder = try makeDoc("Folder \(word.suffix(4))", markdown: "Folder body")
        _ = try makeDoc("Nested \(word)", markdown: "# Nested heading \(word)\n\nThe \(word) lives here.", parent: folder)
        openFromSearch(word: word, heading: "Nested heading \(word)")
    }
}
