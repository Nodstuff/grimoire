import Foundation
import Testing
import TaisceKit
@testable import Taisce

private let tree = [
    DocInfo(id: "answers", title: "Answers", workspaceID: "w1"),
    DocInfo(id: "a2", parentID: "answers", title: "Second", sortKey: "b", workspaceID: "w1"),
    DocInfo(id: "a1", parentID: "answers", title: "First", sortKey: "a", workspaceID: "w1"),
    // labelled into another workspace: still inside the opened doc
    DocInfo(id: "a3", parentID: "answers", title: "Elsewhere", sortKey: "c", workspaceID: "w2"),
    DocInfo(id: "leaf", title: "Leaf", workspaceID: "w1"),
]

@Suite struct DocChildrenTests {
    let index = DocIndex(tree)

    @Test func childrenInTreeOrderAcrossWorkspaces() {
        let kids = DocChildrenLayout.children(of: "answers", index: index, meta: [:])
        #expect(kids.map(\.id) == ["a1", "a2", "a3"], "never filtered by workspace")
        #expect(kids.first?.folder == "Answers")
    }

    @Test func emptyBodyWithChildrenIsAFolder() {
        let kids = DocChildrenLayout.children(of: "answers", index: index, meta: [:])
        #expect(DocChildrenLayout.make(pageEmpty: true, children: kids) == .folder(kids))
    }

    @Test func bodyAndChildrenGetASection() {
        let kids = DocChildrenLayout.children(of: "answers", index: index, meta: [:])
        #expect(DocChildrenLayout.make(pageEmpty: false, children: kids) == .section(kids))
    }

    @Test func aLeafShowsNeither() {
        let kids = DocChildrenLayout.children(of: "leaf", index: index, meta: [:])
        #expect(kids.isEmpty)
        #expect(DocChildrenLayout.make(pageEmpty: true, children: kids) == DocChildrenLayout.none)
        #expect(DocChildrenLayout.make(pageEmpty: false, children: kids) == DocChildrenLayout.none)
        #expect(DocChildrenLayout.make(pageEmpty: nil, children: DocChildrenLayout.children(of: "answers", index: index, meta: [:])) == DocChildrenLayout.none, "still loading")
    }
}

@Suite struct WorkspaceDeleteLocalTests {
    @Test func deletedWorkspacesDocsReResolveAtOnce() async throws {
        let cache = try Cache.inMemory()
        try await cache.replaceTree([
            DocSummary(id: "outer", parentID: nil, title: "Outer", currentEpoch: 1, workspaceID: "wOuter"),
            DocSummary(id: "inner", parentID: "outer", title: "Inner", currentEpoch: 1, workspaceID: "wDel"),
            DocSummary(id: "innerKid", parentID: "inner", title: "Kid", currentEpoch: 1, workspaceID: "wDel"),
            DocSummary(id: "root", parentID: nil, title: "Root", currentEpoch: 1, workspaceID: "wDel"),
            DocSummary(id: "rootKid", parentID: "root", title: "Root kid", currentEpoch: 1, workspaceID: "wDel"),
            DocSummary(id: "other", parentID: nil, title: "Other", currentEpoch: 1, workspaceID: "wKeep"),
        ])
        try await cache.replaceWorkspaces([Workspace(id: "wDel", name: "Work"), Workspace(id: "wKeep", name: "Home")])
        let moved = try await cache.clearWorkspace("wDel")
        #expect(moved == 4)
        let ws = Dictionary(uniqueKeysWithValues: try await cache.docs().map { ($0.id, $0.workspaceID) })
        #expect(ws["root"] == .some(nil) && ws["rootKid"] == .some(nil), "to Unsorted")
        #expect(ws["inner"] == "wOuter" && ws["innerKid"] == "wOuter", "to the outer workspace")
        #expect(ws["other"] == "wKeep" && ws["outer"] == "wOuter", "others untouched")
        #expect(try await cache.workspaces().map(\.id) == ["wKeep"])
        // and the app's filter sees them in Unsorted straight away
        let docs = try await cache.docs().map(DocInfo.init)
        #expect(WorkspaceFilter.docs(docs, in: .unsorted).map(\.id).sorted() == ["root", "rootKid"])
    }
}
