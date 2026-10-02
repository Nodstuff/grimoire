import Foundation
import Testing
import TaisceKit
@testable import Taisce

private let work = Workspace(id: "w1", name: "Work", color: "#5b8def", sortKey: "b")
private let home = Workspace(id: "w2", name: "Home", sortKey: "a")

private let docs = [
    DocInfo(id: "todo", title: "To-do"),
    DocInfo(id: "loose", title: "Loose note"),
    DocInfo(id: "wroot", title: "Work", workspaceID: "w1"),
    DocInfo(id: "wtodo", parentID: "wroot", title: "To-do", workspaceID: "w1"),
    DocInfo(id: "wdeep", parentID: "wtodo", title: "To-do", workspaceID: "w1"),
    // labelled away from its parent: shows at Home's root
    DocInfo(id: "hkid", parentID: "wroot", title: "Recipes", workspaceID: "w2"),
]

@Suite struct WorkspacePickerTests {
    @Test func opensInTheStoredWorkspace() {
        let p = WorkspacePicker(workspaces: [work, home], unsortedCount: 0, stored: .id("w1"))
        #expect(p.current == .id("w1"))
    }

    @Test func noChoiceOrAVanishedOneFallsBackToTheFirstInOrder() {
        #expect(WorkspacePicker(workspaces: [work, home], unsortedCount: 0, stored: nil).current == .id("w2"))
        #expect(WorkspacePicker(workspaces: [work, home], unsortedCount: 0, stored: .id("gone")).current == .id("w2"))
        #expect(WorkspacePicker(workspaces: [], unsortedCount: 4, stored: nil).current == .unsorted)
    }

    @Test func unsortedIsOfferedOnlyWhileItHasDocs() {
        let some = WorkspacePicker(workspaces: [work, home], unsortedCount: 2, stored: .unsorted)
        #expect(some.options == [.id("w2"), .id("w1"), .unsorted])
        #expect(some.current == .unsorted)
        let none = WorkspacePicker(workspaces: [work, home], unsortedCount: 0, stored: .unsorted)
        #expect(none.options == [.id("w2"), .id("w1")])
        #expect(none.current == .id("w2"), "triaged empty: the first workspace")
        #expect(none.name(.unsorted) == "Unsorted" && none.name(.id("w1")) == "Work" && none.color(.id("w1")) == "#5b8def")
    }

    @Test func offOnADaemonWithoutWorkspaces() {
        #expect(WorkspacePicker(workspaces: [], unsortedCount: 3, stored: .id("w1"), enabled: false).current == nil)
    }

    @Test func thePreferencePersists() throws {
        let suite = "taisce.tests.workspace.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let pref = WorkspacePreference(defaults: defaults, key: "workspace-host-0")
        #expect(pref.load() == nil)
        pref.save(.id("w1"))
        #expect(WorkspacePreference(defaults: defaults, key: "workspace-host-0").load() == .id("w1"))
        pref.save(.unsorted)
        #expect(pref.load() == .unsorted)
        #expect(WorkspacePreference(defaults: defaults, key: "workspace-other-0").load() == nil, "per server")
    }
}

@Suite struct WorkspaceFilterTests {
    @Test func treeShowsOnlyTheCurrentWorkspace() {
        #expect(WorkspaceFilter.docs(docs, in: .id("w1")).map(\.id) == ["wroot", "wtodo", "wdeep"])
        #expect(WorkspaceFilter.docs(docs, in: .unsorted).map(\.id) == ["todo", "loose"])
        #expect(WorkspaceFilter.docs(docs, in: nil).count == docs.count, "workspaces off: everything")
        let homeTree = LibraryNode.build(WorkspaceFilter.docs(docs, in: .id("w2")))
        #expect(homeTree.map(\.id) == ["hkid"], "a doc labelled away from its parent sits at the root")
    }

    @Test func eachWorkspaceHasItsOwnToDoList() {
        let index = DocIndex(docs)
        #expect(WorkspaceFilter.todoDoc(in: docs, index: index, scope: .id("w1")) == "wtodo", "the shallowest")
        #expect(WorkspaceFilter.todoDoc(in: docs, index: index, scope: .unsorted) == "todo")
        #expect(WorkspaceFilter.todoDoc(in: docs, index: index, scope: .id("w2")) == nil, "created on first write")
        #expect(WorkspaceFilter.todoDoc(in: docs, index: index, scope: nil) == "todo")
    }

    @Test func todayAndToDoWritesGoToTheCurrentList() throws {
        let clock = TodoClock(today: "2026-10-01", utcOffset: "+00:00")
        #expect(clock.in(.id("w1")).workspace == "w1")
        #expect(clock.in(.unsorted).workspace == "unsorted")
        #expect(clock.in(nil).workspace == nil)
    }

    @Test func emptyWorkspaceSaysSo() {
        let e = WorkspaceEmptyState(name: "Home")
        #expect(e.title == "Nothing in Home yet")
        #expect(e.hint.contains("Move to workspace"))
    }
}

@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct WorkspaceAlertTests {
    func todo(_ id: String, _ text: String) throws -> DocTree {
        let json = """
        {"doc":{"id":"\(id)","parent_id":null,"title":"To-do","current_epoch":1},"roots":[
          {"block":{"id":"\(id)h","doc_id":"\(id)","parent_id":null,"order_key":"a","block_type":"heading","content":"## 2026-09-21","epoch":1,"deleted":false},"children":[
            {"block":{"id":"\(id)i","doc_id":"\(id)","parent_id":"\(id)h","order_key":"b","block_type":"paragraph","content":"- [ ] \(text) · due 2026-09-22 15:00","epoch":1,"deleted":false},"children":[]}
          ]}
        ]}
        """
        return try JSONDecoder().decode(DocTree.self, from: Data(json.utf8))
    }

    @Test func alertsPlanFromEveryWorkspaceAndActionsWriteToTheirList() async throws {
        let center = FakeDueAlertCenter()
        let tz = TimeZone(identifier: "Europe/Dublin")!
        let now = Date(timeIntervalSince1970: 1_790_000_000)
        let c = NotificationCoordinator(center: center, now: { now }, timeZone: { tz })
        let cache = try Cache.inMemory()
        try await cache.replaceTree([
            DocSummary(id: "t1", parentID: nil, title: "To-do", currentEpoch: 1),
            DocSummary(id: "t2", parentID: nil, title: "To-do", currentEpoch: 1, workspaceID: "w2"),
        ])
        try await cache.storeDoc(todo("t1", "unsorted thing"))
        try await cache.storeDoc(todo("t2", "home thing"))
        // offline: plans from every cached list
        c.connect(cache: cache, api: nil, sync: nil)
        await c.reconcile()
        #expect(Set(center.pendingByID.values.map(\.title)) == ["unsorted thing", "home thing"])
        #expect(Set(c.lists.values) == [.unsorted, .id("w2")])

        let homeID = try #require(c.lists.first { $0.value == .id("w2") }?.key)
        let parts = homeID.split(separator: "/").map(String.init)
        await center.onAction?(.done, parts[0], parts[1])
        let queued = try await cache.pendingOutbox()
        let body = try #require(queued.last?.body)
        let obj = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
        #expect(queued.last?.path == "/api/todo/toggle" && obj["workspace"] as? String == "w2")
    }

    @Test func dueItemsReadWithoutAWorkspaceNameTheirList() throws {
        let json = #"{"items":[{"date":"2026-09-21","id":"0-a","text":"a","deadline":"2026-09-22","doc_id":"t2","workspace_id":"w2"},{"date":"2026-09-21","id":"0-b","text":"b","deadline":"2026-09-22","doc_id":"t1","workspace_id":null}],"doc_id":null,"epoch":0,"now":"2026-10-01T00:00:00Z","default_alert_time":"09:00"}"#
        let list = try JSONDecoder().decode(TodoDueList.self, from: Data(json.utf8))
        #expect(list.items.map(\.workspaceID) == ["w2", nil] && list.items.map(\.docID) == ["t2", "t1"])
        let legacy = try JSONDecoder().decode(TodoDueList.self, from: Data(#"{"items":[{"date":"2026-09-21","id":"0-a","text":"a","deadline":"2026-09-22"}],"doc_id":null,"epoch":0,"now":"2026-10-01T00:00:00Z","default_alert_time":"09:00"}"#.utf8))
        #expect(legacy.items.first?.docID == nil, "a daemon without workspaces: the legacy list")
    }

    @Test func offlineSearchKeepsTheCurrentWorkspace() {
        let index = DocIndex(docs)
        let ids = ["loose", "wroot", "hkid"]
        #expect(ids.filter { WorkspaceFilter.keeps(index.byID[$0], scope: .id("w1")) } == ["wroot"])
        #expect(ids.filter { WorkspaceFilter.keeps(index.byID[$0], scope: .unsorted) } == ["loose"])
        #expect(ids.filter { WorkspaceFilter.keeps(index.byID[$0], scope: nil) } == ids, "Everywhere")
    }
}
