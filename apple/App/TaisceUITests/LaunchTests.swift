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
        // the tree lives in Library: a tab on iPhone, the sidebar on iPad
        let library = app.tabBars.buttons["Library"]
        if library.waitForExistence(timeout: 10) { library.tap() }
        let doc = app.descendants(matching: .any)["tree.Welcome"].firstMatch
        XCTAssertTrue(doc.waitForExistence(timeout: 15), "the synced tree lists the seeded doc")
        #if targetEnvironment(macCatalyst)
        doc.click()
        #else
        doc.tap()
        #endif
        // the doc is one editing surface (a text view), not static texts
        let body = app.textViews.firstMatch
        XCTAssertTrue(body.waitForExistence(timeout: 15), "the doc opened")
        let end = Date().addingTimeInterval(15)
        while !((body.value as? String) ?? "").contains("Welcome to Taisce"), Date() < end {
            RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        }
        XCTAssertTrue(((body.value as? String) ?? "").contains("Welcome to Taisce"), "the body rendered")
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.lifetime = .keepAlways
        add(shot)
    }
}
