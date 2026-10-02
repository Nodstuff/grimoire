import XCTest

/// Runnable code blocks end to end on the Mac, against a LOCAL scratch
/// daemon (`TAISCE_UI_URL`, default 127.0.0.1:7519): a bash block written by
/// an agent, Run, the trust sheet appears, its Run starts the run, the
/// output arrives. Run it with a throwaway bundle id, never as the installed
/// app: `xcodebuild … TAISCE_BUNDLE_ID=ie.null.taisce.migtest -only-testing:TaisceUITests/RunCodeUITests test`.
/// Needs synthesized input to reach the app: on the agent's Mac (2026-10-02)
/// no tap reached it at all (not even the toolbar's Edit), so this has not
/// passed yet. In-app substitutes: `RunCodeAppTests.runPresentsTheTrustSheetOnScreen`
/// (a real window presents the sheet, its Run action runs) and
/// `trustSheetButtonsInEitherOrder` (the sheet's own actions and binding).
@MainActor final class RunCodeUITests: XCTestCase {
    var base: URL { URL(string: ProcessInfo.processInfo.environment["TAISCE_UI_URL"] ?? "http://127.0.0.1:7519")! }

    func call(_ path: String, _ body: [String: Any]? = nil, as principal: String? = nil) throws -> Any {
        var r = URLRequest(url: base.appending(path: path))
        r.timeoutInterval = 10
        if let principal { r.setValue(principal, forHTTPHeaderField: "Taisce-Principal") }
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

    func testRunAsksThenRunsFromTheSheet() throws {
        #if !targetEnvironment(macCatalyst)
        throw XCTSkip("code runs on the Mac only")
        #else
        let tag = String(UUID().uuidString.prefix(6))
        let title = "Runs \(tag)"
        let agent = "claude:uitest-\(tag)"
        let doc = try XCTUnwrap(try call("/api/docs", ["title": title], as: agent) as? [String: Any])
        let id = try XCTUnwrap(doc["id"] as? String)
        _ = try call("/api/propose_markdown", ["doc_id": id, "base_epoch": 0, "markdown": "```bash\necho from-the-sheet-\(tag)\n```"], as: agent)

        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base.absoluteString, "-openDoc", title]
        app.launchEnvironment["TAISCE_UI_TEST"] = "1"
        app.launch()

        let run = app.buttons["code.run"]
        XCTAssertTrue(run.waitForExistence(timeout: 30), "the bash block has a Run control")
        // a Mac window that isn't key takes the first click to activate:
        // make it key before pressing Run
        app.activate()
        app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.7, dy: 0.85)).tap()
        run.tap()
        let question = app.staticTexts["code.trust.question"]
        if !question.waitForExistence(timeout: 15) {
            let failShot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
            failShot.name = "after-run"
            failShot.lifetime = .keepAlways
            add(failShot)
            XCTFail("the trust sheet appears")
            return
        }
        XCTAssertTrue(question.label.contains(agent), "it names the agent: \(question.label)")
        let sheetRun = app.buttons["code.trust.run"]
        XCTAssertTrue(sheetRun.exists)
        sheetRun.tap()

        let output = app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", "from-the-sheet-\(tag)")).firstMatch
        XCTAssertTrue(output.waitForExistence(timeout: 20), "the run's output arrives")
        XCTAssertFalse(question.exists, "the sheet has gone")
        let status = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'exit 0'")).firstMatch
        XCTAssertTrue(status.waitForExistence(timeout: 5), "exit 0")
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.lifetime = .keepAlways
        add(shot)

        // approved now: the next Run doesn't ask
        run.tap()
        XCTAssertFalse(question.waitForExistence(timeout: 3), "no second question")
        #endif
    }
}
