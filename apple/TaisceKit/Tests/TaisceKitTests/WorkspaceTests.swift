import Foundation
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
