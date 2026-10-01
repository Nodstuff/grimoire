import XCTest

/// Launches the app against a LOCAL scratch daemon and opens a synced doc.
/// Needs a daemon serving a doc titled "Welcome" whose first heading is
/// "Welcome to Taisce" (`TEST_RUNNER_TAISCE_UI_URL`, default 127.0.0.1:7515).
/// The screenshot is an attachment: `xcrun xcresulttool export attachments`.
final class LaunchTests: XCTestCase {
    func testStartsSyncsAndRendersADoc() throws {
        let url = ProcessInfo.processInfo.environment["TAISCE_UI_URL"] ?? "http://127.0.0.1:7515"
        let app = XCUIApplication()
        // the argument domain overrides UserDefaults' saved server
        app.launchArguments = ["-serverURL", url]
        app.launch()
        let back = app.navigationBars.buttons.firstMatch
        if back.waitForExistence(timeout: 5), !app.staticTexts["Welcome"].exists { back.tap() }
        let doc = app.staticTexts["Welcome"]
        XCTAssertTrue(doc.waitForExistence(timeout: 15), "the synced tree lists the seeded doc")
        doc.tap()
        XCTAssertTrue(app.staticTexts["Welcome to Taisce"].waitForExistence(timeout: 15), "the body rendered")
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.lifetime = .keepAlways
        add(shot)
    }
}
