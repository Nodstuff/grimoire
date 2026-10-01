import Foundation
import Synchronization
import Testing
@testable import TaisceKit

@Suite struct SyncEngineTests {
    static let treeJSON = "[\(Fixture.summary("d1", title: "One", epoch: 1)),\(Fixture.summary("todo", title: "To-do", epoch: 1)),\(Fixture.summary("d3", title: "Three"))]"

    static func docJSON(_ id: String, title: String = "One", epoch: Int) -> String {
        Fixture.docTree(id, title: title, epoch: epoch, roots: """
        {"block":\(Fixture.block("\(id)-b", doc: id, content: "## 2026-10-01\n\n- [ ] task · due 2026-10-01 10:00", epoch: epoch)),"children":[]}
        """)
    }

    static func page(seq: Int, more: Bool, _ changes: String...) -> MockServer.Reply {
        .json(#"{"seq":\#(seq),"changes":[\#(changes.joined(separator: ","))],"more":\#(more)}"#)
    }

    func engine(_ server: MockServer, cache: Cache, sleeps: Sleeps = Sleeps()) -> SyncEngine {
        SyncEngine(
            api: server.client(), cache: cache,
            backoff: Backoff(base: .seconds(1), cap: .seconds(30), jitter: { 1 }),
            pageSize: 2, sleep: { try await sleeps.sleep($0) }
        )
    }

    @Test func catchUpPagesUntilMoreIsFalse() async throws {
        let cache = try Cache.inMemory()
        let server = MockServer { r in
            switch (r.path, r.query["since"]) {
            case ("/api/docs", _): .json(Self.treeJSON)
            case ("/api/changes", "0"): Self.page(seq: 2, more: true,
                Fixture.change(1, doc: "d1", kind: "doc", epoch: 2),
                Fixture.change(2, doc: "d3", kind: "tree"))
            case ("/api/changes", "2"): Self.page(seq: 3, more: false,
                Fixture.change(3, doc: "d3", kind: "deleted"))
            default: .json(#"{"error":"unexpected \#(r.path)"}"#)
            }
        }
        let sync = engine(server, cache: cache)
        try await sync.catchUp()

        let changeCalls = server.requests.filter { $0.path == "/api/changes" }
        #expect(changeCalls.map { $0.query["since"] } == ["0", "2"])
        #expect(changeCalls.allSatisfy { $0.query["limit"] == "2" })
        #expect(try await cache.lastSeq() == 3)
        // d1's body was never fetched: marked stale, not downloaded
        let d1 = try #require(try await cache.doc("d1"))
        #expect(d1.currentEpoch == 2 && d1.bodyEpoch == nil)
        #expect(!server.requests.contains { $0.path == "/api/doc/d1" })
        #expect(try await cache.doc("d3") == nil)
    }

    @Test func freshCacheStartsFromTheTreesSeqHeader() async throws {
        let cache = try Cache.inMemory()
        let server = MockServer { r in
            switch r.path {
            case "/api/docs": MockServer.Reply(chunks: [Data(Self.treeJSON.utf8)], headers: ["X-Grimoire-Seq": "40"])
            case "/api/changes": Self.page(seq: 41, more: false, Fixture.change(41, doc: "d1", kind: "doc", epoch: 2))
            default: .json(#"{"error":"unexpected"}"#)
            }
        }
        try await engine(server, cache: cache).catchUp()
        #expect(server.requests.filter { $0.path == "/api/changes" }.map { $0.query["since"] } == ["40"])
        #expect(try await cache.lastSeq() == 41)
    }

    @Test func treeChangesApplyTheRowsDocStateWithoutRefetch() async throws {
        let cache = try Cache.inMemory()
        try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(Self.docJSON("d1", epoch: 1).utf8)))
        let server = MockServer { r in .json(#"{"error":"unexpected \#(r.path)"}"#) }
        let page = #"{"seq":3,"more":false,"changes":[{"seq":2,"doc_id":"d1","kind":"tree","epoch":0,"at":"","doc":{"title":"Renamed","parent_id":null,"sort_key":"k","status":"draft","current_epoch":1,"deleted":false}},{"seq":3,"doc_id":"n1","kind":"tree","epoch":0,"at":"","doc":{"title":"New","parent_id":"d1","sort_key":"i","status":null,"current_epoch":0,"deleted":false}}]}"#
        let changes = try JSONDecoder().decode(ChangePage.self, from: Data(page.utf8)).changes
        let update = try await engine(server, cache: cache).apply(changes)
        #expect(update.treeChanged && server.requests.isEmpty)
        let d1 = try #require(try await cache.doc("d1"))
        #expect(d1.title == "Renamed" && d1.status == "draft" && d1.bodyEpoch == 1)
        #expect(try await cache.blocks(of: "d1").count == 1, "the body survives a rename")
        #expect(try await cache.doc("n1")?.parentID == "d1")

        let gone = Change(seq: 4, docID: "n1", kind: .tree, doc: Change.DocState(title: "New", deleted: true))
        _ = try await engine(server, cache: cache).apply([gone])
        #expect(try await cache.doc("n1") == nil)
    }

    @Test func applyRefetchesHeldAndTodoDocsOnly() async throws {
        let cache = try Cache.inMemory()
        try await cache.replaceTree(JSONDecoder().decode([DocSummary].self, from: Data(Self.treeJSON.utf8)))
        try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(Self.docJSON("d1", epoch: 1).utf8)))
        let server = MockServer { r in
            switch r.path {
            case "/api/doc/d1": .json(Self.docJSON("d1", epoch: 4))
            case "/api/doc/todo": .json(Self.docJSON("todo", title: "To-do", epoch: 2))
            default: .json(#"{"error":"unexpected \#(r.path)"}"#)
            }
        }
        let sync = engine(server, cache: cache)
        let update = try await sync.apply([
            Change(seq: 10, docID: "d1", kind: .doc, epoch: 3),
            Change(seq: 11, docID: "d1", kind: .doc, epoch: 4),
            Change(seq: 12, docID: "todo", kind: .doc, epoch: 2),
        ])
        #expect(update.docIDs == ["d1", "todo"] && !update.treeChanged)
        // two changes for d1 in one batch → one fetch
        #expect(server.requests.map(\.path) == ["/api/doc/d1", "/api/doc/todo"])
        #expect(try await cache.doc("d1")?.bodyEpoch == 4)
        #expect(try await cache.todos().first?.due == Due(year: 2026, month: 10, day: 1, hour: 10, minute: 0))

        // a change we already hold is skipped
        let again = try await sync.apply([Change(seq: 13, docID: "d1", kind: .doc, epoch: 4)])
        #expect(again.isEmpty && server.requests.count == 2)
    }

    @Test func editThenDeleteInOneBatchJustDeletes() async throws {
        let cache = try Cache.inMemory()
        try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(Self.docJSON("d1", epoch: 1).utf8)))
        let server = MockServer { r in r.path == "/api/docs" ? .json("[]") : .json(#"{"error":"gone"}"#) }
        let update = try await engine(server, cache: cache).apply([
            Change(seq: 1, docID: "d1", kind: .doc, epoch: 2),
            Change(seq: 2, docID: "d1", kind: .deleted),
        ])
        #expect(update.docIDs == ["d1"] && update.treeChanged)
        #expect(!server.requests.contains { $0.path == "/api/doc/d1" })
        #expect(try await cache.doc("d1") == nil)
    }

    @Test func followAppliesEventsAndAdvancesCursor() async throws {
        let cache = try Cache.inMemory()
        try await cache.setLastSeq(7)
        try await cache.replaceTree(JSONDecoder().decode([DocSummary].self, from: Data(Self.treeJSON.utf8)))
        let server = MockServer { r in
            switch r.path {
            case "/api/changes/stream": .sse(
                ": ping\n\n",
                "id: 8\nevent: change\ndata: \(Fixture.change(8, doc: "d1", kind: "doc", epoch: 3))\n\n",
                "id: 9\nevent: change\ndata: \(Fixture.change(9, doc: "d3", kind: "tree"))\n\n")
            case "/api/docs": .json(Self.treeJSON)
            default: .json(#"{"error":"unexpected"}"#)
            }
        }
        let sync = engine(server, cache: cache)
        let updates = await sync.updates()
        try await sync.follow()
        let stream = try #require(server.requests.first { $0.path == "/api/changes/stream" })
        #expect(stream.value(forHTTPHeaderField: "Last-Event-ID") == "7")
        #expect(stream.value(forHTTPHeaderField: "Accept") == "text/event-stream")
        #expect(try await cache.lastSeq() == 9)
        // the tree change refetched the tree once
        #expect(server.requests.filter { $0.path == "/api/docs" }.count == 1)
        var it = updates.makeAsyncIterator()
        #expect(await it.next()?.treeChanged == true)
    }

    @Test func dropBacksOffAndResumesFromLastSeq() async throws {
        let cache = try Cache.inMemory()
        try await cache.replaceTree(JSONDecoder().decode([DocSummary].self, from: Data(Self.treeJSON.utf8)))
        try await cache.setLastSeq(4)
        let streams = Counter()
        let server = MockServer { r in
            switch r.path {
            case "/api/changes": Self.page(seq: 4, more: false)
            case "/api/changes/stream":
                switch streams.next() {
                // first connection: a server-requested retry and one event, then the server closes
                case 0: .sse("retry: 1500\nid: 5\nevent: change\ndata: \(Fixture.change(5, doc: "d1", kind: "doc", epoch: 2))\n\n")
                // then refused twice (server down): no bytes → the backoff grows
                case 1, 2: MockServer.Reply(status: 503, chunks: [])
                default: .sse(": ping\n\n")
                }
            default: .json(Self.treeJSON)
            }
        }
        let sleeps = Sleeps(stopAfter: 4)
        let sync = engine(server, cache: cache, sleeps: sleeps)
        await sync.run()

        let resumes = server.requests.filter { $0.path == "/api/changes/stream" }.map { $0.value(forHTTPHeaderField: "Last-Event-ID") }
        #expect(resumes == ["4", "5", "5", "5"])
        // catch-up ran once; reconnects go straight to the stream
        #expect(server.requests.filter { $0.path == "/api/changes" }.count == 1)
        // retry: 1500 floors every delay; each refusal doubles the backoff
        // (2 s, 4 s), a healthy connection resets it
        #expect(sleeps.recorded == [.milliseconds(1500), .seconds(2), .seconds(4), .milliseconds(1500)])
        #expect(try await cache.lastSeq() == 5)
    }

    @Test func backoffGrowsAndCaps() {
        let b = Backoff(base: .seconds(1), cap: .seconds(30), jitter: { 1 })
        #expect((0..<7).map { b.delay(attempt: $0) } == [1, 2, 4, 8, 16, 30, 30].map { .seconds($0) })
        let half = Backoff(base: .seconds(1), cap: .seconds(30), jitter: { 0.5 })
        #expect(half.delay(attempt: 2) == .seconds(2))
    }
}

/// Records requested delays without sleeping; cancels the loop after N.
final class Sleeps: Sendable {
    private let state = Mutex<[Duration]>([])
    let stopAfter: Int

    init(stopAfter: Int = .max) { self.stopAfter = stopAfter }

    var recorded: [Duration] { state.withLock { $0 } }

    func sleep(_ d: Duration) async throws {
        let n = state.withLock { s in s.append(d); return s.count }
        if n >= stopAfter { throw CancellationError() }
    }
}
