import Foundation
import Testing
@testable import TaisceKit

// Server ids are UUIDs (the daemon's extractors refuse anything else, and
// MockServer checks the move and label routes the same way).
private let A = "0199a000-0000-7000-8000-00000000000a"
private let A1 = "0199a000-0000-7000-8000-0000000000a1"
private let A2 = "0199a000-0000-7000-8000-0000000000a2"
private let B = "0199a000-0000-7000-8000-00000000000b"
private let C = "0199a000-0000-7000-8000-00000000000c"
private let W1 = "0199a000-0000-7000-8000-0000000000f1"

/// Drag a doc into a folder: the API call, the queued write, the cache
/// showing it at once, and the roll-back when the server refuses.
@Suite struct DocMoveTests {
    func body(_ r: URLRequest) throws -> [String: Any] {
        let data = try #require(r.httpBody)
        return try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    func docJSON(_ id: String, parent: String?, key: String) -> String {
        #"{"doc":{"id":"\#(id)","parent_id":\#(parent.map { "\"\($0)\"" } ?? "null"),"title":"T","current_epoch":1,"sort_key":"\#(key)"},"roots":[]}"#
    }

    static let ok = MockServer.Reply.json(#"{"ok":true}"#)
    static let labelled = MockServer.Reply.json(#"{"doc_id":"x","label":"w","workspace_id":"w"}"#)

    /// root: A (kids A1, A2), B, C; A1 and A2 inherit A's workspace
    func seeded() async throws -> Cache {
        let cache = try Cache.inMemory()
        try await cache.replaceTree([
            DocSummary(id: A, parentID: nil, title: "A", currentEpoch: 1, sortKey: "a", workspaceID: W1),
            DocSummary(id: A1, parentID: A, title: "A1", currentEpoch: 1, sortKey: "h", workspaceID: W1),
            DocSummary(id: A2, parentID: A, title: "A2", currentEpoch: 1, sortKey: "p", workspaceID: W1),
            DocSummary(id: B, parentID: nil, title: "B", currentEpoch: 1, sortKey: "i"),
            DocSummary(id: C, parentID: nil, title: "C", currentEpoch: 1, sortKey: "r"),
        ])
        return cache
    }

    func paths(_ server: MockServer) -> [String] {
        server.requests.map { "\($0.httpMethod ?? "") \($0.path)" }
    }

    // MARK: API

    @Test func moveDocPostsParentAndKey() async throws {
        let server = MockServer { _ in Self.ok }
        try await server.client().moveDoc(B, parent: A, sortKey: "k")
        let r = try #require(server.requests.last)
        #expect(r.httpMethod == "POST" && r.url?.path() == "/api/doc/\(B)/move")
        let b = try body(r)
        #expect(b["parent_id"] as? String == A && b["sort_key"] as? String == "k")
    }

    @Test func moveToTheRootSendsExplicitNulls() async throws {
        let server = MockServer { _ in Self.ok }
        try await server.client().moveDoc(B, parent: nil, sortKey: nil)
        let b = try body(try #require(server.requests.last))
        #expect(b["parent_id"] is NSNull && b["sort_key"] is NSNull)
    }

    @Test func aRefusalIsAnError() async throws {
        let server = MockServer { _ in .json(#"{"error":"invalid op: move: doc cannot nest under itself"}"#) }
        await #expect(throws: APIError.server("invalid op: move: doc cannot nest under itself")) {
            try await server.client().moveDoc(A, parent: A1, sortKey: nil)
        }
    }

    /// The bug review round 1 caught: `request_id` is a `Uuid` on the server.
    @Test func theMockRefusesWhatTheServerCannotRead() async throws {
        let server = MockServer { _ in Self.labelled }
        await #expect(throws: APIError.http(status: 422)) {
            _ = try await server.client().setDocWorkspace(B, workspaceID: W1, requestID: "m1:label")
        }
        await #expect(throws: APIError.http(status: 422)) {
            try await server.client().moveDoc(B, parent: "not-a-uuid", sortKey: nil)
        }
        await #expect(throws: APIError.http(status: 400)) {
            try await server.client().moveDoc("b", parent: nil, sortKey: nil)
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
        #expect(OrderKey.isValid(p.appendKey(moving: "f", under: "d")))
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
        let e = try await cache.enqueueDocMove(B, parent: A, sortKey: "t", key: "m1")
        #expect(e.method == "POST" && e.path == "/api/doc/\(B)/move" && e.idempotencyKey == "m1")
        #expect(Cache.docMove(of: e) == QueuedDocMove(doc: B, parent: A, sortKey: "t"))
        var b = try #require(try await cache.doc(B))
        #expect(b.parentID == A && b.sortKey == "t")
        let tree = DocTreeNode.build(try await cache.docs())
        #expect(tree.first { $0.id == A }?.children?.map(\.id) == [A1, A2, B])
        // a tree refresh or change row from before the move lands keeps it
        try await cache.replaceTree([
            DocSummary(id: A, parentID: nil, title: "A", currentEpoch: 1, sortKey: "a"),
            DocSummary(id: B, parentID: nil, title: "B renamed", currentEpoch: 2, sortKey: "i"),
        ])
        b = try #require(try await cache.doc(B))
        #expect(b.parentID == A && b.sortKey == "t" && b.title == "B renamed")
        try await cache.applyDocState(B, Change.DocState(title: "B", parentID: nil, sortKey: "i", currentEpoch: 3))
        #expect(try await cache.doc(B)?.parentID == A)
        // the same key again is a no-op
        _ = try await cache.enqueueDocMove(B, parent: nil, sortKey: nil, key: "m1")
        #expect(try await cache.pendingOutbox().count == 1)
        #expect(try await cache.doc(B)?.parentID == A)
    }

    /// An append sends no key (the server appends when it lands); the row
    /// shows it last meanwhile, through refreshes too.
    @Test func anAppendLeavesTheKeyToTheServer() async throws {
        let cache = try await seeded()
        let e = try await cache.enqueueDocMove(B, parent: A, key: "m1")
        let sentBody = try #require(e.body)
        let sent = try #require(JSONSerialization.jsonObject(with: sentBody) as? [String: Any])
        #expect(sent["sort_key"] is NSNull && sent["parent_id"] as? String == A)
        let shown = try #require(try await cache.doc(B)?.sortKey)
        #expect(shown > "p" && OrderKey.isValid(shown))
        try await cache.replaceTree([
            DocSummary(id: A, parentID: nil, title: "A", currentEpoch: 1, sortKey: "a"),
            DocSummary(id: A1, parentID: A, title: "A1", currentEpoch: 1, sortKey: "z"),
            DocSummary(id: B, parentID: nil, title: "B", currentEpoch: 1, sortKey: "i"),
        ])
        let after = try #require(try await cache.doc(B))
        #expect(after.parentID == A && after.sortKey! > "z", "still last after a sibling moved down")
    }

    @Test func aLandedMoveIsSentAndNoLongerReapplied() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(B, parent: A, sortKey: "t", key: "m1")
        let server = MockServer { _ in Self.ok }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 0)
        let r = try #require(server.requests.first)
        #expect(r.url?.path() == "/api/doc/\(B)/move")
        #expect(try body(r)["parent_id"] as? String == A)
        #expect(try await cache.pendingOutbox().isEmpty)
        #expect(try await cache.queuedMoves().isEmpty)
        try await cache.applyDocState(B, Change.DocState(title: "B", parentID: A, sortKey: "u", currentEpoch: 1))
        #expect(try await cache.doc(B)?.sortKey == "u")
    }

    @Test func aRefusedMoveRollsBackAndIsSettled() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(A, parent: A1, sortKey: "t", key: "m1")
        #expect(try await cache.doc(A)?.parentID == A1)
        let server = MockServer { r in
            if r.path == "/api/doc/\(A)/move" { return .json(#"{"error":"invalid op: move: doc cannot nest under itself"}"#) }
            return .json(self.docJSON(A, parent: nil, key: "a"))
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 1 && report.forbidden == 0)
        #expect(report.refusedMoves.first == .init(key: "m1", doc: A, message: "Couldn't move it: a doc can't go inside itself."))
        let a = try #require(try await cache.doc(A))
        #expect(a.parentID == nil && a.sortKey == "a")
        // settled: not queued, not an unsent change for sign-out to count
        #expect(try await cache.failedOutbox().isEmpty)
        #expect(try await cache.pendingOutbox().isEmpty)
        let row = try #require(try await cache.outboxEntry(key: "m1"))
        #expect(row.state == .done && row.lastError == report.moveError)
        try await cache.replaceTree([DocSummary(id: A, parentID: nil, title: "A", currentEpoch: 1, sortKey: "a")])
        #expect(try await cache.doc(A)?.parentID == nil)
    }

    @Test func aForbiddenMoveSaysWhyNotReadOnly() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(B, parent: A, sortKey: "t", key: "m1")
        let server = MockServer { r in
            if r.path == "/api/doc/\(B)/move" {
                return MockServer.Reply(status: 403, chunks: [Data(#"{"error":"forbidden: only the workspace owner can move docs out of it"}"#.utf8)])
            }
            return .json(self.docJSON(B, parent: nil, key: "i"))
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 1 && report.forbidden == 0)
        #expect(report.moveError == "Couldn't move it: only the workspace owner can move docs out of it")
        #expect(try await cache.doc(B)?.parentID == nil)
    }

    @Test func aMoveIntoAGoneFolderRestoresAndAGoneDocIsDropped() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(B, parent: A, sortKey: "t", key: "m1")
        try await cache.enqueueDocMove(C, parent: A, sortKey: "u", key: "m2")
        let gone = MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found: doc"}"#.utf8)])
        let server = MockServer { r in
            switch r.path {
            case "/api/doc/\(B)": .json(self.docJSON(B, parent: nil, key: "i"))
            default: gone
            }
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 2 && report.gone == 0)
        #expect(try await cache.doc(B)?.parentID == nil)
        #expect(try await cache.doc(C) == nil, "the doc itself is gone")
    }

    @Test func offlineTheMoveStaysQueuedAndShown() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(B, parent: A, sortKey: "t", key: "m1")
        let server = MockServer { _ in MockServer.Reply(status: 503, chunks: []) }
        _ = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(try await cache.pendingOutbox().count == 1)
        #expect(try await cache.doc(B)?.parentID == A)
    }

    /// Two queued moves of one doc: the first refused, the second still
    /// shown after the roll-back (and still queued, offline).
    @Test func aRefusedFirstMoveLeavesTheSecondShown() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(A, parent: A1, sortKey: "t", key: "m1")
        try await cache.enqueueDocMove(A, parent: B, sortKey: "k", key: "m2")
        let server = MockServer { r in
            switch r.path {
            case "/api/doc/\(A)/move":
                let parent = (try? self.body(r))?["parent_id"] as? String
                return parent == A1
                    ? .json(#"{"error":"invalid op: move: doc cannot nest under itself"}"#)
                    : MockServer.Reply(status: 503, chunks: [])
            default: return .json(self.docJSON(A, parent: nil, key: "a"))
            }
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 1)
        let a = try #require(try await cache.doc(A))
        #expect(a.parentID == B && a.sortKey == "k", "the second move laid back over the server's place")
        #expect(try await cache.pendingOutbox().map(\.idempotencyKey) == ["m2"])
    }

    // MARK: the top level of a workspace

    @Test func toTheTopLevelOfAWorkspaceLabelsFirst() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(A1, parent: nil, keepWorkspace: W1, key: "m1")
        let queued = try await cache.pendingOutbox()
        #expect(queued.map(\.path) == ["/api/docs/\(A1)/workspace", "/api/doc/\(A1)/move"])
        #expect(queued.first?.idempotencyKey == Cache.labelKey(forMove: "m1"))
        let labelBody = try #require(queued.first?.body)
        let lb = try #require(JSONSerialization.jsonObject(with: labelBody) as? [String: Any])
        let rid = try #require(lb["request_id"] as? String)
        #expect(lb["workspace_id"] as? String == W1 && UUID(uuidString: rid) != nil, "a real UUID, not the outbox key")
        // into a folder needs no label
        try await cache.enqueueDocMove(A2, parent: B, keepWorkspace: W1, key: "m2")
        #expect(try await cache.pendingOutbox().count == 3)
    }

    @Test func labelLandsThenTheMoveRuns() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(A1, parent: nil, keepWorkspace: W1, key: "m1")
        let server = MockServer { r in r.httpMethod == "PUT" ? Self.labelled : Self.ok }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 0)
        #expect(paths(server) == ["PUT /api/docs/\(A1)/workspace", "POST /api/doc/\(A1)/move"])
        // a replay of the same label row sends the same request_id
        let label = try #require(try await cache.outboxEntry(key: "m1:label"))
        let sent = try #require(server.requests.first?.httpBody)
        #expect(sent == label.body)
        #expect(try await cache.pendingOutbox().isEmpty)
        #expect(try await cache.failedOutbox().isEmpty)
        #expect(try await cache.doc(A1)?.parentID == nil)
    }

    @Test func aRefusedLabelHoldsBackItsMove() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(A1, parent: nil, keepWorkspace: W1, key: "m1")
        let server = MockServer { r in
            switch r.path {
            case "/api/docs/\(A1)/workspace": MockServer.Reply(status: 403, chunks: [Data(#"{"error":"forbidden: read-only"}"#.utf8)])
            case "/api/doc/\(A1)": .json(self.docJSON(A1, parent: A, key: "h"))
            default: Self.ok
            }
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(!server.requests.contains { $0.path == "/api/doc/\(A1)/move" }, "never sent")
        #expect(report.movesRefused == 1 && report.forbidden == 0, "one refusal, said once")
        let a1 = try #require(try await cache.doc(A1))
        #expect(a1.parentID == A && a1.sortKey == "h")
        #expect(try await cache.failedOutbox().isEmpty, "both rows settled")
    }

    /// The label landed, then the move was refused: the label is undone so
    /// the doc keeps no stray label of its own.
    @Test func labelLandedMoveRefusedUndoesTheLabel() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(A1, parent: nil, keepWorkspace: W1, key: "m1")
        let server = MockServer { r in
            switch (r.httpMethod ?? "", r.path) {
            case ("PUT", _): return Self.labelled
            case ("POST", _): return MockServer.Reply(status: 403, chunks: [Data(#"{"error":"forbidden: only the workspace owner can move its top-level docs"}"#.utf8)])
            default: return .json(self.docJSON(A1, parent: A, key: "h"))
            }
        }
        let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(report.movesRefused == 1)
        #expect(paths(server) == [
            "PUT /api/docs/\(A1)/workspace", "POST /api/doc/\(A1)/move", "GET /api/doc/\(A1)", "PUT /api/docs/\(A1)/workspace",
        ])
        let undo = try body(try #require(server.requests.last))
        #expect(undo["workspace_id"] is NSNull && UUID(uuidString: undo["request_id"] as? String ?? "") != nil)
        #expect(try await cache.doc(A1)?.parentID == A)
        #expect(try await cache.pendingOutbox().isEmpty)
        #expect(try await cache.failedOutbox().isEmpty)
    }

    @Test func droppingADocDropsItsQueuedMove() async throws {
        let cache = try await seeded()
        try await cache.enqueueDocMove(B, parent: A, sortKey: "t", key: "m1")
        #expect(try await cache.dropOutbox(forDocs: [B]) == 1)
    }
}
