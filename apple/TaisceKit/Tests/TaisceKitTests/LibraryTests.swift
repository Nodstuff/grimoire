import Foundation
import Testing
@testable import TaisceKit

/// What the app's views compute, from the states a first launch can be in.
@Suite struct LibraryTests {
    @Test func emptyLibrary() async throws {
        let cache = try Cache.inMemory()
        let docs = try await cache.docs()
        #expect(docs.isEmpty && DocTreeNode.build(docs).isEmpty)
        #expect(Library.doc(titled: "Anything", in: docs) == nil && Library.todoDocID(in: docs) == nil)
        // Today with no To-do doc
        let todos = try await cache.todos()
        #expect(todos.isEmpty && TodoParser.dueOrOverdue(todos).isEmpty)
        // a doc view for a missing doc
        #expect(BlockRenderer.render(try await cache.blocks(of: "missing")).isEmpty)
        #expect(try await cache.editor(for: "missing") == nil)
    }

    @Test func singleDocBeforeAndAfterItsBodyArrives() async throws {
        let cache = try Cache.inMemory()
        try await cache.replaceTree([DocSummary(id: "d1", parentID: nil, title: "Only", currentEpoch: 1)])
        let docs = try await cache.docs()
        let tree = DocTreeNode.build(docs)
        #expect(tree.map(\.id) == ["d1"] && tree.first?.children == nil)
        #expect(Library.doc(titled: "Only", in: docs)?.id == "d1")
        // body not fetched yet: renders nothing, is stale (the view fetches)
        #expect(BlockRenderer.render(try await cache.blocks(of: "d1")).isEmpty)
        #expect(try await cache.doc("d1")?.isBodyStale == true)
        // an empty body renders nothing too
        try await cache.storeDoc(DocTree(doc: DocSummary(id: "d1", parentID: nil, title: "Only", currentEpoch: 1), roots: []))
        #expect(BlockRenderer.render(try await cache.blocks(of: "d1")).isEmpty)
        #expect(try await cache.doc("d1")?.isBodyStale == false)
    }

    @Test func orphansShowAtTheRootAndCyclesDoNotHang() {
        func rec(_ id: String, _ parent: String?, _ title: String) -> DocRecord {
            DocRecord(DocSummary(id: id, parentID: parent, title: title, currentEpoch: 1), bodyEpoch: nil)
        }
        let tree = DocTreeNode.build([
            rec("a", nil, "A"), rec("b", "a", "B"), rec("o", "gone", "Orphan"),
            rec("x", "y", "X"), rec("y", "x", "Y"),
        ])
        #expect(tree.map(\.id) == ["a", "o", "x"])
        #expect(tree.last?.children?.map(\.id) == ["y"])
        #expect(tree.first?.children?.map(\.id) == ["b"])
        #expect(Library.doc(titled: "A/B", in: [rec("b", "a", "B")])?.id == "b")
    }
}
