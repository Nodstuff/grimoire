import Foundation
import Testing
import TaisceKit
@testable import Taisce

/// The Books screens' pure pieces: what the reader page says, and the
/// upload toast.
@Suite struct BooksAppTests {
    @Test func readerEventsParse() {
        #expect(ReaderEvent(["type": "loaded"]) == .loaded)
        #expect(ReaderEvent(["type": "ready", "exact": false]) == .ready(exact: false))
        let toc: [[String: Any]] = [["label": "One", "href": "a.xhtml", "depth": 0], ["href": "b.xhtml#x", "depth": 1]]
        #expect(ReaderEvent(["type": "opened", "title": "Dracula", "toc": toc])
            == .opened(title: "Dracula", toc: [.init(id: 0, label: "One", href: "a.xhtml", depth: 0), .init(id: 1, label: "Untitled", href: "b.xhtml#x", depth: 1)]))
        let place = ReaderEvent(["type": "relocate", "fraction": NSNumber(value: 0.25), "cfi": "epubcfi(/6/4)",
                                 "xpointer": "/body/DocFragment[2]/body/p[3]/text()[1].4", "chapter": "Two", "minutes": NSNumber(value: 7.5)])
        #expect(place == .relocate(.init(fraction: 0.25, cfi: "epubcfi(/6/4)", xpointer: "/body/DocFragment[2]/body/p[3]/text()[1].4",
                                          chapter: "Two", href: nil, minutes: 7.5)))
        #expect(ReaderEvent(["type": "error", "message": "gone"]) == .failed("gone"))
        // nothing the page doesn't say: no fraction, no type, unknown types
        #expect(ReaderEvent(["type": "relocate"]) == nil)
        #expect(ReaderEvent(["fraction": 1]) == nil && ReaderEvent(["type": "whatever"]) == nil && ReaderEvent("text") == nil)
    }

    @Test func theReaderNamesThisDevice() {
        #if targetEnvironment(macCatalyst)
        #expect(ReaderPlaces.deviceName == "Taisce on Mac")
        #else
        #expect(ReaderPlaces.name(for: .phone) == "Taisce on iPhone")
        #expect(ReaderPlaces.name(for: .pad) == "Taisce on iPad")
        #expect(ReaderPlaces.name(for: .mac) == "Taisce on Mac")
        #endif
        // the sidebar's e-reader times stay short
        let now = Date(timeIntervalSince1970: 1_790_000_000)
        #expect(BooksSidebarSections.since(now.addingTimeInterval(-7_200), now: now) == "2 h")
        #expect(BooksSidebarSections.since(now.addingTimeInterval(-3 * 86_400), now: now) == "3 d")
        #expect(BooksSidebarSections.since(now.addingTimeInterval(-20), now: now) == "now")
    }

    func result(_ status: String, _ name: String = "a.epub", _ message: String? = nil) throws -> BookUploadResult {
        var o: [String: Any] = ["filename": name, "status": status]
        if let message { o["message"] = message }
        return try JSONDecoder().decode(BookUploadResult.self, from: JSONSerialization.data(withJSONObject: o))
    }

    @Test func uploadSummary() throws {
        let added = BookUploadSummary.line([try result("added"), try result("added"), try result("already_here")])
        #expect(added.text == "Added 2 books · 1 already here" && added.tone == .success)
        let one = BookUploadSummary.line([try result("added"), try result("error", "x.epub", "not an EPUB")])
        #expect(one.text == "Added 1 book · x.epub: not an EPUB" && one.tone == .warning)
        let many = BookUploadSummary.line([try result("clash"), try result("error")])
        #expect(many.text == "2 not added" && many.tone == .warning)
        #expect(BookUploadSummary.line([]).text == "Nothing to add")
        #expect(BookUploadSummary.line([try result("already_here")]).tone == .neutral)
    }
}
