import XCTest

/// Delete a doc from the sidebar's context menu against a LOCAL scratch
/// daemon (`TEST_RUNNER_TAISCE_UI_URL`, default 127.0.0.1:7518), then check
/// the daemon put it in the Trash. iPad layout (the sidebar); on iPhone the
/// Library tab's rows carry the same menu.
final class DeleteDocUITests: XCTestCase {
    var base: URL { URL(string: ProcessInfo.processInfo.environment["TAISCE_UI_URL"] ?? "http://127.0.0.1:7518")! }

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

    func testDeleteFromTheSidebarTrashesTheDoc() throws {
        let title = "Doomed \(Int.random(in: 1000...9999))"
        let doc = try XCTUnwrap(try call("/api/docs", ["title": title]) as? [String: Any])
        let id = try XCTUnwrap(doc["id"] as? String)

        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString]
        app.launch()
        let row = app.staticTexts[title]
        XCTAssertTrue(row.waitForExistence(timeout: 20), "the sidebar lists the seeded doc")
        row.press(forDuration: 1.2)
        let menuItem = app.buttons["Delete\u{2026}"]
        XCTAssertTrue(menuItem.waitForExistence(timeout: 5), "the context menu offers Delete…")
        menuItem.tap()
        let confirm = app.alerts.buttons["Delete"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 5), "it asks first")
        confirm.tap()

        XCTAssertTrue(row.waitForNonExistence(timeout: 15), "the row went")
        let trash = try XCTUnwrap(try call("/api/trash") as? [[String: Any]], "trash is a list")
        XCTAssertTrue(trash.contains { (($0["doc"] as? [String: Any])?["id"] as? String) == id }, "the daemon has it in the Trash")
    }
}
