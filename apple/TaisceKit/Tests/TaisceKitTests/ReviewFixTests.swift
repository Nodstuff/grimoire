import Foundation
import Synchronization
import Testing
@testable import TaisceKit

/// Regressions from the independent review of main...076f78e; each test
/// failed before its fix.
@Suite struct SyncReviewTests {
    func engine(_ server: MockServer, cache: Cache, sleeps: Sleeps = Sleeps()) -> SyncEngine {
        SyncEngine(
            api: server.client(), cache: cache,
            backoff: Backoff(base: .seconds(1), cap: .seconds(30), jitter: { 1 }),
            pageSize: 2, sleep: { try await sleeps.sleep($0) }
        )
    }

    static let tree = "[\(Fixture.summary("d1", title: "One")),\(Fixture.summary("d2", title: "Two"))]"

    static func doc(_ id: String, epoch: Int, content: String = "body") -> String {
        Fixture.docTree(id, title: id, epoch: epoch, roots: """
        {"block":\(Fixture.block("\(id)-b", doc: id, content: content, epoch: epoch)),"children":[]}
        """)
    }

    static func page(seq: Int, more: Bool, _ changes: String...) -> MockServer.Reply {
        .json(#"{"seq":\#(seq),"changes":[\#(changes.joined(separator: ","))],"more":\#(more)}"#)
    }

    /// #1: `seq` is the head; with more=true the next page starts after the last row.
    @Test func catchUpPagesFromTheLastRowNotTheHead() async throws {
        let cache = try Cache.inMemory()
        try await cache.replaceTree(JSONDecoder().decode([DocSummary].self, from: Data(Self.tree.utf8)))
        try await cache.setLastSeq(1)
        let server = MockServer { r in
            switch r.query["since"] {
            case "1": Self.page(seq: 10, more: true, Fixture.change(2, doc: "d1", kind: "doc", epoch: 2), Fixture.change(3, doc: "d2", kind: "doc", epoch: 2))
            case "3": Self.page(seq: 10, more: true, Fixture.change(4, doc: "d1", kind: "doc", epoch: 3), Fixture.change(5, doc: "d2", kind: "doc", epoch: 3))
            case "5": Self.page(seq: 10, more: false, Fixture.change(10, doc: "d1", kind: "doc", epoch: 4))
            default: .json(#"{"error":"unexpected since=\#(r.query["since"] ?? "")"}"#)
            }
        }
        try await engine(server, cache: cache).catchUp()
        #expect(server.requests.filter { $0.path == "/api/changes" }.map { $0.query["since"] } == ["1", "3", "5"])
        #expect(try await cache.lastSeq() == 10)
        #expect(try await cache.doc("d1")?.currentEpoch == 4)
    }

    /// #3: one broken body fetch must not stall the cursor; gone docs are dropped.
    @Test func aFailedBodyFetchDoesNotStallSync() async throws {
        let cache = try Cache.inMemory()
        for id in ["d1", "d2", "d3", "d4"] {
            try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(Self.doc(id, epoch: 1).utf8)))
        }
        try await cache.setLastSeq(1)
        let server = MockServer { r in
            switch r.path {
            case "/api/changes": Self.page(seq: 5, more: false,
                Fixture.change(2, doc: "d1", kind: "doc", epoch: 2), Fixture.change(3, doc: "d2", kind: "doc", epoch: 2),
                Fixture.change(4, doc: "d3", kind: "doc", epoch: 2), Fixture.change(5, doc: "d4", kind: "doc", epoch: 2))
            case "/api/doc/d1": MockServer.Reply(status: 500, chunks: [Data("boom".utf8)], contentType: "text/plain")
            case "/api/doc/d2": .json(Self.doc("d2", epoch: 2, content: "fresh"))
            // today's daemon: 200 + error; the coming one: a real 404
            case "/api/doc/d3": .json(#"{"error":"not found: doc d3"}"#)
            case "/api/doc/d4": MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found: doc d4"}"#.utf8)])
            default: .json(#"{"error":"unexpected"}"#)
            }
        }
        let sync = engine(server, cache: cache)
        try await sync.catchUp()
        #expect(try await cache.lastSeq() == 5)
        #expect(try await cache.blocks(of: "d2").map(\.content) == ["fresh"])
        let d1 = try #require(try await cache.doc("d1"))
        #expect(d1.isBodyStale && d1.currentEpoch == 2, "kept, stale: opening it retries")
        #expect(await sync.failedDocs.keys.sorted() == ["d1"])
        #expect(try await cache.doc("d3") == nil)
        #expect(try await cache.doc("d4") == nil)
    }

    /// #4: the same day heading twice used to collide on (date, position).
    @Test func aRepeatedDayHeadingKeepsDistinctPositions() async throws {
        let md = "## 2026-10-01\n\n- [ ] one\n\n## 2026-10-01\n\n- [ ] two\n- [x] three"
        let items = TodoParser.parse(markdown: md)
        #expect(items.map(\.position) == [0, 1, 2] && items.map(\.text) == ["one", "two", "three"])
        let cache = try Cache.inMemory()
        let tree = Fixture.docTree("todo", title: "To-do", roots: """
        {"block":\(Fixture.block("h1", doc: "todo", type: "heading", content: "## 2026-10-01")),"children":[
          {"block":\(Fixture.block("i1", doc: "todo", content: "- [ ] one", parent: "h1")),"children":[]}]},
        {"block":\(Fixture.block("h2", doc: "todo", type: "heading", content: "## 2026-10-01")),"children":[
          {"block":\(Fixture.block("i2", doc: "todo", content: "- [ ] two", parent: "h2")),"children":[]}]}
        """)
        try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(tree.utf8)))
        #expect(try await cache.todos().map(\.text) == ["one", "two"])
    }

    /// #5: first run fetches the To-do body so Today has items.
    @Test func bootstrapFetchesTheTodoBody() async throws {
        let cache = try Cache.inMemory()
        let todo = Fixture.docTree("todo", title: "To-do", epoch: 3, roots: """
        {"block":\(Fixture.block("h", doc: "todo", type: "heading", content: "## 2026-10-01")),"children":[
          {"block":\(Fixture.block("i", doc: "todo", content: "- [ ] pay rent", parent: "h")),"children":[]}]}
        """)
        let server = MockServer { r in
            switch r.path {
            case "/api/docs": MockServer.Reply(chunks: [Data("[\(Fixture.summary("todo", title: "To-do", epoch: 3))]".utf8)], headers: ["Taisce-Seq": "9"])
            case "/api/changes": Self.page(seq: 9, more: false)
            case "/api/doc/todo": .json(todo)
            default: .json(#"{"error":"unexpected"}"#)
            }
        }
        let sync = engine(server, cache: cache)
        try await sync.catchUp()
        #expect(try await cache.todos().map(\.text) == ["pay rent"])
        // the UI reloads Today from this observation
        var todos = cache.observeTodos().makeAsyncIterator()
        #expect(try await todos.next()?.map(\.text) == ["pay rent"])
        // and doesn't fetch it again while current
        try await sync.catchUp()
        #expect(server.requests.filter { $0.path == "/api/doc/todo" }.count == 1)
    }

    /// #10: a head below our cursor = the server was reset: re-bootstrap.
    @Test func aCursorAboveTheHeadReBootstraps() async throws {
        let cache = try Cache.inMemory()
        try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(Self.doc("d1", epoch: 7).utf8)))
        try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(Self.doc("old", epoch: 2).utf8)))
        try await cache.setLastSeq(500)
        let server = MockServer { r in
            switch (r.path, r.query["since"]) {
            case ("/api/changes", "500"): Self.page(seq: 3, more: false)
            case ("/api/changes", "3"): Self.page(seq: 3, more: false)
            case ("/api/docs", _): MockServer.Reply(chunks: [Data("[\(Fixture.summary("d1", title: "One", epoch: 1))]".utf8)], headers: ["Taisce-Seq": "3"])
            default: .json(#"{"error":"unexpected"}"#)
            }
        }
        try await engine(server, cache: cache).catchUp()
        #expect(try await cache.lastSeq() == 3, "the one time the cursor goes back")
        #expect(try await cache.doc("old") == nil)
        #expect(try await cache.doc("d1")?.isBodyStale == true, "bodies from the old database are refetched")
    }

    /// #11: the cursor never moves backwards except through a reset.
    @Test func theCursorIsMonotonic() async throws {
        let cache = try Cache.inMemory()
        try await cache.setLastSeq(10)
        try await cache.setLastSeq(5)
        #expect(try await cache.lastSeq() == 10)
        try await cache.resetLastSeq(5)
        #expect(try await cache.lastSeq() == 5)
    }

    /// #11: stop() returns only once the old loop is gone, so start() never
    /// runs two loops against one cursor.
    @Test func stopWaitsForTheLoop() async throws {
        let cache = try Cache.inMemory()
        try await cache.replaceTree([])
        try await cache.setLastSeq(1)
        // the stream stays open: a live loop holds exactly one connection
        let server = MockServer { r in
            r.path == "/api/changes/stream"
                ? MockServer.Reply(chunks: [Data(": ping\n\n".utf8)], contentType: "text/event-stream", hold: true)
                : Self.page(seq: 1, more: false)
        }
        let sync = SyncEngine(api: server.client(), cache: cache, backoff: Backoff(base: .milliseconds(20), cap: .milliseconds(20), jitter: { 1 }))
        await sync.start()
        try await IT.eventually(timeout: .seconds(5), "the loop connected") {
            server.requests.contains { $0.path == "/api/changes/stream" } ? true : nil
        }
        await sync.stop()
        #expect(await sync.status == .idle)
        try await Task.sleep(for: .milliseconds(300))
        // a loop still alive after stop() would reconnect once the held stream is cancelled
        #expect(server.requests.filter { $0.path == "/api/changes/stream" }.count == 1, "nothing runs after stop() returns")
        // a restart runs one loop again
        await sync.start()
        await sync.stop()
    }

    /// #13: an SSE event that doesn't decode leaves the cursor alone and
    /// hands over to /api/changes.
    @Test func anUndecodableEventFallsBackToCatchUp() async throws {
        let cache = try Cache.inMemory()
        try await cache.replaceTree(JSONDecoder().decode([DocSummary].self, from: Data(Self.tree.utf8)))
        try await cache.setLastSeq(6)
        let streams = Counter()
        let pages = Counter()
        let server = MockServer { r in
            switch r.path {
            case "/api/changes":
                // nothing new at first; the change shows up by the time we re-read
                pages.next() == 0
                    ? Self.page(seq: 6, more: false)
                    : Self.page(seq: 7, more: false, Fixture.change(7, doc: "d1", kind: "doc", epoch: 2))
            case "/api/changes/stream":
                streams.next() == 0
                    ? .sse("id: 7\nevent: change\ndata: {not json\n\n")
                    : .sse(": ping\n\n")
            default: .json(#"{"error":"unexpected"}"#)
            }
        }
        await engine(server, cache: cache, sleeps: Sleeps(stopAfter: 2)).run()
        let since = server.requests.filter { $0.path == "/api/changes" }.map { $0.query["since"] }
        #expect(since.first == "6" && since.count >= 2, "caught up again after the bad event: \(since)")
        #expect(since[1] == "6", "the bad event didn't move the cursor")
        #expect(try await cache.lastSeq() == 7)
        #expect(try await cache.doc("d1")?.currentEpoch == 2)
    }
}

@Suite struct OutboxReviewTests {
    func propose(_ base: Int, doc: String = "d1") -> ProposeRequest {
        ProposeRequest(docID: doc, baseEpoch: base, ops: [.replace(target: "b1", content: "x")])
    }

    /// #8: a busy or broken server is retried later; only real refusals fail.
    @Test(arguments: [
        (MockServer.Reply(status: 503, chunks: [Data(#"{"error":"busy"}"#.utf8)]), true),
        (MockServer.Reply(status: 502, chunks: [Data("<html>bad gateway</html>".utf8)], contentType: "text/html"), true),
        (MockServer.Reply(status: 429, chunks: []), true),
        (MockServer.Reply(status: 408, chunks: []), true),
        (MockServer.Reply(chunks: [Data("<!doctype html>".utf8)], contentType: "text/html"), true),
        (MockServer.Reply(status: 400, chunks: [Data(#"{"error":"bad op"}"#.utf8)]), false),
        (MockServer.Reply(chunks: [Data(#"{"error":"bad op"}"#.utf8)]), false),
        (MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found: doc d1"}"#.utf8)]), false),
    ])
    func transientFailuresStayQueued(reply: MockServer.Reply, retried: Bool) async throws {
        let cache = try Cache.inMemory()
        try await cache.enqueue(propose(5))
        try await cache.enqueue(propose(6))
        let server = MockServer { _ in reply }
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        let left = try await cache.pendingOutbox()
        if retried {
            #expect(server.requests.count == 1 && left.count == 2, "kept in order for the next replay")
        } else {
            #expect(server.requests.count == 2 && left.isEmpty, "refused: failed, the queue moves on")
        }
    }

    /// #9, per block: a queued edit of b1 is sent on the epoch our last
    /// applied write to b1 landed at, even when others wrote other blocks
    /// in between (the landing epoch jumped); a red (unapplied) op doesn't count.
    @Test func rebaseFollowsOurLastWriteToTheBlock() async throws {
        let cache = try Cache.inMemory()
        try await cache.enqueue(propose(5))
        try await cache.enqueue(propose(5))
        try await cache.enqueue(propose(5))
        let epochs = Counter()
        let server = MockServer { _ in
            let n = epochs.next()
            // lands at 6, then at 9 (others wrote in between), then parked red
            let (epoch, applied, verdict) = [(6, true, "green"), (9, true, "green"), (9, false, "red")][min(n, 2)]
            return .json(#"{"doc_id":"d1","epoch":\#(epoch),"verdicts":[{"op_id":"o\#(n)","block_id":"b1","verdict":"\#(verdict)","confidence":1,"applied":\#(applied),"note":""}]}"#)
        }
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        try await cache.enqueue(propose(5))
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        let bases = server.requests.compactMap { r -> Int? in
            (try? JSONSerialization.jsonObject(with: r.httpBody ?? Data()) as? [String: Any])?["base_epoch"] as? Int
        }
        #expect(bases == [5, 6, 9, 9])
    }
}

@Suite struct AuthReviewTests {
    /// The Keychain write fails once (device locked, say) during a refresh.
    final class FlakyStore: TokenStore {
        let inner: MemoryTokenStore
        let failNextTokenWrite = Mutex(false)
        init(_ inner: MemoryTokenStore) { self.inner = inner }
        func tokens(for server: String) throws -> TokenSet? { try inner.tokens(for: server) }
        func setTokens(_ tokens: TokenSet?, for server: String) throws {
            if failNextTokenWrite.withLock({ v in defer { v = false }; return v }) { throw CocoaError(.fileWriteUnknown) }
            try inner.setTokens(tokens, for: server)
        }
        func clientID(for server: String) throws -> String? { try inner.clientID(for: server) }
        func setClientID(_ id: String?, for server: String) throws { try inner.setClientID(id, for: server) }
    }

    /// #6: the rotated tokens live in memory even if the Keychain write
    /// fails, and the refresh runs inside the app's background-task shield.
    @Test func aRotationSurvivesAFailedKeychainWrite() async throws {
        let fake = FakeAuthServer()
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let store = FlakyStore(MemoryTokenStore(
            tokens: [FakeAuthServer.base: TokenSet(accessToken: "a0", refreshToken: "r0", expiresAt: now.addingTimeInterval(60))],
            clients: [FakeAuthServer.base: "dcr_0"]
        ))
        store.failNextTokenWrite.withLock { $0 = true }
        let shielded = Mutex<[String]>([])
        let auth = AuthSession(oauth: fake.oauth(), store: store, now: { now }, shield: { name in
            shielded.withLock { $0.append("begin \(name)") }
            return { shielded.withLock { $0.append("end") } }
        })
        let first = try await auth.token()
        #expect(first == "a1", "the refresh succeeded despite the Keychain")
        // the next call uses the rotated tokens from memory: no second
        // refresh with the spent r0 (which would revoke the grant)
        #expect(try await auth.token() == "a1")
        #expect(fake.snapshot.tokenForms.count == 1)
        try await IT.eventually(timeout: .seconds(2), "shield ended") {
            shielded.withLock { $0 }.count == 2 ? true : nil
        }
        #expect(shielded.withLock { $0 } == ["begin taisce.token-refresh", "end"])
    }

    /// #7: only a daemon's JSON 404, or HTML from loopback / plain http, means
    /// "no auth". A proxy error or captive portal over https is an error.
    @Test(arguments: [
        ("https://taisce.example", MockServer.Reply(chunks: [Data("<html>portal</html>".utf8)], contentType: "text/html"), "throws"),
        ("https://taisce.example", MockServer.Reply(status: 502, chunks: [Data("<html>bad gateway</html>".utf8)], contentType: "text/html"), "throws"),
        ("https://taisce.example", MockServer.Reply(status: 503, chunks: [Data("{}".utf8)]), "throws"),
        ("https://taisce.example", MockServer.Reply(status: 404, chunks: [Data("not here".utf8)], contentType: "text/plain"), "throws"),
        ("https://taisce.example", MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found"}"#.utf8)]), "local"),
        ("http://127.0.0.1:7425", MockServer.Reply(chunks: [Data("<!doctype html>".utf8)], contentType: "text/html"), "local"),
        ("http://192.168.1.20:7425", MockServer.Reply(chunks: [Data("<!doctype html>".utf8)], contentType: "text/html"), "local"),
    ])
    func discoveryOnlyDowngradesWhenItIsSafe(base: String, reply: MockServer.Reply, expect: String) async throws {
        let server = MockServer { _ in reply }
        let oauth = OAuthClient(baseURL: try #require(URL(string: base)), session: server.session)
        if expect == "throws" {
            await #expect(throws: (any Error).self) { try await oauth.discover() }
        } else {
            #expect(try await oauth.discover() == nil)
        }
    }

    /// #14: a server URL typed without a scheme.
    @Test func serverURLsAreNormalised() {
        func n(_ s: String) -> String? { ServerConfig.normalizedURL(s)?.absoluteString }
        #expect(n("taisce.null.ie") == "https://taisce.null.ie")
        #expect(n("  https://taisce.null.ie/ ") == "https://taisce.null.ie")
        #expect(n("127.0.0.1:7425") == "http://127.0.0.1:7425")
        #expect(n("localhost:7425") == "http://localhost:7425")
        #expect(n("http://192.168.1.5:7425") == "http://192.168.1.5:7425")
        #expect(n("") == nil)
        #expect(n("ftp://x") == nil)
        #expect(n("https://") == nil)
        #expect(n("[::1]:7425") == "http://[::1]:7425")
    }

    /// The app never names a principal: in SERVER mode identity is the token.
    @Test func requestsNeverCarryAPrincipalHeader() async throws {
        let server = MockServer { _ in .json("[]") }
        _ = try await server.client().tree()
        _ = try? await server.client().propose(ProposeRequest(docID: "d", baseEpoch: 0, ops: []))
        #expect(!server.requests.isEmpty)
        #expect(server.requests.allSatisfy { $0.value(forHTTPHeaderField: "Taisce-Principal") == nil })
    }
}
