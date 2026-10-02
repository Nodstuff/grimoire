import Foundation
import GRDB
import Synchronization
import Testing
@testable import TaisceKit

/// ADR 0004 (one server, several people): workspace roles and names, access
/// granted and revoked through the change feed, read-only refusals in the
/// outbox, and a cache that belongs to one signed-in user.
@Suite struct MultiUserTests {
    // MARK: workspaces

    static let list = #"""
    {"workspaces":[
      {"id":"w1","name":"Work","color":null,"icon":null,"sort_key":"b","created_at":"","doc_ids":[],"doc_count":3,
       "owner_id":"u1","owner_name":"Tom","display_name":"Work","role":"owner","shared":false},
      {"id":"w2","name":"Work","color":null,"icon":null,"sort_key":"a","created_at":"","doc_ids":[],"doc_count":1,
       "owner_id":"u2","owner_name":"Aoife","display_name":"Work · Aoife","role":"editor","shared":true},
      {"id":"w3","name":"Family","color":null,"icon":null,"sort_key":"0","created_at":"","doc_ids":[],"doc_count":5,
       "owner_id":"u2","owner_name":"Aoife","display_name":"Family","role":"viewer","shared":true},
      {"id":"w4","name":"Home","color":null,"icon":null,"sort_key":"c","created_at":"","doc_ids":[],"doc_count":0,
       "owner_id":"u1","owner_name":"Tom","display_name":"Home","role":"owner","shared":true}
    ],"unsorted_count":0}
    """#

    @Test func decodesRolesAndDisplayNames() throws {
        let ws = try JSONDecoder().decode(WorkspaceList.self, from: Data(Self.list.utf8)).workspaces
        let byID = Dictionary(uniqueKeysWithValues: ws.map { ($0.id, $0) })
        #expect(byID["w2"]?.label == "Work · Aoife" && byID["w2"]?.ownerName == "Aoife")
        #expect(byID["w3"]?.isViewOnly == true && byID["w3"]?.isOwn == false && byID["w3"]?.shared == true)
        #expect(byID["w4"]?.isOwn == true && byID["w4"]?.shared == true, "my own, shared with others")
        // own first (by sort key), then the ones shared with me (by sort key)
        #expect(Workspace.ordered(ws).map(\.id) == ["w1", "w4", "w3", "w2"])
    }

    @Test func anOlderServerDecodesWithoutTheNewFields() throws {
        let old = #"{"id":"w1","name":"Work","color":null,"icon":null,"sort_key":null,"created_at":"","doc_ids":[],"doc_count":0}"#
        let w = try JSONDecoder().decode(Workspace.self, from: Data(old.utf8))
        #expect(w.label == "Work", "display_name falls back to name")
        #expect(w.role == nil && w.isOwn && !w.isViewOnly && !w.shared && w.ownerName == nil)
        // an empty display_name is no better than none
        let blank = #"{"id":"w1","name":"Work","display_name":""}"#
        #expect(try JSONDecoder().decode(Workspace.self, from: Data(blank.utf8)).label == "Work")
    }

    @Test func theCacheKeepsRolesForOfflineUse() async throws {
        let cache = try Cache.inMemory()
        let ws = try JSONDecoder().decode(WorkspaceList.self, from: Data(Self.list.utf8)).workspaces
        try await cache.replaceWorkspaces(ws)
        let back = Dictionary(uniqueKeysWithValues: try await cache.workspaces().map { ($0.id, $0) })
        #expect(back["w3"]?.role == "viewer" && back["w3"]?.isViewOnly == true)
        #expect(back["w2"]?.label == "Work · Aoife" && back["w2"]?.ownerID == "u2" && back["w2"]?.shared == true)
    }

    // MARK: the change feed's access rows

    static func doc(_ id: String, title: String, epoch: Int = 1, content: String) -> DocTree {
        let json = Fixture.docTree(id, title: title, epoch: epoch, roots: """
        {"block":\(Fixture.block("\(id)-b", doc: id, content: content, epoch: epoch)),"children":[]}
        """)
        return try! JSONDecoder().decode(DocTree.self, from: Data(json.utf8))
    }

    /// A family workspace shared with us: Plan (with a child) and its To-do; and our own doc.
    func sharedFixture() async throws -> Cache {
        let cache = try Cache.inMemory()
        try await cache.storeDoc(Self.doc("plan", title: "Family plan", content: "zebrafamily secret plan"))
        try await cache.storeDoc(Self.doc("kid", title: "Holidays", content: "zebrafamily beach"))
        try await cache.storeDoc(Self.doc("ftodo", title: "To-do", content: "## 2026-10-01\n\n- [ ] family thing · due 2026-10-02"))
        try await cache.storeDoc(Self.doc("mine", title: "Mine", content: "my own zebra notes"))
        // the tree (the bodies' fixture has no parents): bodies are kept
        try await cache.replaceTree([
            DocSummary(id: "plan", parentID: nil, title: "Family plan", currentEpoch: 1, workspaceID: "w3"),
            DocSummary(id: "kid", parentID: "plan", title: "Holidays", currentEpoch: 1, workspaceID: "w3"),
            DocSummary(id: "ftodo", parentID: "plan", title: "To-do", currentEpoch: 1, workspaceID: "w3"),
            DocSummary(id: "mine", parentID: nil, title: "Mine", currentEpoch: 1),
        ])
        try await cache.enqueue(ProposeRequest(docID: "plan", baseEpoch: 1, ops: [.replace(target: "plan-b", content: "edit")]))
        try await cache.enqueueDocWorkspace("kid", workspaceID: nil, key: "move-kid")
        try await cache.enqueue(ProposeRequest(docID: "mine", baseEpoch: 1, ops: [.replace(target: "mine-b", content: "mine edit")]))
        try await cache.enqueueTodo("/api/todo/toggle", date: "2026-10-01", itemID: "0-x", done: true, clock: TodoClock(today: "2026-10-01", utcOffset: "+00:00"), key: "todo-1")
        return cache
    }

    func engine(_ server: MockServer, cache: Cache) -> SyncEngine {
        SyncEngine(api: server.client(), cache: cache, backoff: Backoff(base: .seconds(1), cap: .seconds(30), jitter: { 1 }), sleep: { _ in })
    }

    static func revoked(_ seq: Int, _ doc: String) -> String {
        #"{"seq":\#(seq),"doc_id":"\#(doc)","kind":"deleted","epoch":null,"at":"","access":"revoked"}"#
    }

    @Test func revokedRowsDropTheSubtreeItsBodiesSearchTodosAndQueuedWrites() async throws {
        let cache = try await sharedFixture()
        #expect(try await cache.todos(in: "ftodo").count == 1)
        let page = #"{"seq":12,"more":false,"changes":[\#(Self.revoked(10, "plan")),\#(Self.revoked(11, "kid")),\#(Self.revoked(12, "ftodo"))]}"#
        let server = MockServer { r in
            switch r.path {
            case "/api/changes": .json(page)
            default: MockServer.Reply(status: 404, chunks: [Data(#"{"error":"unexpected \#(r.path)"}"#.utf8)])
            }
        }
        try await cache.setLastSeq(9)
        let sync = engine(server, cache: cache)
        let updates = await sync.updates()
        try await sync.catchUp()
        for id in ["plan", "kid", "ftodo"] {
            #expect(try await cache.doc(id) == nil, "\(id) is gone")
            #expect(try await cache.blocks(of: id).isEmpty)
        }
        #expect(try await cache.searchBlocks("zebrafamily").isEmpty, "the search index forgets it")
        #expect(try await cache.searchBlocks("zebra").map(\.docID) == ["mine"])
        #expect(try await cache.todos(in: "ftodo").isEmpty, "its to-dos stop planning alerts")
        // queued writes for the lost docs go; ours stay, in order
        let left = try await cache.pendingOutbox()
        #expect(left.map(\.path) == ["/api/propose", "/api/todo/toggle"])
        #expect(left.first.flatMap { OutboxReplayer.proposeBase($0)?.0 } == "mine")
        // the server sent a row per doc: no tree refetch needed
        #expect(!server.requests.contains { $0.path == "/api/docs" })
        #expect(try await cache.lastSeq() == 12)
        var it = updates.makeAsyncIterator()
        let u = try #require(await it.next())
        #expect(u.revokedDocIDs == ["plan", "kid", "ftodo"] && u.accessChanged && u.treeChanged)
    }

    @Test func aCachedDescendantWithoutARowIsCheckedAgainstTheTree() async throws {
        // only Plan was revoked; Holidays is still ours through another share
        // (its parent now invisible, so it is a root for us)
        let cache = try await sharedFixture()
        let tree = "[\(Fixture.summary("kid", title: "Holidays")),\(Fixture.summary("mine", title: "Mine"))]"
        let server = MockServer { r in
            r.path == "/api/docs" ? .json(tree) : MockServer.Reply(status: 404, chunks: [Data(#"{"error":"unexpected"}"#.utf8)])
        }
        let u = try await engine(server, cache: cache).apply([JSONDecoder().decode(Change.self, from: Data(Self.revoked(10, "plan").utf8))])
        #expect(u.revokedDocIDs == ["plan"])
        #expect(server.requests.contains { $0.path == "/api/docs" }, "the server's tree decides")
        let kid = try #require(try await cache.doc("kid"))
        #expect(kid.parentID == nil && kid.bodyEpoch == 1, "still ours: kept, body and all")
        #expect(try await cache.doc("ftodo") == nil, "not in the tree: gone")
        #expect(try await cache.doc("plan") == nil)
    }

    @Test func aGrantedRowShowsTheDocAndFetchesItsBody() async throws {
        let cache = try Cache.inMemory()
        let granted = #"{"seq":20,"doc_id":"g1","kind":"tree","epoch":3,"at":"","access":"granted","doc":{"title":"Shared plan","parent_id":null,"sort_key":"a","status":null,"current_epoch":3,"deleted":false,"workspace_id":"w3"}}"#
        let server = MockServer { r in
            r.path == "/api/doc/g1" ? .json(Fixture.docTree("g1", title: "Shared plan", epoch: 3, roots: """
            {"block":\(Fixture.block("g1-b", doc: "g1", content: "newly shared text", epoch: 3)),"children":[]}
            """)) : MockServer.Reply(status: 404, chunks: [Data(#"{"error":"unexpected \#(r.path)"}"#.utf8)])
        }
        let change = try JSONDecoder().decode(Change.self, from: Data(granted.utf8))
        #expect(change.isGranted && !change.isRevoked)
        let u = try await engine(server, cache: cache).apply([change])
        let g = try #require(try await cache.doc("g1"))
        #expect(g.title == "Shared plan" && g.workspaceID == "w3")
        #expect(g.bodyEpoch == 3, "fetched at once")
        #expect(try await cache.searchBlocks("newly").map(\.docID) == ["g1"])
        #expect(u.accessChanged && u.treeChanged && u.docIDs.contains("g1"))
        #expect(!server.requests.contains { $0.path == "/api/docs" }, "the row carries the state")
    }

    @Test func anOrdinaryDeleteStillDropsTheDocWithoutTheAccessNote() async throws {
        let cache = try await sharedFixture()
        let row = #"{"seq":10,"doc_id":"mine","kind":"deleted","epoch":null,"at":""}"#
        let server = MockServer { _ in MockServer.Reply(status: 404, chunks: [Data(#"{"error":"unexpected"}"#.utf8)]) }
        let u = try await engine(server, cache: cache).apply([JSONDecoder().decode(Change.self, from: Data(row.utf8))])
        #expect(try await cache.doc("mine") == nil)
        #expect(u.revokedDocIDs.isEmpty && !u.accessChanged)
        #expect(try await cache.pendingOutbox().count == 4, "a trashed doc's writes fail on their own (and can be restored)")
    }

    // MARK: the outbox

    @Test func aReadOnlyRefusalFailsThatEntryOnlyAndIsNeverRetried() async throws {
        let cache = try Cache.inMemory()
        try await cache.enqueue(ProposeRequest(docID: "viewed", baseEpoch: 1, ops: [.replace(target: "b", content: "x")]))
        try await cache.enqueue(ProposeRequest(docID: "mine", baseEpoch: 1, ops: [.replace(target: "b", content: "y")]))
        try await cache.enqueue(ProposeRequest(docID: "gone", baseEpoch: 1, ops: [.replace(target: "b", content: "z")]))
        try await cache.enqueueTodo("/api/todo/toggle", date: "2026-10-01", itemID: "0-x", done: true, key: "t1")
        let server = MockServer { r in
            let doc = r.httpBody.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }?["doc_id"] as? String
            switch doc {
            case "viewed": return MockServer.Reply(status: 403, chunks: [Data(#"{"error":"forbidden: read-only: you can view this workspace but not change it"}"#.utf8)])
            case "gone": return MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found: doc gone"}"#.utf8)])
            default: return .json(#"{"epoch":2,"verdicts":[]}"#)
            }
        }
        let replayer = OutboxReplayer(api: server.client(), cache: cache)
        let report = try await replayer.replay()
        #expect(report.forbidden == 1 && report.gone == 1)
        #expect(server.requests.count == 4, "a refusal doesn't stop the queue")
        let failed = try await cache.failedOutbox()
        let byDoc = Dictionary(uniqueKeysWithValues: failed.compactMap { e in OutboxReplayer.proposeBase(e).map { ($0.0, e) } })
        #expect(Set(byDoc.keys) == ["viewed", "gone"])
        #expect(byDoc["viewed"]?.lastError == OutboxReplayer.readOnlyMessage)
        #expect(OutboxReplayer.readOnlyMessage.contains("You can only view this workspace"))
        // a plain 404 (deleted elsewhere) keeps the write, text and all, as failed
        #expect(byDoc["gone"]?.lastError == "This doc no longer exists, so this change wasn't saved.")
        let kept = try #require(byDoc["gone"]?.body.flatMap { try? JSONDecoder().decode(ProposeRequest.self, from: $0) })
        #expect(kept.ops.first?.kind == .replace(target: "b", content: "z"), "the typed text is still there")
        #expect(try await cache.failedBlocks("gone") == ["b"], "where the editor shows refused writes")
        #expect(try await cache.pendingOutbox().isEmpty)
        #expect(try await cache.outboxState(for: "mine").failed == 0)
        // no endless retries: the next replay sends nothing
        _ = try await replayer.replay()
        #expect(server.requests.count == 4)
    }

    @Test func forbiddenIsRecognised() {
        #expect(APIError.server("forbidden: read-only: x").isForbidden)
        #expect(APIError.http(status: 403).isForbidden)
        #expect(!APIError.server("bad op").isForbidden && !APIError.notFound("x").isForbidden)
    }

    // MARK: one user's cache

    @Test func wipeForgetsEverything() async throws {
        let cache = try await sharedFixture()
        try await cache.replaceWorkspaces([Workspace(id: "w3", name: "Family", role: "viewer")])
        try await cache.setLastSeq(42)
        try await cache.setOwner("u1")
        #expect(try await !cache.isEmpty())
        try await cache.wipe()
        #expect(try await cache.isEmpty())
        #expect(try await cache.docs().isEmpty)
        #expect(try await cache.todos().isEmpty)
        #expect(try await cache.workspaces().isEmpty)
        #expect(try await cache.pendingOutbox().isEmpty)
        #expect(try await cache.failedOutbox().isEmpty)
        #expect(try await cache.searchBlocks("zebra").isEmpty)
        #expect(try await cache.lastSeq() == 0)
        #expect(try await cache.owner() == nil)
        // and it still works afterwards
        try await cache.storeDoc(Self.doc("n", title: "New", content: "fresh start"))
        #expect(try await cache.searchBlocks("fresh").map(\.docID) == ["n"])
    }

    @Test func wipeOnDiskLeavesNoContentInTheFile() async throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "taisce-wipe-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let path = dir.appending(path: "cache.sqlite").path(percentEncoded: false)
        let cache = try Cache(path: path)
        try await cache.storeDoc(Self.doc("d", title: "Private", content: "zebrauniquemarker salary"))
        try await cache.setOwner("u1")
        // the app has live observations on the pool while it wipes
        let watching = Task { for try await _ in cache.observeTree() {} }
        defer { watching.cancel() }
        try await Task.sleep(for: .milliseconds(50))
        try await cache.wipe()
        let wal = (try? FileManager.default.attributesOfItem(atPath: path + "-wal")[.size] as? Int) ?? 0
        #expect(wal == 0, "the WAL is checkpointed and truncated: no wiped page lingers there")
        #expect(try await cache.docs().isEmpty)
        #expect(try await cache.searchBlocks("zebrauniquemarker").isEmpty)
        // no trace in the database, its WAL or SHM
        for file in [path, path + "-wal", path + "-shm"] where FileManager.default.fileExists(atPath: file) {
            let bytes = try Data(contentsOf: URL(filePath: file))
            #expect(bytes.range(of: Data("zebrauniquemarker".utf8)) == nil, "\(file) still holds the text")
        }
    }

    @Test func ownershipDecisions() {
        typealias D = CacheOwnership.Decision
        let d = CacheOwnership.decide
        // the same person: keep, fresh sign-in or not
        #expect(d("u1", "u1", true, false) == D.keep)
        #expect(d("u1", "u1", false, false) == D.keep)
        // someone else on this device: start empty
        #expect(d("u1", "u2", true, false) == D.wipe(owner: "u2"))
        #expect(d("u1", "u2", false, false) == D.wipe(owner: "u2"))
        // first launch of this build, still signed in: it's theirs
        #expect(d(nil, "u1", false, false) == D.adopt("u1"))
        #expect(d(nil, "u1", true, true) == D.adopt("u1"))
        // a fresh sign-in over unclaimed data (an older build's sign-out): wipe
        #expect(d(nil, "u2", true, false) == D.wipe(owner: "u2"))
        // the profile can't be read: a resume keeps; a fresh sign-in doesn't risk it
        #expect(d("u1", nil, false, false) == D.keep)
        #expect(d("u1", nil, true, false) == D.wipe(owner: nil))
        #expect(d(nil, nil, true, true) == D.keep)
    }

    @Test func reconcileReadsTheProfileAndWipesForAnotherUser() async throws {
        let cache = try await sharedFixture()
        try await cache.setOwner("tom-principal")
        let server = MockServer { r in
            r.path == "/api/profile" ? .json(#"{"name":"Aoife","principal_id":"aoife-principal","confirmed":true}"#) : MockServer.Reply(status: 404, chunks: [])
        }
        let decision = try await CacheOwnership.reconcile(cache: cache, api: server.client(), freshSignIn: true)
        #expect(decision == .wipe(owner: "aoife-principal"))
        #expect(try await cache.docs().isEmpty)
        #expect(try await cache.pendingOutbox().isEmpty)
        #expect(try await cache.owner() == "aoife-principal")
        // and her next launch keeps her cache
        try await cache.storeDoc(Self.doc("a", title: "Aoife's", content: "hers"))
        #expect(try await CacheOwnership.reconcile(cache: cache, api: server.client(), freshSignIn: false) == .keep)
        #expect(try await cache.doc("a") != nil)
    }

    /// The upgrade edge: Tom's cache from an older build, unclaimed; the
    /// launch's profile read fails (offline), so it stays unclaimed; a later
    /// read works and records him; then his grant lapses and he signs in
    /// again: his cache and queued writes stay.
    @Test func aLaterProfileReadClaimsTheCacheSoTheSamePersonKeepsIt() async throws {
        let cache = try await sharedFixture()
        let online = Mutex(false)
        let server = MockServer { r in
            guard r.path == "/api/profile", online.withLock({ $0 }) else { return MockServer.Reply(status: 503, chunks: []) }
            return .json(#"{"name":"Tom","principal_id":"tom-principal"}"#)
        }
        let api = server.client()
        // launch, offline: a resume keeps, nothing recorded
        #expect(try await CacheOwnership.reconcile(cache: cache, api: api, freshSignIn: false) == .keep)
        #expect(try await cache.owner() == nil)
        #expect(await CacheOwnership.recordIfUnclaimed(cache: cache, api: api) == false, "still offline")
        // back online: the next successful read claims it
        online.withLock { $0 = true }
        #expect(await CacheOwnership.recordIfUnclaimed(cache: cache, api: api))
        #expect(try await cache.owner() == "tom-principal")
        // a fresh sign-in by the same person keeps everything
        let queued = try await cache.pendingOutbox().count
        #expect(try await CacheOwnership.reconcile(cache: cache, api: api, freshSignIn: true) == .keep)
        #expect(try await cache.pendingOutbox().count == queued && queued == 4)
        #expect(try await cache.doc("plan") != nil)
        // recording never overwrites someone already recorded
        try await cache.setOwner("aoife-principal")
        #expect(await CacheOwnership.recordIfUnclaimed(cache: cache, api: api))
        #expect(try await cache.owner() == "aoife-principal")
    }

    @Test func decisionChangesNothingUntilApplied() async throws {
        let cache = try await sharedFixture()
        try await cache.setOwner("tom-principal")
        let server = MockServer { _ in .json(#"{"name":"Aoife","principal_id":"aoife-principal"}"#) }
        let d = try await CacheOwnership.decision(cache: cache, api: server.client(), freshSignIn: true)
        #expect(d == .wipe(owner: "aoife-principal"))
        #expect(try await cache.doc("plan") != nil, "nothing wiped yet: the app stops sync first")
        try await CacheOwnership.apply(d, to: cache)
        #expect(try await cache.doc("plan") == nil)
        #expect(try await cache.owner() == "aoife-principal")
    }

    @Test func profileDecodes() throws {
        let p = try JSONDecoder().decode(Profile.self, from: Data(#"{"name":"Tom","principal_id":"0199","confirmed":true}"#.utf8))
        #expect(p.name == "Tom" && p.principalID == "0199")
        let old = try JSONDecoder().decode(Profile.self, from: Data(#"{"name":"Tom"}"#.utf8))
        #expect(old.principalID == nil)
    }

    @Test func historyNamesComeThroughWhole() throws {
        let row = #"{"op":{"id":"0199a0b0-c0d0-7abc-8def-0123456789ab","epoch_applied":3},"principal_name":"claude:tagger (Aoife)","principal_kind":"agent"}"#
        let e = try JSONDecoder().decode(DocHistoryEntry.self, from: Data(row.utf8))
        #expect(e.principalName == "claude:tagger (Aoife)")
    }
}
