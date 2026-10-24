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
        // iPhone: the Library tab; iPad and the Mac: the sidebar
        let tab = app.tabBars.buttons["Library"]
        if tab.waitForExistence(timeout: 3) { tab.tap() }
        // a Library tree row (LibraryTree: one element per row)
        let row = app.descendants(matching: .any)["tree.\(title)"].firstMatch
        #if targetEnvironment(macCatalyst)
        // a row scrolled out of the Mac's sidebar isn't in the accessibility tree
        if app.windows.firstMatch.waitForExistence(timeout: 10), !row.waitForExistence(timeout: 5) {
            let sidebar = app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.12, dy: 0.6))
            for _ in 0..<25 where !row.exists {
                sidebar.scroll(byDeltaX: 0, deltaY: -240)
                RunLoop.current.run(until: Date().addingTimeInterval(0.3))
            }
        }
        #endif
        XCTAssertTrue(row.waitForExistence(timeout: 20), "the sidebar lists the seeded doc")
        #if targetEnvironment(macCatalyst)
        row.rightClick()
        let menuItem = app.menuItems["Delete\u{2026}"].firstMatch
        #else
        row.press(forDuration: 1.2)
        let menuItem = app.buttons["Delete\u{2026}"].firstMatch
        #endif
        XCTAssertTrue(menuItem.waitForExistence(timeout: 5), "the context menu offers Delete…")
        #if targetEnvironment(macCatalyst)
        menuItem.click()
        #else
        menuItem.tap()
        #endif
        #if targetEnvironment(macCatalyst)
        // Catalyst shows the alert as a sheet on the window
        let confirm = app.sheets.buttons["Delete"].firstMatch.exists ? app.sheets.buttons["Delete"].firstMatch
            : app.dialogs.buttons["Delete"].firstMatch.exists ? app.dialogs.buttons["Delete"].firstMatch
            : app.buttons.matching(NSPredicate(format: "label == 'Delete'")).firstMatch
        #else
        let confirm = app.alerts.buttons["Delete"].firstMatch
        #endif
        XCTAssertTrue(confirm.waitForExistence(timeout: 5), "it asks first")
        #if targetEnvironment(macCatalyst)
        confirm.click()
        #else
        confirm.tap()
        #endif

        XCTAssertTrue(row.waitForNonExistence(timeout: 15), "the row went")
        let trash = try XCTUnwrap(try call("/api/trash") as? [[String: Any]], "trash is a list")
        XCTAssertTrue(trash.contains { (($0["doc"] as? [String: Any])?["id"] as? String) == id }, "the daemon has it in the Trash")
    }
}
