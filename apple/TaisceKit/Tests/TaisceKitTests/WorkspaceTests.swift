import Foundation
import GRDB
import Testing
@testable import TaisceKit

@Suite struct WorkspaceTests {
    static let list = #"""
    {"workspaces":[{"id":"w1","name":"Work","color":"#5b8def","icon":null,"sort_key":"b","created_at":"2026-10-01T09:00:00Z","doc_ids":["d1"],"doc_count":3},
    {"id":"w2","name":"Home","color":null,"icon":null,"sort_key":"a","created_at":"2026-10-01T09:00:00Z","doc_ids":[],"doc_count":0}],"unsorted_count":2}
    """#

    func body(_ r: URLRequest) throws -> [String: Any] {
        let data = try #require(r.httpBody)
        return try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    @Test func decodesTheListInSidebarOrder() async throws {
        let server = MockServer { _ in .json(Self.list) }
        let list = try await server.client().workspaces()
        #expect(list.unsortedCount == 2)
        #expect(list.workspaces.map(\.docCount) == [3, 0])
        #expect(Workspace.ordered(list.workspaces).map(\.name) == ["Home", "Work"])
        #expect(list.workspaces[0].docIDs == ["d1"] && list.workspaces[0].color == "#5b8def")
    }

    @Test func docsAndChangeRowsCarryTheResolvedWorkspace() throws {
        let doc = #"{"id":"d1","parent_id":null,"title":"A","current_epoch":1,"workspace_id":"w1"}"#
        #expect(try JSONDecoder().decode(DocSummary.self, from: Data(doc.utf8)).workspaceID == "w1")
        let unsorted = #"{"id":"d2","parent_id":null,"title":"B","current_epoch":1,"workspace_id":null}"#
        #expect(try JSONDecoder().decode(DocSummary.self, from: Data(unsorted.utf8)).workspaceID == nil)
        let change = #"{"seq":4,"doc_id":"d1","kind":"tree","epoch":null,"at":"","doc":{"title":"A","parent_id":null,"sort_key":null,"status":null,"current_epoch":1,"deleted":false,"workspace_id":"w2"}}"#
        #expect(try JSONDecoder().decode(Change.self, from: Data(change.utf8)).doc?.workspaceID == "w2")
    }

    @Test func moveToSendsPutWithRequestID() async throws {
        let server = MockServer { _ in .json(#"{"doc_id":"d1","label":"w1","workspace_id":"w1"}"#) }
        let out = try await server.client().setDocWorkspace("d1", workspaceID: "w1", requestID: "r1")
        #expect(out.workspaceID == "w1" && out.label == "w1")
        let r = try #require(server.requests.last)
        #expect(r.httpMethod == "PUT" && r.url?.path() == "/api/docs/d1/workspace")
        let b = try body(r)
        #expect(b["workspace_id"] as? String == "w1" && b["request_id"] as? String == "r1")
    }

    @Test func moveToUnsortedSendsAnExplicitNull() async throws {
        let server = MockServer { _ in .json(#"{"doc_id":"d1","label":null,"workspace_id":null}"#) }
        _ = try await server.client().setDocWorkspace("d1", workspaceID: nil, requestID: "r2")
        let last = try #require(server.requests.last)
        let b = try body(last)
        #expect(b.keys.contains("workspace_id") && b["workspace_id"] is NSNull)
    }

    @Test func queuedMoveUsesTheKeyAsRequestID() async throws {
        let cache = try Cache.inMemory()
        let e = try await cache.enqueueDocWorkspace("d1", workspaceID: "w2", key: "k1")
        #expect(e.method == "PUT" && e.path == "/api/docs/d1/workspace")
        let data = try #require(e.body)
        let b = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
        #expect(b["workspace_id"] as? String == "w2" && b["request_id"] as? String == "k1")
    }

    @Test func createSendsNameColourAndRequestID() async throws {
        let server = MockServer { _ in .json(##"{"id":"w9","name":"Side","color":"#3fb68b","icon":null,"sort_key":null,"created_at":"","doc_ids":[],"doc_count":0}"##) }
        let w = try await server.client().createWorkspace(name: "Side", color: "#3fb68b", requestID: "r3")
        #expect(w.id == "w9")
        let last = try #require(server.requests.last)
        let b = try body(last)
        #expect(b["name"] as? String == "Side" && b["color"] as? String == "#3fb68b" && b["request_id"] as? String == "r3")
    }

    @Test func todoWritesCarryTheWorkspaceOnlyWhenSet() throws {
        let clock = TodoClock(today: "2026-10-01", utcOffset: "+01:00")
        let plain = try JSONSerialization.jsonObject(with: JSONEncoder().encode(APIClient.TodoBody(date: "2026-10-01", text: "a", clock: clock))) as? [String: Any]
        #expect(plain?["workspace"] == nil)
        let scoped = try JSONSerialization.jsonObject(with: JSONEncoder().encode(APIClient.TodoBody(date: "2026-10-01", text: "a", clock: clock.in(.id("w1"))))) as? [String: Any]
        #expect(scoped?["workspace"] as? String == "w1" && scoped?["today"] as? String == "2026-10-01")
        let deadline = try JSONSerialization.jsonObject(with: JSONEncoder().encode(APIClient.DeadlineBody(date: "2026-10-01", itemID: "0-x", deadline: nil, clock: clock.in(.unsorted)))) as? [String: Any]
        #expect(deadline?["workspace"] as? String == "unsorted")
    }

    @Test func dueAndSearchTakeTheWorkspaceParam() async throws {
        let server = MockServer { r in
            r.url?.path() == "/api/search" ? .json("[]") : .json(#"{"items":[{"date":"2026-10-01","id":"0-a","text":"a","deadline":"2026-10-02","doc_id":"t2","workspace_id":"w2"}],"doc_id":null,"epoch":0,"now":"2026-10-01T00:00:00Z","default_alert_time":"09:00"}"#)
        }
        let api = server.client()
        let all = try await api.todoDue()
        #expect(all.items.first?.workspaceID == "w2" && all.items.first?.docID == "t2")
        _ = try await api.todoDue(workspace: .id("w1"))
        _ = try await api.search("q", workspace: .unsorted)
        _ = try await api.search("q")
        let reqs = server.requests
        #expect(reqs[0].query["workspace"] == nil, "alerts read every list")
        #expect(reqs[1].query["workspace"] == "w1")
        #expect(reqs[2].query["workspace"] == "unsorted")
        #expect(reqs[3].query["workspace"] == nil)
    }

    @Test func newRootDocsCarryTheWorkspaceChildrenDoNot() async throws {
        let server = MockServer { _ in .json(#"{"id":"n1","parent_id":null,"title":"New","current_epoch":0,"workspace_id":"w1"}"#) }
        let api = server.client()
        let made = try await NewDoc.create(api: api, title: "New", parent: nil, known: [], workspaceID: "w1")
        #expect(made.workspaceID == "w1")
        _ = try await api.createDoc(title: "Kid", parent: "p1", workspaceID: "w1")
        let reqs = server.requests
        let root = try #require(reqs[0].httpBody)
        let kid = try #require(reqs[1].httpBody)
        let r = try #require(JSONSerialization.jsonObject(with: root) as? [String: Any])
        let k = try #require(JSONSerialization.jsonObject(with: kid) as? [String: Any])
        #expect(r["workspace_id"] as? String == "w1" && r["parent_doc_id"] == nil)
        #expect(k["workspace_id"] == nil && k["parent_doc_id"] as? String == "p1")
    }

    /// Tom's phone has a v1 cache: open one with the full Cache and keep
    /// everything but the to-do rows (rebuilt from the To-do doc on the next
    /// sync) and the cursor (reset so a bootstrap fills in workspace ids).
    @Test func aV1CacheMigratesThroughBothV2s() async throws {
        let q = try DatabaseQueue()
        try Cache.migrator.migrate(q, upTo: "v1")
        try await q.write { db in
            try db.execute(sql: """
                INSERT INTO docs(id, parent_id, title, current_epoch, sort_key, status, is_canvas, is_shared, mirror_permission, body_epoch) VALUES
                ('t', NULL, 'To-do', 3, 'a', NULL, 0, 0, NULL, 3),
                ('g', NULL, 'Grimoire', 5, 'b', 'active', 0, 1, NULL, 5),
                ('r', 'g', 'Roadmap', 2, 'a', NULL, 0, 0, NULL, NULL)
                """)
            try db.execute(sql: """
                INSERT INTO blocks(id, doc_id, parent_id, order_key, block_type, content, epoch, refers_to, position, depth) VALUES
                ('th', 't', NULL, 'a', 'heading', '## 2026-10-01', 3, NULL, 0, 0),
                ('ti', 't', 'th', 'b', 'paragraph', '- [ ] old item · due 2026-10-02', 3, NULL, 1, 1),
                ('gh', 'g', NULL, 'a', 'heading', '# Grimoire', 5, NULL, 0, 0),
                ('gp', 'g', 'gh', 'b', 'paragraph', 'alpha bravo', 5, NULL, 1, 1)
                """)
            try db.execute(sql: "INSERT INTO todos(date, position, mark, text, deadline) VALUES ('2026-10-01', 0, ' ', 'old item', '2026-10-02')")
            try db.execute(sql: "INSERT INTO sync_state(key, value) VALUES ('last_seq', 42)")
            try db.execute(sql: """
                INSERT INTO outbox(created_at, idempotency_key, method, path, body, state, attempts, last_error) VALUES
                ('2026-10-01 09:00:00.000', 'k1', 'POST', '/api/todo/toggle', X'7B7D', 'pending', 1, 'offline')
                """)
        }
        let cache = try Cache(writer: q)
        let applied = try await q.read { db in try Cache.migrator.appliedIdentifiers(db) }
        #expect(applied == ["v1", "v2", "v2-workspaces", "v3"])
        let (outbox, docCols, todoCols, wsTable) = try await q.read { db in
            (try db.columns(in: "outbox").map(\.name), try db.columns(in: "docs").map(\.name),
             try db.columns(in: "todos").map(\.name), try db.tableExists("workspaces"))
        }
        #expect(outbox.contains("outcome"), "the editor's v2")
            #expect(outbox.contains("conflict"), "the editor's v3")
        #expect(docCols.contains("workspace_id") && todoCols.contains("doc_id") && wsTable)
        // docs and blocks survive; only the To-do body is marked for refetch
        let docs = try await cache.docs()
        #expect(Set(docs.map(\.id)) == ["t", "g", "r"])
        #expect(docs.allSatisfy { $0.workspaceID == nil }, "filled in by the bootstrap")
        let g = try #require(docs.first { $0.id == "g" })
        #expect(g.bodyEpoch == 5 && g.isShared && g.status == "active")
        #expect(try await cache.doc("t")?.bodyEpoch == nil, "the To-do doc refetches its list")
        #expect(try await cache.blocks(of: "g").map(\.content) == ["# Grimoire", "alpha bravo"])
        #expect(try await cache.blocks(of: "t").count == 2)
        #expect(try await cache.searchBlocks("bravo").map(\.id) == ["gp"], "FTS still indexed")
        // the queued write survives, untouched, with no outcome yet
        let queued = try await cache.pendingOutbox()
        #expect(queued.count == 1)
        let e = try #require(queued.first)
        #expect(e.idempotencyKey == "k1" && e.path == "/api/todo/toggle" && e.attempts == 1 && e.lastError == "offline")
        #expect(e.body == Data("{}".utf8) && e.outcome == nil)
        // cursor reset: the next catch-up bootstraps the tree (bodies kept)
        #expect(try await cache.lastSeq() == 0)
        // to-do rows are rebuilt from the To-do doc on its next fetch, keyed by it
        #expect(try await cache.todos().isEmpty)
        let json = Fixture.docTree("t", title: "To-do", epoch: 4, roots: """
        {"block":\(Fixture.block("th", doc: "t", type: "heading", content: "## 2026-10-01", epoch: 4)),"children":[
          {"block":\(Fixture.block("ti", doc: "t", content: "- [ ] old item · due 2026-10-02", parent: "th", epoch: 4)),"children":[]}
        ]}
        """)
        try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(json.utf8)))
        let rebuilt = try await cache.todos()
        #expect(rebuilt.map(\.text) == ["old item"] && rebuilt.map(\.docID) == ["t"])
        #expect(try await cache.todos(in: "t").first?.deadline == "2026-10-02")
    }

    @Test func scopeParamRoundTrips() throws {
        #expect(WorkspaceScope(param: "unsorted") == .unsorted)
        #expect(WorkspaceScope(param: "w1") == .id("w1"))
        #expect(WorkspaceScope(nil) == .unsorted && WorkspaceScope("w1").workspaceID == "w1")
        let data = try JSONEncoder().encode([WorkspaceScope.unsorted, .id("w1")])
        #expect(try JSONDecoder().decode([WorkspaceScope].self, from: data) == [.unsorted, .id("w1")])
    }

    @Test func cacheKeepsOneListPerToDoDocAndTheWorkspaceIDs() async throws {
        let cache = try Cache.inMemory()
        try await cache.replaceTree([
            DocSummary(id: "t1", parentID: nil, title: "To-do", currentEpoch: 1),
            DocSummary(id: "p", parentID: nil, title: "Work", currentEpoch: 1, workspaceID: "w1"),
            DocSummary(id: "t2", parentID: "p", title: "To-do", currentEpoch: 1, workspaceID: "w1"),
        ])
        for (id, text) in [("t1", "home thing"), ("t2", "work thing")] {
            let json = Fixture.docTree(id, title: "To-do", roots: """
            {"block":\(Fixture.block("\(id)h", doc: id, type: "heading", content: "## 2026-10-01")),"children":[
              {"block":\(Fixture.block("\(id)i", doc: id, content: "- [ ] \(text) · due 2026-10-02", parent: "\(id)h")),"children":[]}
            ]}
            """)
            try await cache.storeDoc(JSONDecoder().decode(DocTree.self, from: Data(json.utf8)))
        }
        #expect(try await cache.todos().count == 2, "every workspace's list is cached")
        #expect(try await cache.todos(in: "t2").map(\.text) == ["work thing"])
        #expect(try await cache.todos(in: "t1").map(\.docID) == ["t1"])
        // storeDoc keeps the tree's workspace (the doc route has none)
        #expect(try await cache.doc("t2")?.workspaceID == "w1")
        try await cache.applyDocState("t1", Change.DocState(title: "To-do", currentEpoch: 1, workspaceID: "w2"))
        #expect(try await cache.doc("t1")?.workspaceID == "w2")
        try await cache.replaceWorkspaces([Workspace(id: "w1", name: "Work", color: "#5b8def", docCount: 2)])
        #expect(try await cache.workspaces().map(\.name) == ["Work"])
    }
}
