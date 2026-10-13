import Foundation
import Testing
@testable import TaisceKit

/// Drag a doc into a folder: the API call, the queued write, the cache
/// showing it at once, and the roll-back when the server refuses.
@Suite struct DocMoveTests {
    func body(_ r: URLRequest) throws -> [String: Any] {
        let data = try #require(r.httpBody)
        return try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    /// root: a (kids a1, a2), b, c
    func seeded() async throws -> Cache {
        let cache = try Cache.inMemory()
        try await cache.replaceTree([
            DocSummary(id: "a", parentID: nil, title: "A", currentEpoch: 1, sortKey: "a"),
            DocSummary(id: "a1", parentID: "a", title: "A1", currentEpoch: 1, sortKey: "h"),
            DocSummary(id: "a2", parentID: "a", title: "A2", currentEpoch: 1, sortKey: "p"),
            DocSummary(id: "b", parentID: nil, title: "B", currentEpoch: 1, sortKey: "i"),
            DocSummary(id: "c", parentID: nil, title: "C", currentEpoch: 1, sortKey: "r", workspaceID: "w1"),
        ])
        return cache
    }

    // MARK: API

    @Test func moveDocPostsParentAndKey() async throws {
        let server = MockServer { _ in .json(#"{"ok":true}"#) }
        try await server.client().moveDoc("d1", parent: "p1", sortKey: "k")
        let r = try #require(server.requests.last)
        #expect(r.httpMethod == "POST" && r.url?.path() == "/api/doc/d1/move")
        let b = try body(r)
        #expect(b["parent_id"] as? String == "p1" && b["sort_key"] as? String == "k")
    }

    @Test func moveToTheRootSendsExplicitNulls() async throws {
        let server = MockServer { _ in .json(#"{"ok":true}"#) }
        try await server.client().moveDoc("d1", parent: nil, sortKey: nil)
        let b = try body(try #require(server.requests.last))
        #expect(b["parent_id"] is NSNull && b["sort_key"] is NSNull)
    }

    @Test func aRefusalIsAnError() async throws {
        let server = MockServer { _ in .json(#"{"error":"invalid op: move: doc cannot nest under itself"}"#) }
        await #expect(throws: APIError.server("invalid op: move: doc cannot nest under itself")) {
            try await server.client().moveDoc("a", parent: "a1", sortKey: nil)
        }
    }

    // MARK: placement

    @Test func descendantsAndSelfAreNeverTargets() {
        let p = DocPlacement([
            .init(id: "a", parentID: nil, sortKey: "a"), .init(id: "a1", parentID: "a", sortKey: "h"),
            .init(id: "a11", parentID: "a1", sortKey: "h"), .init(id: "b", parentID: nil, sortKey: "i"),
            // a cycle the client might hold mid-sync: must not hang
            .init(id: "x", parentID: "y", sortKey: nil), .init(id: "y", parentID: "x", sortKey: nil),
        ])
        #expect(p.isSelfOrDescendant("a", of: "a"))
        #expect(p.isSelfOrDescendant("a11", of: "a"))
        #expect(!p.isSelfOrDescendant("b", of: "a"))
        #expect(!p.canPlace("a", under: "a") && !p.canPlace("a", under: "a11"))
        #expect(p.canPlace("a11", under: "b") && p.canPlace("a1", under: nil) && p.canPlace("a11", under: "a"))
        #expect(!p.isSelfOrDescendant("x", of: "b"))
        #expect(p.place("a", .before, "a1") == nil, "beside its own child is inside itself")
        #expect(p.place("a", .after, "a") == nil)
    }

    @Test func intoAFolderAppendsAfterTheLastKeyedChild() {
        let p = DocPlacement([
            .init(id: "f", parentID: nil, sortKey: "a"), .init(id: "k1", parentID: "f", sortKey: "h"),
            .init(id: "k2", parentID: "f", sortKey: "p"), .init(id: "loose", parentID: "f", sortKey: nil),
            .init(id: "d", parentID: nil, sortKey: "i"),
        ])
        let key = p.appendKey(moving: "d", under: "f")
        #expect(key > "p" && OrderKey.isValid(key))
        // into an empty doc (it becomes a folder)
        #expect(OrderKey.isValid(p.appendKey(moving: "f", under: "d")))
        // the doc itself is not its own bound
        #expect(p.appendKey(moving: "k2", under: "f") > "h")
    }

    @Test func beforeAndAfterLandBetweenNeighbours() throws {
        let p = DocPlacement([
            .init(id: "a", parentID: nil, sortKey: "a"), .init(id: "b", parentID: nil, sortKey: "i"),
            .init(id: "c", parentID: nil, sortKey: "r"), .init(id: "k", parentID: "a", sortKey: "h"),
        ])
        let before = try #require(p.place("k", .before, "b"))
        #expect(before.parent == nil && before.sortKey > "a" && before.sortKey < "i")
        let after = try #require(p.place("k", .after, "c"))
        #expect(after.sortKey > "r")
        let first = try #require(p.place("c", .before, "a"))
        #expect(first.sortKey < "a")
    }

    @Test func keyForPositionSkipsUnkeyedNeighbours() {
        #expect(DocPlacement.keyForPosition(["h", nil, "p"], index: 2) > "h")
        #expect(DocPlacement.keyForPosition(["h", nil, "p"], index: 2) < "p")
        #expect(DocPlacement.keyForPosition([], index: 0) == OrderKey.between(nil, nil))
    }

    // MARK: outbox + cache

    @Test func queuedMoveShowsAtOnceAndSurvivesATreeRefresh() async throws {
        let cache = try await seeded()
        let e = try await cache.enqueueDocMove("b", parent: "a", sortKey: "t", key: "m1")
        #expect(e.method == "POST" && e.path == "/api/doc/b/move" && e.idempotencyKey == "m1")
        #expect(Cache.docMove(of: e) == QueuedDocMove(doc: "b", parent: "a", sortKey: "t"))
        var b = try #require(try await cache.doc("b"))
        #expect(b.parentID == "a" && b.sortKey == "t")
        let tree = DocTreeNode.build(try await cache.docs())
        #expect(tree.first { $0.id == "a" }?.children?.map(\.id) == ["a1", "a2", "b"])
        // a tree refresh or change row from before the move lands keeps it
        try await cache.replaceTree([
            DocSummary(id: "a", parentID: nil, title: "A", currentEpoch: 1, sortKey: "a"),
            DocSummary(id: "b", parentID: nil, title: "B renamed", currentEpoch: 2, sortKey: "i"),
        ])
        b = try #require(try await cache.doc("b"))
        #expect(b.parentID == "a" && b.sortKey == "t" && b.title == "B renamed")
        try await cache.applyDocState("b", Change.DocState(title: "B", parentID: nil, sortKey: "i", status: nil, currentEpoch: 3, deleted: false))
        #expect(try await cache.doc("b")?.parentID == "a")
        // the same key again is a no-op
        _ = try await cache.enqueueDocMove("b", parent: nil, sortKey: nil, key: "m1")
        #expect(try await cache.pendingOutbox().count == 1)
        #expect(try await cache.doc("b")?.parentID == "a")
    }

    @Test func aLandedMoveIsSentAndNoLongerReapplied() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove("b", parent: "a", sortKey: "t", key: "m1")
        let server = MockServer { _ in .json(#"{"ok":true}"#) }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 0)
        let r = try #require(server.requests.first)
        #expect(r.url?.path() == "/api/doc/b/move")
        #expect(try body(r)["parent_id"] as? String == "a")
        #expect(try await cache.pendingOutbox().isEmpty)
        #expect(try await cache.queuedMoves().isEmpty)
        // the server's own row is the truth from here on
        try await cache.applyDocState("b", Change.DocState(title: "B", parentID: "a", sortKey: "u", status: nil, currentEpoch: 1, deleted: false))
        #expect(try await cache.doc("b")?.sortKey == "u")
    }

    @Test func aRefusedMoveRollsBackToTheServersPlace() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove("a", parent: "a1", sortKey: "t", key: "m1")
        #expect(try await cache.doc("a")?.parentID == "a1")
        let server = MockServer { r in
            if r.url?.path() == "/api/doc/a/move" { return .json(#"{"error":"invalid op: move: doc cannot nest under itself"}"#) }
            return .json(#"{"doc":{"id":"a","parent_id":null,"title":"A","current_epoch":1,"sort_key":"a"},"roots":[]}"#)
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 1 && report.forbidden == 0)
        #expect(report.moveError == "Couldn't move it: a doc can't go inside itself.")
        let a = try #require(try await cache.doc("a"))
        #expect(a.parentID == nil && a.sortKey == "a")
        let failed = try await cache.failedOutbox()
        #expect(failed.count == 1 && failed.first?.lastError == report.moveError)
        // refused, it is no longer laid over the tree
        try await cache.replaceTree([DocSummary(id: "a", parentID: nil, title: "A", currentEpoch: 1, sortKey: "a")])
        #expect(try await cache.doc("a")?.parentID == nil)
    }

    @Test func aForbiddenMoveSaysWhyNotReadOnly() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove("b", parent: "a", sortKey: "t", key: "m1")
        let server = MockServer { r in
            if r.url?.path() == "/api/doc/b/move" {
                return Reply(status: 403, chunks: [Data(#"{"error":"forbidden: only the workspace owner can move docs out of it"}"#.utf8)])
            }
            return .json(#"{"doc":{"id":"b","parent_id":null,"title":"B","current_epoch":1,"sort_key":"i"},"roots":[]}"#)
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 1 && report.forbidden == 0)
        #expect(report.moveError == "Couldn't move it: only the workspace owner can move docs out of it")
        #expect(try await cache.doc("b")?.parentID == nil)
    }

    @Test func aMoveIntoAGoneFolderRestoresAndAGoneDocIsDropped() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove("b", parent: "a", sortKey: "t", key: "m1")
        try await cache.enqueueDocMove("c", parent: "a", sortKey: "u", key: "m2")
        let server = MockServer { r in
            switch r.url?.path() {
            case "/api/doc/b/move", "/api/doc/c/move": Reply(status: 404, chunks: [Data(#"{"error":"not found: doc"}"#.utf8)])
            case "/api/doc/b": .json(#"{"doc":{"id":"b","parent_id":null,"title":"B","current_epoch":1,"sort_key":"i"},"roots":[]}"#)
            default: Reply(status: 404, chunks: [Data(#"{"error":"not found: doc"}"#.utf8)])
            }
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 2 && report.gone == 0)
        #expect(try await cache.doc("b")?.parentID == nil)
        #expect(try await cache.doc("c") == nil, "the doc itself is gone")
    }

    @Test func offlineTheMoveStaysQueuedAndShown() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove("b", parent: "a", sortKey: "t", key: "m1")
        let server = MockServer { _ in Reply(status: 503, chunks: []) }
        _ = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(try await cache.pendingOutbox().count == 1)
        #expect(try await cache.doc("b")?.parentID == "a")
    }

    @Test func toTheTopLevelOfAWorkspaceLabelsFirst() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove("a1", parent: nil, sortKey: "z", keepWorkspace: "w1", key: "m1")
        let queued = try await cache.pendingOutbox()
        #expect(queued.map(\.path) == ["/api/docs/a1/workspace", "/api/doc/a1/move"])
        #expect(queued.first?.idempotencyKey == Cache.labelKey(forMove: "m1"))
        let label = try #require(queued.first?.body)
        let lb = try #require(JSONSerialization.jsonObject(with: label) as? [String: Any])
        #expect(lb["workspace_id"] as? String == "w1" && lb["request_id"] as? String == "m1:label")
        // into a folder needs no label
        try await cache.enqueueDocMove("a2", parent: "b", sortKey: "z", keepWorkspace: "w1", key: "m2")
        #expect(try await cache.pendingOutbox().count == 3)
    }

    @Test func aRefusedLabelHoldsBackItsMove() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove("a1", parent: nil, sortKey: "z", keepWorkspace: "w1", key: "m1")
        let server = MockServer { r in
            switch r.url?.path() {
            case "/api/docs/a1/workspace": Reply(status: 403, chunks: [Data(#"{"error":"forbidden: read-only"}"#.utf8)])
            case "/api/doc/a1": .json(#"{"doc":{"id":"a1","parent_id":"a","title":"A1","current_epoch":1,"sort_key":"h"},"roots":[]}"#)
            default: .json(#"{"ok":true}"#)
            }
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(!server.requests.contains { $0.url?.path() == "/api/doc/a1/move" }, "never sent")
        #expect(report.movesRefused == 1)
        let a1 = try #require(try await cache.doc("a1"))
        #expect(a1.parentID == "a" && a1.sortKey == "h")
    }

    @Test func droppingADocDropsItsQueuedMove() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove("b", parent: "a", sortKey: "t", key: "m1")
        #expect(try await cache.dropOutbox(forDocs: ["b"]) == 1)
    }
}

private typealias Reply = MockServer.Reply
