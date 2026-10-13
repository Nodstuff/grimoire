import Foundation
import Testing
@testable import TaisceKit

/// A server change merged into an open surface.
@Suite struct SurfaceMergeTests {
    static func b(_ id: String, _ content: String, key: String) -> Block {
        Block(id: id, docID: "d", parentID: nil, orderKey: key, blockType: .paragraph, content: content)
    }

    static let server0 = [b("a", "alpha", key: "i"), b("b", "beta", key: "m"), b("c", "gamma", key: "r")]
    static var baseline: [DocDiff.BaselineBlock] { Surface.load(DocEditor(docID: "d", baseEpoch: 1, blocks: server0)).baseline }

    static func local(_ pairs: [(String, String)]) -> [(id: BlockID, content: EditorBlockContent)] {
        pairs.map { ($0.0, EditorBlockContent.parse(markdown: $0.1)) }
    }

    @Test func nothingChangedIsNoRebuild() {
        let r = SurfaceMerge.merge(local: Self.local([("a", "alpha"), ("b", "beta"), ("c", "gamma")]), baseline: Self.baseline,
                                   conflicts: [:], oldEpoch: 1, server: Self.server0)
        #expect(!r.changed)
        #expect(r.blocks.allSatisfy { $0.local })
    }

    @Test func aChangeToAnUntouchedBlockIsTaken() {
        let server = [Self.b("a", "alpha", key: "i"), Self.b("b", "BETA", key: "m"), Self.b("c", "gamma", key: "r")]
        let r = SurfaceMerge.merge(local: Self.local([("a", "alpha!"), ("b", "beta"), ("c", "gamma")]), baseline: Self.baseline,
                                   conflicts: [:], oldEpoch: 1, server: server)
        #expect(r.changed)
        #expect(r.blocks.map(\.local) == [true, false, true])
        #expect(r.blocks[1].content.markdown == "BETA")
        #expect(r.conflicts.isEmpty)
        // the unsaved edit to a still diffs as ours
        let entries = r.blocks.map { DocDiff.Entry(id: $0.id, content: $0.content.markdown, level: 0) }
        #expect(DocDiff.computeOps(baseline: r.baseline, entries: entries) == [.replace(target: "a", content: "alpha!")])
    }

    @Test func aChangeUnderAnEditIsAConflictOnTheOlderEpoch() {
        let server = [Self.b("a", "ALPHA", key: "i"), Self.b("b", "beta", key: "m"), Self.b("c", "gamma", key: "r")]
        let r = SurfaceMerge.merge(local: Self.local([("a", "alpha!"), ("b", "beta"), ("c", "gamma")]), baseline: Self.baseline,
                                   conflicts: [:], oldEpoch: 3, server: server)
        #expect(r.blocks[0].local && r.blocks[0].content.markdown == "alpha!")
        #expect(r.conflicts["a"] == SurfaceConflict(base: 3, theirs: "ALPHA"))
        #expect(!r.changed)
    }

    @Test func aConflictEndsWhenTheServerShowsOurs() {
        let server = [Self.b("a", "alpha!", key: "i"), Self.b("b", "beta", key: "m"), Self.b("c", "gamma", key: "r")]
        let r = SurfaceMerge.merge(local: Self.local([("a", "alpha!"), ("b", "beta"), ("c", "gamma")]), baseline: Self.baseline,
                                   conflicts: ["a": SurfaceConflict(base: 1, theirs: "x")], oldEpoch: 2, server: server)
        #expect(r.conflicts.isEmpty)
    }

    @Test func newRemoteBlocksArriveInPlace() {
        let server = [Self.b("a", "alpha", key: "i"), Self.b("n", "new", key: "k"), Self.b("b", "beta", key: "m"), Self.b("c", "gamma", key: "r")]
        let r = SurfaceMerge.merge(local: Self.local([("a", "alpha"), ("x", "typed here"), ("b", "beta"), ("c", "gamma")]), baseline: Self.baseline,
                                   conflicts: [:], oldEpoch: 1, server: server)
        #expect(r.blocks.map(\.id) == ["a", "x", "n", "b", "c"])
        #expect(r.changed)
        #expect(r.baseline.map(\.id) == ["a", "n", "b", "c"])
    }

    @Test func aDeletedUntouchedBlockGoesAndAnEditedOneComesBackNew() {
        let server = [Self.b("a", "alpha", key: "i")]
        var n = 0
        let r = SurfaceMerge.merge(local: Self.local([("a", "alpha"), ("b", "beta edited"), ("c", "gamma")]), baseline: Self.baseline,
                                   conflicts: [:], oldEpoch: 1, server: server) { n += 1; return "fresh\(n)" }
        #expect(r.blocks.map(\.id) == ["a", "fresh1"])
        #expect(r.blocks[1].from == "b")
        #expect(r.baseline.map(\.id) == ["a"])
        let entries = r.blocks.map { DocDiff.Entry(id: $0.id, content: $0.content.markdown, level: 0) }
        let ops = DocDiff.computeOps(baseline: r.baseline, entries: entries)
        #expect(ops.count == 1)
        if case let .insert(id, _, _, _, content) = ops.first { #expect(id == "fresh1" && content == "beta edited") } else { Issue.record("want an insert") }
    }

    @Test func aRemoteReorderReordersTheSurface() {
        let server = [Self.b("c", "gamma", key: "a"), Self.b("a", "alpha", key: "i"), Self.b("b", "beta", key: "m")]
        let r = SurfaceMerge.merge(local: Self.local([("a", "alpha"), ("b", "beta"), ("c", "gamma")]), baseline: Self.baseline,
                                   conflicts: [:], oldEpoch: 1, server: server)
        #expect(r.changed)
        #expect(r.blocks.map(\.id) == ["c", "a", "b"])
        let entries = r.blocks.map { DocDiff.Entry(id: $0.id, content: $0.content.markdown, level: 0) }
        #expect(DocDiff.computeOps(baseline: r.baseline, entries: entries).isEmpty, "the reorder is not sent back")
    }

    @Test func aLocalDeleteStaysDeleted() {
        let r = SurfaceMerge.merge(local: Self.local([("a", "alpha"), ("c", "gamma")]), baseline: Self.baseline,
                                   conflicts: [:], oldEpoch: 1, server: Self.server0)
        #expect(r.blocks.map(\.id) == ["a", "c"])
        let entries = r.blocks.map { DocDiff.Entry(id: $0.id, content: $0.content.markdown, level: 0) }
        #expect(DocDiff.computeOps(baseline: r.baseline, entries: entries) == [.delete(target: "b")])
    }
}
