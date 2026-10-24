import XCTest

/// Every screen, for the design comparison against the artboards: iPhone
/// portrait, iPad landscape (the artboards' orientation). Skipped unless
/// `TEST_RUNNER_TAISCE_SHOTS` names a directory on the host;
/// `TEST_RUNNER_TAISCE_UI_URL` is a SERVER-mode daemon with a library and
/// `TEST_RUNNER_TAISCE_UI_SIGNIN_RELAY` its sign-in relay (see BooksUITests).
/// `TEST_RUNNER_TAISCE_UI_READ_ID` is a book to open in the reader. Each shot
/// is `<iphone|ipad>-<screen>-<suffix>.png`, in the current appearance.
final class FinalShotsUITests: XCTestCase {
    let env = ProcessInfo.processInfo.environment

    func testEveryScreen() throws {
        guard let dir = env["TAISCE_SHOTS"], let url = env["TAISCE_UI_URL"] else {
            throw XCTSkip("TEST_RUNNER_TAISCE_SHOTS / TAISCE_UI_URL not set")
        }
        #if targetEnvironment(macCatalyst)
        let pad = true
        let device = "mac"
        #else
        let pad = UIDevice.current.userInterfaceIdiom == .pad
        let device = pad ? "ipad" : "iphone"
        if pad { XCUIDevice.shared.orientation = .landscapeLeft }
        #endif
        let suffix = env["TAISCE_SHOTS_SUFFIX"] ?? "light"
        var front: XCUIApplication?

        func launch(_ args: [String]) -> XCUIApplication {
            let app = XCUIApplication()
            // the app's own Appearance setting (the Mac's system one is the person's)
            let look = env["TAISCE_SHOTS_APPEARANCE"].map { ["-appearance", $0] } ?? []
            app.launchArguments = ["-serverURL", url] + look + args
            app.launchEnvironment["TAISCE_UI_TEST"] = "1"
            if let relay = env["TAISCE_UI_SIGNIN_RELAY"] { app.launchEnvironment["TAISCE_UI_SIGNIN_RELAY"] = relay }
            app.launch()
            front = app
            RunLoop.current.run(until: Date().addingTimeInterval(Double(env["TAISCE_SHOTS_WAIT"] ?? "7") ?? 7))
            return app
        }
        func shot(_ name: String) throws {
            #if targetEnvironment(macCatalyst)
            // the window alone, cut from the screen (an element's own
            // screenshot comes back short of the window's trailing edge)
            let screen = XCUIScreen.main.screenshot().image
            var image = screen
            if let w = front?.windows.firstMatch, w.exists, let cg = screen.cgImage {
                // the screen image's size reads in pixels with scale 2; the frame is in screen points
                let k = CGFloat(cg.width) / (screen.size.width / screen.scale)
                let r = CGRect(x: w.frame.minX * k, y: w.frame.minY * k, width: w.frame.width * k, height: w.frame.height * k).integral
                if let cut = cg.cropping(to: r) { image = UIImage(cgImage: cut, scale: screen.scale, orientation: .up) }
            }
            #else
            // the screenshot carries the rotation as a flag: draw it upright
            let image = XCUIScreen.main.screenshot().image
            #endif
            let format = UIGraphicsImageRendererFormat()
            format.scale = image.scale
            let upright = UIGraphicsImageRenderer(size: image.size, format: format).image { _ in image.draw(at: .zero) }
            guard let png = upright.pngData() else { return }
            let file = "\(device)-\(name)-\(suffix).png"
            // the Mac's test runner is sandboxed: there the shots ride the
            // result bundle (`xcrun xcresulttool export attachments`)
            let a = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
            a.name = file
            a.lifetime = .keepAlways
            add(a)
            try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(file))
        }
        let only = env["TAISCE_SHOTS_ONLY"].flatMap { $0.isEmpty ? nil : $0 }.map { Set($0.split(separator: ",").map(String.init)) }
        func wants(_ name: String) -> Bool { only?.contains(name) ?? true }

        let screens: [(String, [String])] = [
            ("today", ["-tab", "today"]),
            ("library", ["-tab", "library"]),
            ("books", ["-tab", "books"]),
            ("todos", ["-tab", "todos"]),
            ("search", ["-tab", "search", "-searchQuery", "review gate"]),
            ("search-books", ["-tab", "search", "-searchQuery", "Horus"]),
            ("review", ["-tab", "review"]),
            ("doc", ["-openDoc", "Architecture"]),
            ("doc-review", ["-openDoc", "Holidays 2027"]),
            ("inspector", ["-openDoc", "Holidays 2027", "-showInspector", "YES"]),
            ("settings", ["-tab", "today", "-showSettings", "YES"]),
        ] + (env["TAISCE_UI_READ_ID"].map { [("reader", ["-readBook", $0])] } ?? [])
        for (name, args) in screens where wants(name) {
            let app = launch(args)
            if name == "search-books" {
                // the scope picker's Books (not the tab's)
                let scoped = app.segmentedControls.buttons["Books"].firstMatch
                if scoped.waitForExistence(timeout: 3) { scoped.tap() }
                RunLoop.current.run(until: Date().addingTimeInterval(2))
            }
            try shot(name)
            if name == "today", !pad, wants("today-lower") {
                app.swipeUp()
                RunLoop.current.run(until: Date().addingTimeInterval(1.5))
                try shot("today-lower")
            }
            if name == "doc", wants("doc-lower") {
                #if targetEnvironment(macCatalyst)
                app.windows.firstMatch.scroll(byDeltaX: 0, deltaY: -900)
                #else
                app.swipeUp()
                app.swipeUp()
                #endif
                RunLoop.current.run(until: Date().addingTimeInterval(1.5))
                try shot("doc-lower")
            }
            if name == "books", !pad, wants("book-detail") {
                // the first cover in the grid opens its details
                let tile = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'book.'")).firstMatch
                if tile.waitForExistence(timeout: 5) {
                    tile.tap()
                    RunLoop.current.run(until: Date().addingTimeInterval(2))
                    try shot("book-detail")
                }
            }
            app.terminate()
        }
    }
}
