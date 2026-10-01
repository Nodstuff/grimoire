import Foundation
import Testing
@testable import TaisceKit

/// Saving: coalescing, the debounce, chained bases, the offline queue
/// across a relaunch, and what the gate's verdicts leave behind.
@Suite(.timeLimit(.minutes(1))) struct EditorSaveTests {
    func seed(_ cache: Cache) async throws {
        func b(_ id: String, _ parent: String?, _ key: String, _ type: BlockType, _ content: String) -> Block {
            Block(id: id, docID: "d1", parentID: parent, orderKey: key, blockType: type, content: content, epoch: 5)
        }
        try await cache.storeDoc(DocTree(doc: DocSummary(id: "d1", parentID: nil, title: "Doc", currentEpoch: 5), roots: [
            BlockNode(block: b("h1", nil, "i", .heading, "# One"), children: [
                BlockNode(block: b("p1", "h1", "i", .paragraph, "alpha")),
                BlockNode(block: b("p2", "h1", "r", .paragraph, "bravo")),
            ]),
        ]))
    }

    func replace(_ block: BlockID, _ text: String, base: Int = 5) -> ProposeRequest {
        ProposeRequest(docID: "d1", baseEpoch: base, ops: [.replace(target: block, content: text)], requestID: UUID().uuidString.lowercased())
    }

    func bodies(_ cache: Cache) async throws -> [ProposeRequest] {
        try await cache.pendingOutbox().map { try JSONDecoder().decode(ProposeRequest.self, from: $0.body ?? Data()) }
    }

    @Test func typingInOneBlockCoalescesIntoOneWrite() async throws {
        let cache = try Cache.inMemory()
        try await cache.enqueueCoalescing(replace("p1", "a"))
        try await cache.enqueueCoalescing(replace("p1", "al"))
        try await cache.enqueueCoalescing(replace("p1", "alp"))
        #expect(try await bodies(cache).map { $0.ops.map(\.kind) } == [[.replace(target: "p1", content: "alp")]])
        // another block, then back: three rows, order kept
        try await cache.enqueueCoalescing(replace("p2", "b"))
        try await cache.enqueueCoalescing(replace("p1", "alpha"))
        #expect(try await bodies(cache).map { $0.ops.first?.kind.target } == ["p1", "p2", "p1"])
        // a structure op is never folded into, nor folds anything
        try await cache.enqueue(ProposeRequest(docID: "d1", baseEpoch: 5, ops: [.delete(target: "p2")]))
        try await cache.enqueueCoalescing(replace("p1", "alpha!"))
        #expect(try await cache.pendingOutbox().count == 5)
    }

    @Test func aRowAlreadyTriedIsNeverRewritten() async throws {
        let cache = try Cache.inMemory()
        let first = try await cache.enqueueCoalescing(replace("p1", "one"))
        // sent once, answer lost: it may have landed under its request_id
        _ = try await cache.claimOutbox(try #require(first.id))
        try await cache.markOutbox(try #require(first.id), state: .pending, error: "timed out")
        try await cache.enqueueCoalescing(replace("p1", "two"))
        #expect(try await bodies(cache).map { $0.ops.first?.kind } == [.replace(target: "p1", content: "one"), .replace(target: "p1", content: "two")])
        // in flight: same
        let cache2 = try Cache.inMemory()
        let row = try await cache2.enqueueCoalescing(replace("p1", "x"))
        _ = try await cache2.claimOutbox(try #require(row.id))
        try await cache2.enqueueCoalescing(replace("p1", "xy"))
        #expect(try await cache2.pendingOutbox().count == 2)
    }

    @Test @MainActor func theSchedulerDebouncesPerBlockAndFlushes() async throws {
        let saved = Locked<[Set<String>]>([])
        let scheduler = SaveScheduler<String>(delay: .milliseconds(80)) { keys in saved.mutate { $0.append(keys) } }
        for _ in 0..<5 {
            scheduler.touch("a")
            try await Task.sleep(for: .milliseconds(10))
        }
        scheduler.touch("b")
        #expect(saved.value.isEmpty, "still typing")
        try await Self.waitUntil { saved.value.count == 2 }
        #expect(Set(saved.value) == [["a"], ["b"]], "one save per block once it went quiet")
        scheduler.touch("c")
        scheduler.touch("d")
        await scheduler.flush()
        #expect(saved.value.last == ["c", "d"], "flush saves everything waiting at once")
        try await Task.sleep(for: .milliseconds(150))
        #expect(saved.value.count == 3, "and the timers it replaced never fire")
    }

    static func waitUntil(_ ok: @escaping @Sendable () -> Bool) async throws {
        for _ in 0..<200 where !ok() { try await Task.sleep(for: .milliseconds(10)) }
        #expect(ok())
    }

    @Test func offlineEditsSurviveARelaunch() async throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "taisce-editor-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let path = dir.appending(path: "cache.sqlite").path(percentEncoded: false)
        do {
            let cache = try Cache(path: path)
            try await seed(cache)
            var session = EditorSession(editor: try #require(try await cache.editor(for: "d1")))
            session.update("p1", content: .paragraph(AttributedString("alpha, offline")))
            for r in session.commitText() { try await cache.enqueueCoalescing(r) }
            let changeResult = session.returnKey("p1", caret: EditorCaret(offset: 14))
            let change = try #require(changeResult)
            try await cache.enqueue(try #require(change.request))
            session.update(change.focus, content: .paragraph(AttributedString("a new block, offline")))
            for r in session.commitText() { try await cache.enqueueCoalescing(r) }
        }
        // the app relaunches: a new cache on the same file
        let cache = try Cache(path: path)
        #expect(try await cache.pendingOutbox().count == 3)
        let restored = EditorSession(editor: try #require(try await cache.editor(for: "d1")))
        #expect(restored.items.map(\.content.markdown) == ["# One", "alpha, offline", "a new block, offline", "bravo"])
        var again = restored
        #expect(again.commitText().isEmpty, "nothing to resend: the queue already holds it")
    }

    func green(_ epoch: Int, _ block: String) -> MockServer.Reply {
        .json(#"{"doc_id":"d1","epoch":\#(epoch),"verdicts":[{"op_id":"o\#(epoch)","block_id":"\#(block)","verdict":"green","confidence":1,"applied":true,"note":""}]}"#)
    }

    func sentBases(_ s: MockServer) -> [Int] {
        s.requests.compactMap { (try? JSONSerialization.jsonObject(with: $0.httpBody ?? Data()) as? [String: Any])?["base_epoch"] as? Int }
    }

    /// (a) our replace of p1 lands at 7, an agent writes p2 at 8: our next
    /// replace of p1, made on 6, goes out on 7, so the gate sees p1 as
    /// unchanged since and applies it green.
    @Test func ourOwnWriteToABlockIsNeverAConflict() async throws {
        let cache = try Cache.inMemory()
        try await seed(cache)
        try await cache.enqueue(replace("p1", "one", base: 6))
        let first = MockServer { _ in self.green(7, "p1") }
        try await OutboxReplayer(api: first.client(), cache: cache).replay()
        try await cache.applyDocState("d1", Change.DocState(title: "Doc", currentEpoch: 8)) // the agent's write to p2
        try await cache.enqueue(replace("p1", "two", base: 6))
        let second = MockServer { _ in self.green(9, "p1") }
        try await OutboxReplayer(api: second.client(), cache: cache).replay()
        #expect(sentBases(second) == [7])
        #expect(try await cache.lastLandedEpoch("d1") == 9)
    }

    /// (b) the desktop changes p1 at 8 after our write at 7: our next
    /// replace of p1 still counts from 7, so the gate sees the desktop's
    /// change and scores it (red), never silently overwriting it. A propose
    /// touching p1 and an untouched p2 goes on the lower base.
    @Test func someoneElsesChangeToTheBlockStillConflicts() async throws {
        let cache = try Cache.inMemory()
        try await seed(cache)
        try await cache.enqueue(replace("p1", "one", base: 6))
        let first = MockServer { _ in self.green(7, "p1") }
        try await OutboxReplayer(api: first.client(), cache: cache).replay()
        try await cache.applyDocState("d1", Change.DocState(title: "Doc", currentEpoch: 8))
        try await cache.enqueue(replace("p1", "two", base: 6))
        try await cache.enqueue(ProposeRequest(docID: "d1", baseEpoch: 6, ops: [.replace(target: "p1", content: "x"), .replace(target: "p2", content: "y")]))
        let second = MockServer { _ in .json(#"{"doc_id":"d1","epoch":8,"verdicts":[]}"#) }
        try await OutboxReplayer(api: second.client(), cache: cache).replay()
        #expect(sentBases(second) == [7, 6], "never past our write to p1; p2 we never wrote")
        #expect(try await cache.adjustedBase(replace("p1", "z", base: 6)) == 7)
    }

    @Test func refusedWritesStayOnScreen() async throws {
        let cache = try Cache.inMemory()
        try await seed(cache)
        try await cache.enqueue(replace("p1", "typed, then refused"))
        let refuse = MockServer { _ in .json(#"{"error":"read-only"}"#) }
        try await OutboxReplayer(api: refuse.client(), cache: cache).replay()
        #expect(try await cache.outboxState(for: "d1").failed == 1)
        let editor = try #require(try await cache.editor(for: "d1"))
        #expect(editor.blocks["p1"]?.content == "typed, then refused")
    }

    @Test func verdictsMarkBlocksUntilReviewed() async throws {
        let cache = try Cache.inMemory()
        try await cache.enqueue(ProposeRequest(docID: "d1", baseEpoch: 5, ops: [.replace(target: "p1", content: "x"), .replace(target: "p2", content: "y")]))
        try await cache.enqueue(replace("h1", "# Renamed"))
        let n = Counter()
        let server = MockServer { _ in
            n.next() == 0
                ? .json(#"{"doc_id":"d1","epoch":5,"verdicts":[{"op_id":"r1","block_id":"p1","verdict":"red","confidence":0.2,"applied":false,"note":"stale"},{"op_id":"y1","block_id":"p2","verdict":"yellow","confidence":0.6,"applied":true,"note":""}]}"#)
                : .json(#"{"doc_id":"d1","epoch":6,"verdicts":[{"op_id":"g1","block_id":"h1","verdict":"green","confidence":1,"applied":true,"note":""}]}"#)
        }
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        let marks = try await cache.reviews(for: "d1")
        #expect(marks["p1"]?.verdict == .red && marks["p2"]?.verdict == .yellow && marks["h1"] == nil)
        #expect(try await cache.reviews(for: "other").isEmpty)
        // the desktop accepted the red one: the open queue no longer has it
        let after = try await cache.reviews(for: "d1", openOps: ["y1"])
        #expect(after["p1"] == nil && after["p2"]?.verdict == .yellow)
        // a later green for the same block clears it
        try await cache.enqueue(replace("p2", "y2"))
        let green = MockServer { _ in .json(#"{"doc_id":"d1","epoch":7,"verdicts":[{"op_id":"g2","block_id":"p2","verdict":"green","confidence":1,"applied":true,"note":""}]}"#) }
        try await OutboxReplayer(api: green.client(), cache: cache).replay()
        #expect(try await cache.reviews(for: "d1")["p2"] == nil)
    }

    /// Another doc's write must not wake an editor watching this one.
    @Test func blockObservationIgnoresOtherDocs() async throws {
        let cache = try Cache.inMemory()
        try await seed(cache)
        var it = cache.observeBlocks(of: "d1").makeAsyncIterator()
        let first = try await it.next()
        #expect(first?.count == 3)
        try await cache.storeDoc(DocTree(doc: DocSummary(id: "d2", parentID: nil, title: "Other", currentEpoch: 1), roots: [
            BlockNode(block: Block(id: "x", docID: "d2", parentID: nil, orderKey: "i", blockType: .paragraph, content: "other")),
        ]))
        try await cache.storeDoc(DocTree(doc: DocSummary(id: "d1", parentID: nil, title: "Doc", currentEpoch: 6), roots: [
            BlockNode(block: Block(id: "h1", docID: "d1", parentID: nil, orderKey: "i", blockType: .heading, content: "# One", epoch: 6)),
        ]))
        let next = try await it.next()
        #expect(next?.map(\.id) == ["h1"], "the next emission is this doc's change, not a repeat")
    }

    @Test func outboxStateForTheChip() async throws {
        let cache = try Cache.inMemory()
        try await cache.enqueue(replace("p1", "one"))
        try await cache.enqueue(replace("p2", "two"))
        let hot = MockServer { _ in .json(#"{"error":"doc is in a live session — edits go through the session; retry after it ends (P2.3)"}"#) }
        try await OutboxReplayer(api: hot.client(), cache: cache).replay()
        var s = try await cache.outboxState(for: "d1")
        #expect(s.pending == 2 && s.liveSession && s.failed == 0)
        let refuse = MockServer { _ in .json(#"{"error":"doc d1 is read-only here"}"#) }
        try await OutboxReplayer(api: refuse.client(), cache: cache).replay()
        s = try await cache.outboxState(for: "d1")
        #expect(s.failed == 2 && s.pending == 0 && s.failure?.contains("read-only") == true)
        try await cache.retryFailed(docID: "d1")
        #expect(try await cache.outboxState(for: "d1").pending == 2, "kept, never dropped: retry puts them back")
        try await cache.discardFailed(docID: "d1")
        #expect(try await cache.outboxState(for: "d1").pending == 2, "discard only drops refused rows")
    }

    @Test func newDocRetriesWithoutDoubleCreating() async throws {
        // the first create lands but its answer is lost; the retry finds it
        let n = Counter()
        let server = MockServer { r in
            if r.url?.path == "/api/docs", r.httpMethod == "POST" {
                return n.next() == 0 ? MockServer.Reply(status: 503, chunks: [Data()]) : .json(Fixture.summary("dup", title: "Notes"))
            }
            return .json("[\(Fixture.summary("old", title: "Notes")),\(Fixture.summary("made", title: "Notes", parent: "f"))]")
        }
        let doc = try await NewDoc.create(api: server.client(), title: "Notes", parent: "f", known: ["old"])
        #expect(doc.id == "made")
        let posts = server.requests.filter { $0.httpMethod == "POST" }
        #expect(posts.count == 1, "never a second create once it exists")
        let body = try #require(try JSONSerialization.jsonObject(with: posts[0].httpBody ?? Data()) as? [String: Any])
        #expect(body["title"] as? String == "Notes" && body["parent_doc_id"] as? String == "f" && body["request_id"] is String)
        // a refusal is final
        let refused = MockServer { _ in .json(#"{"error":"title must not be empty"}"#) }
        await #expect(throws: APIError.self) { try await NewDoc.create(api: refused.client(), title: "", parent: nil, known: []) }
    }
}

/// A tiny lock for test closures.
final class Locked<T: Sendable>: @unchecked Sendable {
    private var v: T
    private let lock = NSLock()
    init(_ v: T) { self.v = v }
    var value: T { lock.withLock { v } }
    func mutate(_ f: (inout T) -> Void) { lock.withLock { f(&v) } }
}
