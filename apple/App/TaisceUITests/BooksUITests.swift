import XCTest

/// Books against a SERVER-mode scratch daemon with a library (the library is
/// a signed-in person's surface; LOCAL mode has none). Needs, through
/// `TEST_RUNNER_…` variables:
/// - `TAISCE_UI_BOOKS_URL`     the daemon (e.g. http://localhost:7586)
/// - `TAISCE_UI_SIGNIN_RELAY`  the softpasskey sign-in relay (the app's Debug seam)
/// - `TAISCE_UI_TOKEN`         a first-party access token for the test's own API calls
/// - `TAISCE_UI_KOSYNC_USER` / `TAISCE_UI_KOSYNC_KEY`  a paired e-reader's
///   KOReader sync login (the "Kindle" that moves on elsewhere)
/// The library holds real EPUBs (any; the reader test picks one by title,
/// `TAISCE_UI_READ_TITLE`, default "The Time Machine"). Skipped without them.
final class BooksUITests: XCTestCase {
    let env = ProcessInfo.processInfo.environment
    var base: String { env["TAISCE_UI_BOOKS_URL"] ?? "" }

    override func setUpWithError() throws {
        try XCTSkipIf(env["TAISCE_UI_BOOKS_URL"] == nil || env["TAISCE_UI_TOKEN"] == nil, "no books daemon configured")
        continueAfterFailure = false
    }

    // MARK: plumbing

    /// What a whole-screen coordinate is taken from: the window on the Mac
    /// (the application has no frame there), the app elsewhere.
    func screen(_ app: XCUIApplication) -> XCUIElement {
        #if targetEnvironment(macCatalyst)
        app.windows.firstMatch
        #else
        app
        #endif
    }

    /// A click on the Mac (a touch-style tap doesn't reach Catalyst controls), a tap elsewhere.
    func hit(_ e: XCUIElement) {
        #if targetEnvironment(macCatalyst)
        e.click()
        #else
        e.tap()
        #endif
    }

    func hit(at c: XCUICoordinate) {
        #if targetEnvironment(macCatalyst)
        c.click()
        #else
        c.tap()
        #endif
    }

    func launch(_ args: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-serverURL", base] + args
        app.launchEnvironment["TAISCE_UI_TEST"] = "1"
        if let relay = env["TAISCE_UI_SIGNIN_RELAY"] { app.launchEnvironment["TAISCE_UI_SIGNIN_RELAY"] = relay }
        app.launch()
        let signIn = app.buttons["Sign in with passkey"]
        if signIn.waitForExistence(timeout: 6) { hit(signIn) }
        return app
    }

    func http(_ method: String, _ path: String, body: [String: Any]? = nil, headers: [String: String]? = nil) throws -> (Int, [String: Any]) {
        var r = URLRequest(url: URL(string: base + path)!)
        r.httpMethod = method
        r.timeoutInterval = 15
        for (k, v) in headers ?? ["Authorization": "Bearer \(env["TAISCE_UI_TOKEN"] ?? "")"] { r.setValue(v, forHTTPHeaderField: k) }
        if let body {
            r.setValue("application/json", forHTTPHeaderField: "Content-Type")
            r.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let done = expectation(description: path)
        var out: (Int, Data?) = (0, nil)
        URLSession.shared.dataTask(with: r) { d, resp, _ in
            out = ((resp as? HTTPURLResponse)?.statusCode ?? 0, d)
            done.fulfill()
        }.resume()
        wait(for: [done], timeout: 20)
        let json = (out.1.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any]) ?? [:]
        return (out.0, json)
    }

    func bookID(_ title: String) throws -> String {
        let q = title.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? title
        let (_, page) = try http("GET", "/api/books?search=\(q)&limit=20")
        let books = page["books"] as? [[String: Any]] ?? []
        let hit = books.first { ($0["title"] as? String) == title } ?? books.first
        return try XCTUnwrap(hit?["id"] as? String, "no book titled \(title)")
    }

    func position(_ id: String) throws -> [String: Any]? {
        try http("GET", "/api/books/\(id)/position").1["position"] as? [String: Any]
    }

    /// Another device (the paired "Kindle") moves on, through KOReader sync.
    func kindleMoves(_ id: String, to fraction: Double, xpointer: String) throws {
        let doc = try XCTUnwrap(try http("GET", "/api/books/\(id)/position").1["document"] as? String)
        let (status, _) = try http("PUT", "/kosync/syncs/progress",
                                   body: ["document": doc, "progress": xpointer, "percentage": fraction, "device": "Kindle", "device_id": "dev-kindle"],
                                   headers: ["x-auth-user": env["TAISCE_UI_KOSYNC_USER"] ?? "", "x-auth-key": env["TAISCE_UI_KOSYNC_KEY"] ?? "",
                                             "Accept": "application/vnd.koreader.v1+json", "Content-Type": "application/json"])
        XCTAssertEqual(status, 200, "the Kindle's push")
    }

    func shot(_ app: XCUIApplication, _ name: String) {
        let a = XCTAttachment(screenshot: app.screenshot())
        a.name = name
        a.lifetime = .keepAlways
        add(a)
        // also straight to a folder on the Mac (TAISCE_UI_SHOTS), named by device
        if let dir = env["TAISCE_UI_SHOTS"] {
            let device = UIDevice.current.userInterfaceIdiom == .pad ? "ipad" : "iphone"
            try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
            try? app.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(device)-\(name)\(env["TAISCE_UI_SHOT_SUFFIX"] ?? "").png"))
        }
    }

    func waitFor(_ timeout: TimeInterval = 15, _ what: String, _ check: () throws -> Bool) rethrows {
        let end = Date().addingTimeInterval(timeout)
        while Date() < end {
            if try check() { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.4))
        }
        XCTFail("timed out: \(what)")
    }

    /// The reader's percent ("12%"), read off its progress line.
    func percent(_ app: XCUIApplication) -> Int? {
        let label = app.descendants(matching: .any)["reader.progress"].firstMatch.label
        return label.split(separator: "%").first.flatMap { Int($0.trimmingCharacters(in: .whitespaces)) }
    }

    func turnPage(_ app: XCUIApplication) {
        hit(at: screen(app).coordinate(withNormalizedOffset: CGVector(dx: 0.88, dy: 0.5)))
        RunLoop.current.run(until: Date().addingTimeInterval(1.2))
    }

    // MARK: tests

    /// The shelf, a book's details, and the shelf's own screenshots.
    func testShelfAndDetails() throws {
        let app = launch(["-tab", "books"])
        let summary = app.staticTexts["books.summary"]
        XCTAssertTrue(summary.waitForExistence(timeout: 30), "Books opened")
        XCTAssertTrue(summary.label.contains("books"), summary.label)
        RunLoop.current.run(until: Date().addingTimeInterval(2)) // covers
        shot(app, "books-top")
        // the first Continue reading card: its body opens the details
        let first = try XCTUnwrap(try http("GET", "/api/books/currently-reading").1["books"] as? [[String: Any]]).first?["title"] as? String ?? ""
        let card = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH %@ AND label CONTAINS %@", first, "% read")).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 10), "the \(first) Continue reading card")
        hit(at: card.coordinate(withNormalizedOffset: CGVector(dx: 0.15, dy: 0.3)))
        XCTAssertTrue(app.buttons["book.read"].waitForExistence(timeout: 10), "the details opened")
        RunLoop.current.run(until: Date().addingTimeInterval(1.5))
        shot(app, "book-detail")
        #if targetEnvironment(macCatalyst)
        // the Mac's pane: a scroll wheel, not a swipe
        app.buttons["book.read"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).scroll(byDeltaX: 0, deltaY: -400)
        #else
        app.swipeUp()
        #endif
        RunLoop.current.run(until: Date().addingTimeInterval(1))
        shot(app, "book-detail-lower")
        // Send to…: the system share sheet over the EPUB (downloaded, named Author - Title.epub)
        let send = app.buttons["Send to…"]
        if send.waitForExistence(timeout: 3), send.isHittable {
            hit(send)
            #if targetEnvironment(macCatalyst)
            // the Mac's share picker is a popover or menu of services
            let picker = app.popovers.firstMatch
            let menu = app.menus.firstMatch
            let end = Date().addingTimeInterval(15)
            while Date() < end, !picker.exists, !menu.exists { RunLoop.current.run(until: Date().addingTimeInterval(0.3)) }
            XCTAssertTrue(picker.exists || menu.exists, "the share picker opened")
            shot(app, "share-sheet")
            app.typeKey(.escape, modifierFlags: [])
            #else
            let close = app.buttons["Close"].firstMatch
            XCTAssertTrue(close.waitForExistence(timeout: 15), "the share sheet opened")
            RunLoop.current.run(until: Date().addingTimeInterval(1))
            shot(app, "share-sheet")
            hit(close)
            #endif
        }
    }

    /// Open a book, turn pages, the position lands with an XPointer; reopen
    /// and resume there; another device moving on is taken on return (and a
    /// stale write is refused with a 409 that jumps there too).
    func testReaderRoundTrip() throws {
        let title = env["TAISCE_UI_READ_TITLE"] ?? "The Time Machine"
        let id = try bookID(title)
        let before = (try position(id)?["timestamp"] as? Int) ?? 0

        var app = launch(["-readBook", id])
        let progress = app.descendants(matching: .any)["reader.progress"].firstMatch
        XCTAssertTrue(progress.waitForExistence(timeout: 40), "the book opened")
        shot(app, "reader-open")
        for _ in 0..<4 { turnPage(app) }
        let here = try XCTUnwrap(percent(app))
        shot(app, "reader-turned")

        // the write lands: our device, an XPointer, the fraction
        var row: [String: Any] = [:]
        try waitFor(20, "the position PUT") {
            row = try position(id) ?? [:]
            return ((row["timestamp"] as? Int) ?? 0) > before && (row["device_id"] as? String) != "dev-kindle"
        }
        let xp = try XCTUnwrap(row["progress"] as? String)
        XCTAssertTrue(xp.hasPrefix("/body/DocFragment["), "an XPointer: \(xp)")
        XCTAssertGreaterThan((row["percentage"] as? Double) ?? 0, 0)
        print("BOOKS-UITEST written \(row)")
        // the debounced write of where we ended up (≤ 10 s after the first)
        let firstStamp = (row["timestamp"] as? Int) ?? 0
        if (row["progress"] as? String) != nil, abs(((row["percentage"] as? Double) ?? 0) * 100 - Double(here)) > 0.5 {
            try waitFor(20, "the turned-to page written") {
                row = try position(id) ?? [:]
                return ((row["timestamp"] as? Int) ?? 0) > firstStamp && (row["progress"] as? String) != xp
            }
        }
        XCTAssertEqual(((row["percentage"] as? Double) ?? 0) * 100, Double(here), accuracy: 1, "the row is where the reader is")
        print("BOOKS-UITEST settled \(row)")

        // reopen: resumes where we were
        app.terminate()
        app = launch(["-readBook", id])
        XCTAssertTrue(app.descendants(matching: .any)["reader.progress"].firstMatch.waitForExistence(timeout: 40), "reopened")
        try waitFor(10, "resumed at \(here)%") { abs((percent(app) ?? -100) - here) <= 1 }
        shot(app, "reader-resumed")

        // the Kindle moves on while we're away: back in front, we go there
        #if targetEnvironment(macCatalyst)
        app.typeKey("h", modifierFlags: .command) // Hide: the app leaves the front
        #else
        XCUIDevice.shared.press(.home)
        #endif
        RunLoop.current.run(until: Date().addingTimeInterval(2))
        // an XPointer this copy can't resolve: the reader goes by the percentage
        let elsewhere = "/body/DocFragment[999]/body/p[1]/text()[1].0"
        let first = min(0.9, Double(here) / 100 + 0.2)
        try kindleMoves(id, to: first, xpointer: elsewhere)
        app.activate()
        let synced = app.staticTexts.containing(NSPredicate(format: "label BEGINSWITH %@", "Synced from Kindle")).firstMatch
        XCTAssertTrue(synced.waitForExistence(timeout: 15), "the return jump says where it came from")
        shot(app, "reader-synced-on-return")
        try waitFor(10, "at the Kindle's place") { abs((percent(app) ?? -100) - Int((first * 100).rounded())) <= 2 }

        // a stale write: the Kindle moves again, then we turn a page here
        RunLoop.current.run(until: Date().addingTimeInterval(11)) // past the writer's 10 s spacing and the stamp's second
        let second = min(0.97, first + 0.05)
        try kindleMoves(id, to: second, xpointer: elsewhere)
        turnPage(app)
        let label = "Kindle · \(Int((second * 100).rounded()))%"
        let refused = app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", label)).firstMatch
        XCTAssertTrue(refused.waitForExistence(timeout: 20), "the 409 hands back the Kindle's row and the reader goes there")
        shot(app, "reader-409-jump")
        let kept = try XCTUnwrap(try position(id))
        XCTAssertEqual(kept["device_id"] as? String, "dev-kindle", "our stale write did not drag the Kindle back")
        print("BOOKS-UITEST kept \(kept)")
    }

    /// The reading settings: paper, then back to Auto (the next run starts clean).
    func testReaderSettings() throws {
        let id = try bookID(env["TAISCE_UI_READ_TITLE"] ?? "The Time Machine")
        let app = launch(["-readBook", id])
        XCTAssertTrue(app.descendants(matching: .any)["reader.progress"].firstMatch.waitForExistence(timeout: 40), "the book opened")
        RunLoop.current.run(until: Date().addingTimeInterval(2.5)) // the bar steps aside
        hit(at: screen(app).coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)))
        let settings = app.buttons["reader.settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 5), "the bar came back")
        shot(app, "reader-chrome")
        hit(settings)
        let sepia = app.buttons["Sepia paper"]
        XCTAssertTrue(sepia.waitForExistence(timeout: 5))
        hit(sepia)
        RunLoop.current.run(until: Date().addingTimeInterval(1))
        shot(app, "reader-settings-sepia")
        hit(app.buttons["Done"].firstMatch)
        RunLoop.current.run(until: Date().addingTimeInterval(1.5))
        shot(app, "reader-sepia")
        hit(settings)
        hit(app.buttons["Dark paper"])
        hit(app.buttons["Done"].firstMatch)
        RunLoop.current.run(until: Date().addingTimeInterval(1.5))
        shot(app, "reader-dark")
        hit(settings)
        hit(app.buttons["Auto paper"])
        hit(app.buttons["Done"].firstMatch)
    }
}
