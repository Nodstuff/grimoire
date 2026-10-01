import Foundation
import Synchronization
import Testing
@testable import TaisceKit

@Suite struct CacheTests {
    func tree(_ id: String, title: String = "Doc", epoch: Int = 2) throws -> DocTree {
        let json = Fixture.docTree(id, title: title, epoch: epoch, roots: """
        {"block":\(Fixture.block("\(id)h1", doc: id, type: "heading", content: "# Title")),"children":[
          {"block":\(Fixture.block("\(id)p1", doc: id, content: "alpha bravo", parent: "\(id)h1")),"children":[]},
          {"block":\(Fixture.block("\(id)p2", doc: id, content: "charlie", parent: "\(id)h1")),"children":[]}
        ]},
        {"block":\(Fixture.block("\(id)h2", doc: id, type: "heading", content: "# Second")),"children":[]}
        """)
        return try JSONDecoder().decode(DocTree.self, from: Data(json.utf8))
    }

    @Test func storesBodyInDocumentOrderWithDepth() async throws {
        let cache = try Cache.inMemory()
        try await cache.storeDoc(tree("d1"))
        let blocks = try await cache.blocks(of: "d1")
        #expect(blocks.map(\.id) == ["d1h1", "d1p1", "d1p2", "d1h2"])
        #expect(blocks.map(\.depth) == [0, 1, 1, 0])
        #expect(try await cache.doc("d1")?.bodyEpoch == 2)
    }

    @Test func replaceTreeKeepsBodiesAndDropsMissingDocs() async throws {
        let cache = try Cache.inMemory()
        try await cache.storeDoc(tree("d1"))
        try await cache.storeDoc(tree("d2"))
        try await cache.replaceTree([
            DocSummary(id: "d1", parentID: nil, title: "Renamed", currentEpoch: 3),
            DocSummary(id: "d3", parentID: "d1", title: "New", currentEpoch: 1),
        ])
        let docs = try await cache.docs()
        #expect(Set(docs.map(\.id)) == ["d1", "d3"])
        let d1 = try #require(docs.first { $0.id == "d1" })
        #expect(d1.title == "Renamed" && d1.bodyEpoch == 2 && d1.isBodyStale)
        #expect(try await cache.blocks(of: "d2").isEmpty, "blocks cascade with their doc")
        #expect(try await cache.blocks(of: "d1").count == 4)
    }

    @Test func storeReplacesBlocksAndDeleteCascades() async throws {
        let cache = try Cache.inMemory()
        try await cache.storeDoc(tree("d1"))
        let smaller = try JSONDecoder().decode(DocTree.self, from: Data(Fixture.docTree("d1", title: "Doc", epoch: 3, roots: """
        {"block":\(Fixture.block("p9", doc: "d1", content: "only")),"children":[]}
        """).utf8))
        try await cache.storeDoc(smaller)
        #expect(try await cache.blocks(of: "d1").map(\.id) == ["p9"])
        try await cache.deleteDoc("d1")
        #expect(try await cache.doc("d1") == nil)
        #expect(try await cache.blocks(of: "d1").isEmpty)
    }

    @Test func fullTextSearchFollowsUpserts() async throws {
        let cache = try Cache.inMemory()
        try await cache.storeDoc(tree("d1"))
        #expect(try await cache.searchBlocks("brav").map(\.id) == ["d1p1"])
        try await cache.deleteDoc("d1")
        #expect(try await cache.searchBlocks("brav").isEmpty)
    }

    @Test func noteEpochMarksStaleWithoutBody() async throws {
        let cache = try Cache.inMemory()
        try await cache.replaceTree([DocSummary(id: "d1", parentID: nil, title: "A", currentEpoch: 1)])
        try await cache.noteEpoch("d1", epoch: 5)
        try await cache.noteEpoch("d1", epoch: 4)
        let d = try #require(try await cache.doc("d1"))
        #expect(d.currentEpoch == 5 && d.bodyEpoch == nil && d.isBodyStale)
    }

    @Test func todoDocIsParsedIntoTodos() async throws {
        let cache = try Cache.inMemory()
        let json = Fixture.docTree("todo", title: "To-do", roots: """
        {"block":\(Fixture.block("h", doc: "todo", type: "heading", content: "## 2026-10-01")),"children":[
          {"block":\(Fixture.block("i1", doc: "todo", content: "- [ ] pay rent · due 2026-10-01 09:30", parent: "h")),"children":[]},
          {"block":\(Fixture.block("i2", doc: "todo", content: "- [x] done", parent: "h")),"children":[]}
        ]}
        """)
        try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(json.utf8)))
        let todos = try await cache.todos()
        #expect(todos.map(\.text) == ["pay rent", "done"])
        #expect(todos.first?.due == Due(year: 2026, month: 10, day: 1, hour: 9, minute: 30))
    }

    @Test func lastSeqRoundTrips() async throws {
        let cache = try Cache.inMemory()
        #expect(try await cache.lastSeq() == 0)
        try await cache.setLastSeq(42)
        try await cache.setLastSeq(43)
        #expect(try await cache.lastSeq() == 43)
    }
}

@Suite struct OutboxTests {
    func propose(_ rid: String?) -> ProposeRequest {
        ProposeRequest(docID: "d1", baseEpoch: 3, ops: [.replace(target: "b1", content: "new")], requestID: rid)
    }

    @Test func enqueueIsIdempotentAndOrdered() async throws {
        let cache = try Cache.inMemory()
        let a = try await cache.enqueue(propose("11111111-1111-1111-1111-111111111111"))
        let again = try await cache.enqueue(propose("11111111-1111-1111-1111-111111111111"))
        let b = try await cache.enqueue(propose(nil))
        #expect(a.id == again.id)
        let pending = try await cache.pendingOutbox()
        #expect(pending.map(\.id) == [a.id, b.id])
        #expect(pending.allSatisfy { $0.state == .pending })
        // the key is the server's request_id, minted when absent
        let body = try #require(pending[1].body)
        let obj = try #require(try JSONSerialization.jsonObject(with: body) as? [String: Any])
        #expect(obj["request_id"] as? String == pending[1].idempotencyKey)
        #expect((obj["ops"] as? [[String: Any]])?.first?["kind"] as? [String: String] == ["op": "replace", "target": "b1", "content": "new"])
    }

    @Test func replaySendsInOrderAndStopsOnTransportFailure() async throws {
        let cache = try Cache.inMemory()
        let first = try await cache.enqueue(propose(nil))
        let second = try await cache.enqueue(propose(nil))
        let third = try await cache.enqueue(propose(nil))
        let calls = Counter()
        let server = MockServer { _ in
            switch calls.next() {
            case 0: .json(#"{"doc_id":"d1","epoch":4,"verdicts":[]}"#)
            case 1: .json(#"{"error":"hot session"}"#)
            default: MockServer.Reply(chunks: [], failAfter: true)
            }
        }
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(server.requests.map(\.path) == ["/api/propose", "/api/propose", "/api/propose"])
        let left = try await cache.pendingOutbox()
        // first done, second failed (server refused), third back to pending
        #expect(left.map(\.id) == [third.id])
        #expect(left.first?.attempts == 1 && left.first?.lastError != nil)
        _ = (first, second)
    }
}

final class Counter: Sendable {
    private let n = Mutex(0)
    func next() -> Int { n.withLock { v in defer { v += 1 }; return v } }
}
