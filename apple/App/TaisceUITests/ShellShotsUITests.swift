import XCTest

/// Landscape iPad screenshots of the shell (the sidebar with Today, To-dos,
/// Review, Books, Search and Settings) for the design comparison. Skipped
/// unless `TEST_RUNNER_TAISCE_SHOTS` names a directory on the host;
/// `TEST_RUNNER_TAISCE_UI_URL` is the daemon. Each shot is
/// `ipad-<name>-<suffix>.png`, taken in the current appearance.
final class ShellShotsUITests: XCTestCase {
    /// iPad landscape shots: the Mac's runner reports the iPad idiom but has no orientation
    static var onMac: Bool {
        #if targetEnvironment(macCatalyst)
        true
        #else
        false
        #endif
    }

    func testShellLandscapeShots() throws {
        let env = ProcessInfo.processInfo.environment
        guard let dir = env["TAISCE_SHOTS"], UIDevice.current.userInterfaceIdiom == .pad, !Self.onMac else {
            throw XCTSkip("TEST_RUNNER_TAISCE_SHOTS not set, or not an iPad")
        }
        let url = env["TAISCE_UI_URL"] ?? "http://127.0.0.1:7515"
        let suffix = env["TAISCE_SHOTS_SUFFIX"] ?? "light"
        XCUIDevice.shared.orientation = .landscapeLeft
        let shots: [(String, [String])] = [
            ("today", ["-tab", "today"]),
            ("todos", ["-tab", "todos"]),
            ("review", ["-tab", "review"]),
            ("search", ["-tab", "search", "-searchQuery", "review gate"]),
            ("settings", ["-tab", "today", "-showSettings", "YES"]),
        ]
        for (name, args) in shots {
            let app = XCUIApplication()
            app.launchArguments = ["-serverURL", url] + args
            app.launch()
            RunLoop.current.run(until: Date().addingTimeInterval(8))
            // the screenshot's image carries the rotation as a flag: draw it upright
            let image = XCUIScreen.main.screenshot().image
            let format = UIGraphicsImageRendererFormat()
            format.scale = image.scale
            let upright = UIGraphicsImageRenderer(size: image.size, format: format).image { _ in image.draw(at: .zero) }
            guard let png = upright.pngData() else { throw XCTSkip("no PNG") }
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("ipad-\(name)-\(suffix).png"))
            app.terminate()
        }
    }
}
