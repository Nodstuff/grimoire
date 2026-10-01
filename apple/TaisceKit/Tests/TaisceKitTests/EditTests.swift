import Foundation
import Testing
@testable import TaisceKit

@Suite struct OrderKeyTests {
    /// The vectors crates/store/src/order_key.rs shares with the web UI.
    @Test func fixedVectorsMatchTheStore() {
        let vectors: [(String?, String?, String)] = [
            (nil, nil, "i"), ("i", nil, "r"), (nil, "i", "9"),
            ("i", "r", "m"), ("i", "j", "ii"), ("z", nil, "zi"),
        ]
        for (a, b, want) in vectors {
            #expect(OrderKey.between(a, b) == want, "between(\(a ?? "nil"), \(b ?? "nil"))")
        }
    }

    @Test func denseAppendsAndBisectionsStayOrdered() {
        var prev = OrderKey.between(nil, nil)
        for _ in 0..<100 {
            let next = OrderKey.between(prev, nil)
            #expect(prev < next && OrderKey.isValid(next))
            prev = next
        }
        let hi = OrderKey.between(nil, nil)
        var lo = OrderKey.between(nil, hi)
        for _ in 0..<100 {
            let mid = OrderKey.between(lo, hi)
            #expect(lo < mid && mid < hi && OrderKey.isValid(mid))
            lo = mid
        }
    }

    @Test func totalLikeTheServer() {
        #expect(OrderKey.between("i", "i") > "i")
        #expect(OrderKey.between("r", "i") > "r")
        #expect(OrderKey.between("I-1", "r") > "r", "garbage bound: lands after the valid one")
        #expect(OrderKey.between("i", "i0") > "i")
        #expect(OrderKey.between("!!", nil) == "i")
        #expect(!OrderKey.isValid("") && !OrderKey.isValid("i0") && !OrderKey.isValid("I") && OrderKey.isValid("zz9"))
    }
}

@Suite struct DocEditorTests {
    // h1 { p1, p2 }, h2 — keys as the server would have minted them
    func editor() -> DocEditor {
        DocEditor(docID: "d1", baseEpoch: 5, blocks: [
            Block(id: "h1", docID: "d1", parentID: nil, orderKey: "i", blockType: .heading, content: "# One"),
            Block(id: "p1", docID: "d1", parentID: "h1", orderKey: "i", blockType: .paragraph, content: "alpha"),
            Block(id: "p2", docID: "d1", parentID: "h1", orderKey: "r", blockType: .paragraph, content: "bravo"),
            Block(id: "h2", docID: "d1", parentID: nil, orderKey: "r", blockType: .heading, content: "# Two"),
        ])
    }

    func ids(_ e: DocEditor) -> [String] { e.ordered().map(\.block.id) }

    @Test func replaceText() throws {
        var e = editor()
        #expect(try e.apply(.replaceText("p1", "alpha!")) == [.replace(target: "p1", content: "alpha!")])
        #expect(e.blocks["p1"]?.content == "alpha!")
        #expect(throws: EditError.unknownBlock("nope")) { try e.apply(.replaceText("nope", "x")) }
    }

    @Test func insertAfterLandsBetweenNeighbours() throws {
        var e = editor()
        let ops = try e.apply(.insert(after: "p1", parent: "h1", type: .paragraph, content: "mid", id: "n1"))
        #expect(ops == [.insert(blockID: "n1", parentID: "h1", orderKey: "m", type: .paragraph, content: "mid")])
        #expect(ids(e) == ["h1", "p1", "n1", "p2", "h2"])
        // after the last sibling, first under a parent, and a minted id
        _ = try e.apply(.insert(after: "h2", parent: nil, type: .paragraph, content: "end", id: "n2"))
        _ = try e.apply(.insert(after: nil, parent: "h1", type: .paragraph, content: "first", id: "n3"))
        let minted = try e.apply(.insert(after: "n2", parent: nil, type: .paragraph, content: "x"))
        guard case let .insert(id?, nil, key, _, _) = minted.first else { Issue.record("no id"); return }
        #expect(UUID(uuidString: id) != nil && key > "r")
        #expect(ids(e) == ["h1", "n3", "p1", "n1", "p2", "h2", "n2", id])
        #expect(throws: EditError.notASibling("p1")) { try e.apply(.insert(after: "p1", parent: nil, type: .paragraph, content: "")) }
    }

    @Test func deleteTakesTheSubtreeChildrenFirst() throws {
        var e = editor()
        #expect(try e.apply(.delete("h1")) == [.delete(target: "p1"), .delete(target: "p2"), .delete(target: "h1")])
        #expect(ids(e) == ["h2"])
    }

    @Test func moveReparentsAndRefusesCycles() throws {
        var e = editor()
        // p2 to the top, before h1
        #expect(try e.apply(.move("p2", after: nil, parent: nil)) == [.move(target: "p2", newParent: nil, newOrderKey: "9")])
        #expect(ids(e) == ["p2", "h1", "p1", "h2"])
        // p1 to the end of h2's section (h2 has no children yet)
        _ = try e.apply(.move("p1", after: nil, parent: "h2"))
        #expect(ids(e) == ["p2", "h1", "h2", "p1"])
        // the moving block is not its own neighbour
        #expect(try e.apply(.move("h1", after: "h2", parent: nil)).first == .move(target: "h1", newParent: nil, newOrderKey: "v"))
        #expect(throws: EditError.cycle("h2")) { try e.apply(.move("h2", after: nil, parent: "p1")) }
    }

    @Test func proposeIsAllOrNothing() throws {
        var e = editor()
        #expect(throws: EditError.unknownBlock("zz")) {
            try e.propose([.replaceText("p1", "changed"), .delete("zz")])
        }
        #expect(e.blocks["p1"]?.content == "alpha", "a failed batch leaves the editor untouched")
        let req = try e.propose([.replaceText("p1", "changed"), .delete("p2")], requestID: "11111111-1111-1111-1111-111111111111")
        #expect(req.docID == "d1" && req.baseEpoch == 5 && req.ops.map(\.kind) == [.replace(target: "p1", content: "changed"), .delete(target: "p2")])
    }

    @Test func blockOpsRoundTripThroughJSON() throws {
        let ops: [BlockOp] = [
            .insert(blockID: "n", parentID: nil, orderKey: "i", type: .heading, content: "# x"),
            .insert(blockID: nil, parentID: "h", orderKey: "r", type: .paragraph, content: "y"),
            .replace(target: "a", content: "b"), .delete(target: "c"),
            .move(target: "d", newParent: nil, newOrderKey: "m"),
        ]
        #expect(try JSONDecoder().decode([BlockOp].self, from: JSONEncoder().encode(ops)) == ops)
    }
}

@Suite struct EditOutboxTests {
    func cachedDoc() async throws -> Cache {
        let cache = try Cache.inMemory()
        let json = Fixture.docTree("d1", title: "Doc", epoch: 5, roots: """
        {"block":\(Fixture.block("h1", doc: "d1", type: "heading", content: "# One")),"children":[
          {"block":\(Fixture.block("p1", doc: "d1", content: "alpha", parent: "h1")),"children":[]}
        ]}
        """)
        try await cache.storeDoc(try JSONDecoder().decode(DocTree.self, from: Data(json.utf8)))
        return cache
    }

    func sent(_ r: URLRequest) throws -> [String: Any] {
        try #require(try JSONSerialization.jsonObject(with: r.httpBody ?? Data()) as? [String: Any])
    }

    /// Offline: edit, insert, then edit the inserted block. The editor from
    /// the cache sees the queue; replay sends in order, each propose based on
    /// the epoch the previous one landed at, with its own request_id.
    @Test func queuedEditsChainAndReplayInOrder() async throws {
        let cache = try await cachedDoc()
        var e = try #require(try await cache.editor(for: "d1"))
        #expect(e.baseEpoch == 5)
        try await cache.enqueue([.replaceText("p1", "alpha 2")], on: &e)
        try await cache.enqueue([.insert(after: "p1", parent: "h1", type: .paragraph, content: "new", id: "n1")], on: &e)

        // a fresh editor (the app relaunched) still sees both queued edits
        var again = try #require(try await cache.editor(for: "d1"))
        #expect(again.ordered().map(\.block.id) == ["h1", "p1", "n1"])
        #expect(again.blocks["p1"]?.content == "alpha 2" && again.baseEpoch == 5)
        try await cache.enqueue([.replaceText("n1", "new, edited")], on: &again)

        let epoch = Counter()
        let server = MockServer { _ in .json(#"{"doc_id":"d1","epoch":\#(6 + epoch.next()),"verdicts":[]}"#) }
        try await OutboxReplayer(api: server.client(), cache: cache).replay()

        let bodies = try server.requests.map(sent)
        #expect(server.requests.map(\.path) == ["/api/propose", "/api/propose", "/api/propose"])
        #expect(bodies.map { $0["base_epoch"] as? Int } == [5, 6, 7])
        let rids = bodies.compactMap { $0["request_id"] as? String }
        #expect(Set(rids).count == 3 && rids.allSatisfy { UUID(uuidString: $0) != nil })
        let kinds = bodies.map { (($0["ops"] as? [[String: Any]])?.first?["kind"] as? [String: Any])?["op"] as? String }
        #expect(kinds == ["replace", "insert", "replace"])
        #expect(try await cache.pendingOutbox().isEmpty)
    }

    @Test func aLiveSessionKeepsTheQueueForLater() async throws {
        let cache = try await cachedDoc()
        var e = try #require(try await cache.editor(for: "d1"))
        try await cache.enqueue([.replaceText("p1", "one")], on: &e)
        try await cache.enqueue([.replaceText("p1", "two")], on: &e)
        let server = MockServer { _ in
            .json(#"{"error":"doc is in a live session — edits go through the session; retry after it ends (P2.3)"}"#)
        }
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(server.requests.count == 1, "stops at the refused entry to keep the order")
        let left = try await cache.pendingOutbox()
        #expect(left.count == 2 && left.first?.lastError?.contains("live session") == true)
    }

    @Test func rebaseOnlyTouchesTheSameDocAndBase() async throws {
        let cache = try Cache.inMemory()
        try await cache.enqueue(ProposeRequest(docID: "d1", baseEpoch: 5, ops: [.delete(target: "a")]))
        try await cache.enqueue(ProposeRequest(docID: "d2", baseEpoch: 5, ops: [.delete(target: "b")]))
        try await cache.enqueue(ProposeRequest(docID: "d1", baseEpoch: 4, ops: [.delete(target: "c")]))
        try await cache.rebaseOutbox(docID: "d1", from: 5, to: 9)
        let bases = try await cache.pendingOutbox().map { e in
            try JSONDecoder().decode(ProposeRequest.self, from: e.body ?? Data()).baseEpoch
        }
        #expect(bases == [9, 5, 4])
    }
}
