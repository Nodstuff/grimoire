import XCTest

/// Landscape iPad screenshots for the design comparison (the sidebar, the
/// doc and the inspector column). Skipped unless `TEST_RUNNER_TAISCE_SHOTS`
/// names a directory on the host; `TEST_RUNNER_TAISCE_UI_URL` is the daemon.
/// Each shot is `ipad-<name>.png`, taken in the current appearance.
final class DocsShotsUITests: XCTestCase {
    func testLandscapeShots() throws {
        let env = ProcessInfo.processInfo.environment
        guard let dir = env["TAISCE_SHOTS"], UIDevice.current.userInterfaceIdiom == .pad else {
            throw XCTSkip("TEST_RUNNER_TAISCE_SHOTS not set, or not an iPad")
        }
        let url = env["TAISCE_UI_URL"] ?? "http://127.0.0.1:7515"
        let suffix = env["TAISCE_SHOTS_SUFFIX"] ?? "light"
        XCUIDevice.shared.orientation = .landscapeLeft
        let shots: [(String, [String])] = [
            ("library", ["-tab", "library"]),
            ("doc", ["-openDoc", "Architecture"]),
            ("doc-inspector", ["-openDoc", "Architecture", "-showInspector", "YES"]),
            ("doc-edit", ["-openDoc", "Architecture", "-editDoc", "YES"]),
        ]
        for (name, args) in shots {
            let app = XCUIApplication()
            app.launchArguments = ["-serverURL", url] + args
            app.launch()
            RunLoop.current.run(until: Date().addingTimeInterval(8))
            // the screenshot's image carries the rotation as an orientation
            // flag; draw it upright so the PNG is landscape in every viewer
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
