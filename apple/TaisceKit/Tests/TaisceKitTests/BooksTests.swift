import Foundation
import Synchronization
import Testing
@testable import TaisceKit

/// Book JSON in the daemon's shapes (`library.rs` book_json, `kosync.rs`
/// progress_json / reading_json, `reader.rs` position_json).
enum BookFixture {
    static func book(_ id: String, title: String, author: String? = "Aaron Dembski-Bowden", series: String? = nil, index: Double? = nil,
                     added: String = "2026-09-04T10:00:00Z", hasCover: Bool = true, grey: Bool? = false, choice: String = "none") -> String {
        let a = author.map { "\"\($0)\"" } ?? "\"\""
        let s = series.map { "\"\($0)\"" } ?? "null"
        let i = index.map { String($0) } ?? "null"
        let g = grey.map { String($0) } ?? "null"
        return """
        {"id":"\(id)","title":"\(title)","author":\(a),"series":\(s),"series_index":\(i),"book_identifier":null,\
        "original_filename":"\(title).epub","size":1153433,"has_cover":\(hasCover),"has_embedded_cover":\(hasCover),\
        "has_color_cover":false,"cover_greyscale":\(g),"cover_choice":"\(choice)","cover_version":0,\
        "added_at":"\(added)","updated_at":"\(added)"}
        """
    }

    static func progress(_ pct: Double, device: String = "Kindle", at: String = "2026-10-24T08:00:00Z") -> String {
        let status = pct >= 98 ? "finished" : pct > 0 ? "reading" : "unread"
        return #"{"percentage":\#(pct),"device":"\#(device)","updated_at":"\#(at)","status":"\#(status)"}"#
    }

    static func position(_ xp: String = "/body/DocFragment[9]/body/p[6]/text()[1].42", pct: Double = 0.12, device: String = "KindlePaperWhite6", id: String = "kpw6", ts: Int64 = 1_790_000_000) -> String {
        #"{"progress":"\#(xp)","percentage":\#(pct),"device":"\#(device)","device_id":"\#(id)","timestamp":\#(ts)}"#
    }
}

@Suite struct BookAPITests {
    @Test func decodesTheLibraryPageAndPagesOn() async throws {
        let server = MockServer { r in
            if r.query["before"] == nil {
                return .json(#"{"books":[\#(BookFixture.book("b1", title: "The First Heretic", series: "The Horus Heresy", index: 14))],"next_before":"1790000000:b1"}"#)
            }
            return .json(#"{"books":[\#(BookFixture.book("b2", title: "Artemis", author: nil, grey: nil))],"next_before":null}"#)
        }
        let books = try await server.client().allBooks()
        #expect(books.map(\.id) == ["b1", "b2"])
        #expect(books[0].seriesName == "The Horus Heresy" && books[0].seriesIndex == 14)
        #expect(books[1].authorName == nil && books[1].coverGreyscale == nil)
        #expect(server.requests[0].query["limit"] == "200")
        #expect(server.requests[1].query["before"] == "1790000000:b1")
    }

    @Test func progressIsAskedInChunksAndNullsAreDropped() async throws {
        let server = MockServer { r in
            let ids = (r.query["ids"] ?? "").split(separator: ",")
            let body = ids.map { id in id == "x" ? #""x":null"# : #""\#(id)":\#(BookFixture.progress(50))"# }.joined(separator: ",")
            return .json(#"{"progress":{\#(body)}}"#)
        }
        let ids = (0..<250).map { "b\($0)" } + ["x"]
        let p = try await server.client().bookProgress(ids: ids)
        #expect(server.requests.count == 2)
        #expect(p.count == 250 && p["x"] == nil)
        #expect(p["b3"]?.status == .reading && p["b3"]?.percentage == 50)
    }

    @Test func currentlyReadingFlattensBookAndProgress() async throws {
        let row = BookFixture.book("b1", title: "The First Heretic").dropLast() + #","percentage":64.0,"device":"Kindle","status":"reading"}"#
        let server = MockServer { _ in .json(#"{"books":[\#(row)]}"#) }
        let rows = try await server.client().currentlyReading()
        #expect(rows.count == 1 && rows[0].book.title == "The First Heretic" && rows[0].progress.percentage == 64)
    }

    @Test func patchSendsOnlyWhatChangedAndNullClears() throws {
        let book = try JSONDecoder().decode(Book.self, from: Data(BookFixture.book("b1", title: "Red Tithe", author: "Robbie MacNiven", series: "Carcharodons", index: 1).utf8))
        let p = try BookPatch.edits(from: book, title: "Red Tithe", author: "Robbie MacNiven", series: "", seriesIndex: "")
        let json = String(data: try JSONEncoder().encode(p), encoding: .utf8)
        #expect(json == #"{"series":null,"series_index":null}"# || json == #"{"series_index":null,"series":null}"#)
        let same = try BookPatch.edits(from: book, title: " Red Tithe ", author: "Robbie MacNiven", series: "Carcharodons", seriesIndex: "1")
        #expect(same.isEmpty)
        #expect(throws: BookPatch.Invalid.seriesIndex) { try BookPatch.edits(from: book, title: "Red Tithe", author: "", series: "", seriesIndex: "one") }
        #expect(throws: BookPatch.Invalid.emptyTitle) { try BookPatch.edits(from: book, title: "  ", author: "", series: "", seriesIndex: "") }
        let author = try BookPatch.edits(from: book, title: "Red Tithe", author: "", series: "Carcharodons", seriesIndex: "2,5")
        #expect(author.author == "" && author.seriesIndex == .some(2.5))
    }

    @Test func updateAndDeleteUseTheirMethods() async throws {
        let server = MockServer { r in
            r.httpMethod == "DELETE" ? MockServer.Reply(status: 204, chunks: []) : .json(BookFixture.book("b1", title: "New"))
        }
        let api = server.client()
        let b = try await api.updateBook("b1", patch: BookPatch(title: "New"))
        try await api.deleteBook("b1")
        #expect(b.title == "New")
        #expect(server.requests[0].httpMethod == "PATCH" && server.requests[0].path == "/api/books/b1")
        #expect(String(data: server.requests[0].httpBody ?? Data(), encoding: .utf8) == #"{"title":"New"}"#)
        #expect(server.requests[1].httpMethod == "DELETE" && server.requests[1].path == "/api/books/b1")
    }

    @Test func positionReadsRowNoneOrUnavailable() async throws {
        let server = MockServer { r in
            switch r.path {
            case "/api/books/b1/position": .json(#"{"document":"abc","position":\#(BookFixture.position())}"#)
            case "/api/docs/d1/position": .json(#"{"document":"abc","position":null}"#)
            default: MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found"}"#.utf8)])
            }
        }
        let api = server.client()
        guard case let .available(row?) = try await api.position(.book("b1")) else { Issue.record("no row"); return }
        #expect(row.deviceID == "kpw6" && row.percentage == 0.12 && row.progress.hasPrefix("/body/DocFragment[9]"))
        #expect(try await api.position(.doc("d1")) == .available(nil))
        #expect(try await api.position(.book("local")) == .unavailable)
    }

    @Test func putPositionWritesOrHandsBackTheNewerRow() async throws {
        let server = MockServer { r in
            let body = (try? JSONSerialization.jsonObject(with: r.httpBody ?? Data())) as? [String: Any]
            if (body?["base"] as? Int) == 0 {
                return MockServer.Reply(status: 409, chunks: [Data(#"{"error":"another device has a newer position","document":"d","position":\#(BookFixture.position(ts: 1_790_000_500))}"#.utf8)])
            }
            if (body?["base"] as? Int) == 1 { return MockServer.Reply(status: 502, chunks: [Data("bad gateway".utf8)]) }
            return .json(#"{"document":"d","position":\#(BookFixture.position("/body/DocFragment[2]/body", pct: 0.3, device: "Taisce web", id: "me", ts: 1_790_000_600))}"#)
        }
        let api = server.client()
        let newer = await api.putPosition(.book("b1"), PositionWrite(progress: "/x", percentage: 0.2, deviceID: "me", base: 0))
        #expect(newer == .newer(try JSONDecoder().decode(ReaderPosition.self, from: Data(BookFixture.position(ts: 1_790_000_500).utf8))))
        #expect(await api.putPosition(.book("b1"), PositionWrite(progress: "/x", percentage: 0.2, deviceID: "me", base: 1)) == .failed)
        guard case let .written(row) = await api.putPosition(.doc("d1"), PositionWrite(progress: "/x", percentage: 0.3, deviceID: "me", base: 5)) else {
            Issue.record("not written"); return
        }
        #expect(row.timestamp == 1_790_000_600)
        let sent = try #require(server.requests.last)
        #expect(sent.httpMethod == "PUT" && sent.path == "/api/docs/d1/position")
        let json = try #require(try JSONSerialization.jsonObject(with: sent.httpBody ?? Data()) as? [String: Any])
        #expect(json["device_id"] as? String == "me" && json["base"] as? Int == 5 && json["progress"] as? String == "/x")
    }

    @Test func epubComesFromTheBookFileOrTheDocExport() async throws {
        let server = MockServer { r in MockServer.Reply(chunks: [Data("PK\u{3}\u{4}".utf8)], contentType: "application/epub+zip") }
        let api = server.client()
        #expect(try await api.epub(.book("b1")).count == 4)
        _ = try await api.epub(.doc("d1"))
        #expect(server.requests.map(\.path) == ["/api/books/b1/file", "/api/docs/d1/epub"])
    }

    @Test func coverAsksForTheVersionAndTheEmbeddedOne() async throws {
        let server = MockServer { _ in MockServer.Reply(chunks: [Data([0xFF, 0xD8, 0xFF])], contentType: "image/jpeg") }
        let api = server.client()
        #expect(try await api.coverImage("b1", version: 7, embedded: true).count == 3)
        #expect(server.requests[0].query == ["which": "embedded", "v": "7"])
        let missing = MockServer { _ in MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found"}"#.utf8)]) }
        await #expect(throws: APIError.self) { _ = try await missing.client().coverImage("b1", version: nil, embedded: false) }
    }

    @Test func redirectsToAnotherHostDropTheBearer() async throws {
        let coverURL = try #require(URL(string: "https://taisce.example/api/books/b1/cover"))
        var original = URLRequest(url: coverURL)
        original.setValue("Bearer t", forHTTPHeaderField: "Authorization")
        let task = URLSession(configuration: .ephemeral).dataTask(with: original)
        let d = StripAuthOnRedirect()
        let resp = try #require(HTTPURLResponse(url: coverURL, statusCode: 302, httpVersion: nil, headerFields: nil))
        var s3 = original
        s3.url = URL(string: "https://bucket.s3.eu-west-1.amazonaws.com/library/x.jpg?X-Amz-Signature=abc")
        let next = await d.urlSession(.shared, task: task, willPerformHTTPRedirection: resp, newRequest: s3)
        #expect(next?.value(forHTTPHeaderField: "Authorization") == nil)
        var same = original
        same.url = URL(string: "https://taisce.example/api/books/b1/cover?v=2")
        let kept = await d.urlSession(.shared, task: task, willPerformHTTPRedirection: resp, newRequest: same)
        #expect(kept?.value(forHTTPHeaderField: "Authorization") == "Bearer t")
    }

    @Test func coverCandidatesFindWithTheBookOrDifferentTerms() async throws {
        let answer = #"{"book":\#(BookFixture.book("b1", title: "Red Tithe (Carcharodons Book 1)")),"looked_up_at":"2026-10-20T10:00:00Z","candidates":[{"id":"c1","source":"search","width":400,"height":600,"colorfulness":31.5,"url":"/api/books/b1/cover-candidates/c1"}],"unavailable":false}"#
        let server = MockServer { r in r.path.hasSuffix("/c1") ? MockServer.Reply(chunks: [Data([1, 2])], contentType: "image/jpeg") : .json(answer) }
        let api = server.client()
        let found = try await api.findCoverCandidates("b1", refresh: false, title: "Red Tithe", author: " Robbie MacNiven ")
        #expect(found.candidates.first?.sourceLabel == "Title match")
        let parsed = try JSONSerialization.jsonObject(with: server.requests[0].httpBody ?? Data())
        let body = try #require(parsed as? [String: Any])
        #expect(body["title"] as? String == "Red Tithe" && body["author"] as? String == "Robbie MacNiven" && body["refresh"] as? Bool == true)
        _ = try await api.findCoverCandidates("b1", refresh: false, title: nil, author: nil)
        #expect(String(data: server.requests[1].httpBody ?? Data(), encoding: .utf8) == #"{"refresh":false}"#)
        #expect(try await api.candidateImage(found.candidates[0].url).count == 2)
        await #expect(throws: APIError.self) { _ = try await api.candidateImage("https://covers.openlibrary.org/b/id/1-L.jpg") }
    }

    @Test func chooseCoverSendsCandidateOrEmbedded() async throws {
        let server = MockServer { _ in .json(BookFixture.book("b1", title: "X", choice: "color")) }
        let api = server.client()
        _ = try await api.chooseCover("b1", candidate: "c1", unlessChosen: false)
        _ = try await api.chooseCover("b1", candidate: nil, unlessChosen: false)
        let a = try #require(try JSONSerialization.jsonObject(with: server.requests[0].httpBody ?? Data()) as? [String: Any])
        let b = try #require(try JSONSerialization.jsonObject(with: server.requests[1].httpBody ?? Data()) as? [String: Any])
        #expect(a["candidate"] as? String == "c1" && a["use"] == nil)
        #expect(b["use"] as? String == "embedded" && b["candidate"] == nil)
    }

    @Test func uploadIsMultipartWithOneFilesFieldEach() async throws {
        let server = MockServer { _ in .json(#"{"results":[{"filename":"a.epub","status":"added"},{"filename":"b.epub","status":"already_here"}]}"#) }
        let results = try await server.client().uploadBooks([("a.epub", Data("A".utf8)), ("b.epub", Data("B".utf8))])
        #expect(results.map(\.status) == [.added, .alreadyHere])
        let r = try #require(server.requests.first)
        #expect(r.value(forHTTPHeaderField: "Content-Type")?.hasPrefix("multipart/form-data; boundary=") == true)
        let body = String(decoding: r.httpBody ?? Data(), as: UTF8.self)
        #expect(body.components(separatedBy: #"name="files"; filename="#).count == 3)
    }

    @Test func ereadersDecode() async throws {
        let server = MockServer { _ in .json(#"{"ereaders":[{"id":"e1","label":"Kindle","stream_downloads":false,"created_at":"2026-09-01T00:00:00Z","last_used_at":null,"revoked_at":null},{"id":"e2","label":"Old","stream_downloads":false,"created_at":"x","last_used_at":null,"revoked_at":"2026-09-02T00:00:00Z"}]}"#) }
        let list = try await server.client().ereaders()
        #expect(list.filter(\.isActive).map(\.label) == ["Kindle"])
    }
}

@Suite struct ReaderSyncTests {
    let kindle = ReaderPosition(progress: "/body/DocFragment[9]/body/p[6]/text()[1].42", percentage: 0.12, device: "KindlePaperWhite6", deviceID: "kpw6", timestamp: 200)

    @Test func opensAtAnotherDevicesNewerPlace() {
        let local = LocalPlace(cfi: "epubcfi(/6/4!/4/2)", fraction: 0.1, timestamp: 100)
        #expect(ReaderSync.startTarget(remote: kindle, local: local, mine: "me") == .xpointer(kindle.progress, fraction: 0.12, from: kindle))
        // ours, older or equal: our finer CFI
        #expect(ReaderSync.startTarget(remote: kindle, local: LocalPlace(cfi: "c", fraction: 0.2, timestamp: 200), mine: "me") == .cfi("c", fraction: 0.2))
        var mine = kindle
        mine.deviceID = "me"
        #expect(ReaderSync.startTarget(remote: mine, local: local, mine: "me") == .cfi(local.cfi, fraction: 0.1))
        // our own row, but nothing local (another install of ours): the row
        #expect(ReaderSync.startTarget(remote: mine, local: nil, mine: "me") == .xpointer(mine.progress, fraction: 0.12, from: mine))
        #expect(ReaderSync.startTarget(remote: nil, local: nil, mine: "me") == .start)
    }

    @Test func toastOnlyForAnotherDevice() {
        #expect(ReaderSync.syncToast(kindle, mine: "me") == "Synced from KindlePaperWhite6 · 12%")
        #expect(ReaderSync.syncToast(kindle, mine: "kpw6") == nil)
        var blank = kindle
        blank.device = ""
        #expect(ReaderSync.syncToast(blank, mine: "me") == "Synced from another device · 12%")
    }

    @Test func newerMeansAnotherDeviceAfterWhatWeSaw() {
        #expect(ReaderSync.isNewer(kindle, seen: 100, mine: "me"))
        #expect(!ReaderSync.isNewer(kindle, seen: 200, mine: "me"))
        #expect(!ReaderSync.isNewer(kindle, seen: 0, mine: "kpw6"))
        #expect(!ReaderSync.isNewer(nil, seen: 0, mine: "me"))
    }
}

@Suite struct PositionWriterTests {
    /// Records each PUT and answers from a script.
    final class Server: Sendable {
        let log = Mutex<[PositionWrite]>([])
        let answers = Mutex<[PositionWriteResult]>([])
        let newer = Mutex<[ReaderPosition]>([])

        func put(_ w: PositionWrite) -> PositionWriteResult {
            log.withLock { $0.append(w) }
            return answers.withLock { $0.isEmpty ? .written(ReaderPosition(progress: w.progress, percentage: w.percentage, device: "Taisce", deviceID: w.deviceID, timestamp: 500)) : $0.removeFirst() }
        }
    }

    func writer(_ s: Server, seen: Int64 = 100) -> PositionWriter {
        PositionWriter(device: "me", seen: seen, minInterval: .seconds(10), sleep: { _ in try await Task.sleep(for: .seconds(3600)) },
                       put: { s.put($0) }, onNewer: { p in s.newer.withLock { $0.append(p) } })
    }

    @Test func firstMoveWritesWithTheSeenStampThenWaits() async throws {
        let s = Server()
        let w = writer(s)
        await w.moved(ReaderHere(progress: "/a", percentage: 0.1))
        try await Task.sleep(for: .milliseconds(50))
        #expect(s.log.withLock { $0 }.map(\.base) == [100])
        // within the interval: held until a flush
        await w.moved(ReaderHere(progress: "/b", percentage: 0.2))
        try await Task.sleep(for: .milliseconds(50))
        #expect(s.log.withLock { $0.count } == 1)
        #expect(await w.hasPending)
        await w.flush()
        let log = s.log.withLock { $0 }
        #expect(log.map(\.progress) == ["/a", "/b"] && log[1].base == 500)
    }

    @Test func aRefusalTakesTheNewerRowAndRaisesTheBase() async throws {
        let s = Server()
        let row = ReaderPosition(progress: "/k", percentage: 0.5, device: "Kindle", deviceID: "kpw6", timestamp: 900)
        s.answers.withLock { $0 = [.newer(row)] }
        let w = writer(s)
        await w.moved(ReaderHere(progress: "/a", percentage: 0.1))
        await w.flush()
        #expect(s.newer.withLock { $0 } == [row])
        #expect(await w.base == 900)
    }

    @Test func aFailedWriteIsRetriedAndTheSamePlaceIsNotRewritten() async throws {
        let s = Server()
        s.answers.withLock { $0 = [.failed] }
        let w = writer(s)
        await w.moved(ReaderHere(progress: "/a", percentage: 0.1))
        await w.flush()
        #expect(await w.hasPending)
        await w.flush()
        #expect(s.log.withLock { $0.count } == 2)
        await w.moved(ReaderHere(progress: "/a", percentage: 0.1))
        await w.flush()
        #expect(s.log.withLock { $0.count } == 2)
    }

    @Test func discardAndStopDropThePendingPlace() async throws {
        let s = Server()
        let w = writer(s)
        await w.moved(ReaderHere(progress: "/a", percentage: 0.1))
        await w.flush()
        await w.moved(ReaderHere(progress: "/b", percentage: 0.2))
        await w.discard()
        await w.flush()
        await w.stop()
        await w.moved(ReaderHere(progress: "/c", percentage: 0.3))
        await w.flush()
        #expect(s.log.withLock { $0 }.map(\.progress) == ["/a"])
    }
}

@Suite struct BookShelfTests {
    let now = try! Date("2026-10-24T12:00:00Z", strategy: .iso8601)
    var cal: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC")!
        return c
    }

    func book(_ id: String, _ title: String, series: String? = nil, index: Double? = nil, added: String = "2026-06-01T00:00:00Z", grey: Bool? = false) -> Book {
        Book(id: id, title: title, author: "A", series: series, seriesIndex: index, hasCover: true, coverGreyscale: grey, coverChoice: "none", added: added)
    }

    var shelf: BookShelf {
        BookShelf(
            books: [
                book("1", "False Gods", series: "The Horus Heresy", index: 2),
                book("2", "Horus Rising", series: "The Horus Heresy", index: 1),
                book("3", "Foundation", series: "Foundation", index: 1, grey: true),
                book("4", "Artemis", added: "2026-10-23T09:00:00Z"),
                book("5", "Red Tithe", series: "Carcharodons", index: 1),
            ],
            progress: [
                "1": BookProgress(percentage: 100, device: "Kindle", updated: "2026-08-12T10:00:00Z", status: .finished),
                "5": BookProgress(percentage: 12.4, device: "Xteink X4", updated: "2026-10-20T10:00:00Z", status: .reading),
            ],
            ereaderCount: 2)
    }

    @Test func summaryCountsBooksReadingAndEreaders() {
        #expect(shelf.summary == "5 books · 1 reading · on 2 e-readers")
        #expect(BookShelf(books: [book("1", "A")]).summary == "1 book")
    }

    @Test func gridIsBySeriesThenNumberThenTitle() {
        #expect(shelf.shown(.all, now: now, calendar: cal).map(\.title) == ["Artemis", "Red Tithe", "Foundation", "Horus Rising", "False Gods"])
        #expect(shelf.shown(.series("The Horus Heresy")).map(\.title) == ["Horus Rising", "False Gods"])
        #expect(shelf.shown(.all, query: "horus").map(\.id) == ["2", "1"])
    }

    @Test func shelvesFilterByStateMonthAndCover() {
        #expect(shelf.count(.reading) == 1 && shelf.count(.finished) == 1 && shelf.count(.unread) == 3)
        #expect(shelf.count(.addedThisMonth, now: now, calendar: cal) == 1)
        #expect(shelf.count(.blackAndWhite) == 1)
        #expect(shelf.series.map(\.name) == ["The Horus Heresy", "Carcharodons", "Foundation"])
    }

    @Test func stateLinesAndTheNewBadge() {
        #expect(shelf.stateLine(shelf.books[0], now: now, calendar: cal) == "Finished")
        #expect(shelf.stateLine(shelf.books[4], now: now, calendar: cal).hasPrefix("12% · "))
        #expect(shelf.stateLine(shelf.books[3], now: now, calendar: cal) == "Added yesterday")
        #expect(shelf.stateLine(shelf.books[2], now: now, calendar: cal) == "Unread")
        #expect(shelf.isNew(shelf.books[3], now: now) && !shelf.isNew(shelf.books[2], now: now))
    }

    @Test func continueReadingPrefersTheServersListWithTheShelfsCopy() {
        var s = shelf
        #expect(s.continueReading.map(\.book.id) == ["5"])
        var renamed = s.books[4]
        renamed.title = "Old title"
        s.reading = [ReadingBook(book: renamed, progress: s.progress["5"]!)]
        #expect(s.continueReading.first?.book.title == "Red Tithe")
    }

    @Test func labels() {
        let b = book("1", "The First Heretic", series: "The Horus Heresy", index: 14)
        #expect(BookFormat.seriesLine(b) == "The Horus Heresy · Book 14")
        #expect(BookFormat.percent(63.6) == "64%")
        #expect(BookFormat.ago(now.addingTimeInterval(-7200), now: now, calendar: cal) == "2 h ago")
        #expect(BookFormat.ago(now.addingTimeInterval(-86_400), now: now, calendar: cal) == "yesterday")
        #expect(b.downloadFilename == "A - The First Heretic.epub")
    }

    @Test func cacheRoundTrips() throws {
        let file = FileManager.default.temporaryDirectory.appending(path: "books-\(UUID().uuidString)/shelf.json")
        let cache = BookShelfCache(file: file)
        #expect(cache.load() == nil)
        var s = shelf
        s.reading = [ReadingBook(book: s.books[4], progress: s.progress["5"]!)]
        try cache.save(s, at: now)
        let back = try #require(cache.load())
        #expect(back.shelf == s && back.savedAt == now)
        cache.remove()
        #expect(cache.load() == nil)
    }
}
