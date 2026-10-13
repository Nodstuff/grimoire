import Foundation
import Testing
@testable import TaisceKit

/// Our edits the gate parked, in an open editor: shown as ours, saved only
/// on the conflict path, and gone (or kept) once a human decides.
@Suite struct ParkedEditsTests {
    static func b(_ id: String, _ content: String, key: String) -> Block {
        Block(id: id, docID: "d", parentID: nil, orderKey: key, blockType: .paragraph, content: content)
    }

    static let server = [b("a", "alpha", key: "i"), b("b", "beta", key: "r")]
    /// what the editor holds after saving "beta!" (parked at epoch 3)
    static let parked = [ParkedOp(kind: .replace(target: "b", content: "beta!"), baseEpoch: 3, opID: "op1")]

    struct State {
        var local: [(id: BlockID, content: EditorBlockContent)]
        var baseline: [DocDiff.BaselineBlock]
        var conflicts: [BlockID: SurfaceConflict] = [:]
        var parked: Set<BlockID> = []

        /// One refresh: the server's blocks, then what is still in review.
        mutating func refresh(_ server: [Block], parked ops: [ParkedOp], epoch: Int) -> SurfaceMerge.Result {
            var fresh = DocEditor(docID: "d", baseEpoch: epoch, blocks: server)
            parked = ParkedEdits.apply(ops, on: &fresh, conflicts: &conflicts, parkedBefore: parked)
            let r = SurfaceMerge.merge(local: local, baseline: baseline, conflicts: conflicts, oldEpoch: epoch - 1,
                                       server: fresh.ordered().map(\.block), parked: parked)
            baseline = r.baseline
            conflicts = r.conflicts
            local = r.blocks.map { ($0.id, $0.content) }
            return r
        }

        /// The ops a save would send on the normal path (not in conflict).
        func normalOps() -> [BlockOp] {
            let entries = local.map { DocDiff.Entry(id: $0.id, content: $0.content.markdown, level: 0) }
            return DocDiff.computeOps(baseline: baseline, entries: entries).filter { op in op.target.map { conflicts[$0] == nil } ?? true }
        }
    }

    static func start() -> State {
        // saved "beta!": the baseline is what we wrote
        State(local: [("a", .paragraph(AttributedString("alpha"))), ("b", .paragraph(AttributedString("beta!")))],
              baseline: [DocDiff.BaselineBlock(id: "a", content: "alpha", parent: nil, orderKey: "i"),
                         DocDiff.BaselineBlock(id: "b", content: "beta!", parent: nil, orderKey: "r")])
    }

    @Test func aParkedEditStaysOursAndInConflict() {
        var st = Self.start()
        let r = st.refresh(Self.server, parked: Self.parked, epoch: 4)
        #expect(!r.changed, "still shows our parked text")
        #expect(st.local.map { $0.content.markdown } == ["alpha", "beta!"])
        #expect(st.conflicts["b"]?.base == 3, "later saves of b go on the parked propose's epoch")
        // a keystroke in it: never on the normal path
        st.local[1].content = .paragraph(AttributedString("beta!?"))
        #expect(st.normalOps().isEmpty)
        // another refresh while it waits changes nothing
        _ = st.refresh(Self.server, parked: Self.parked, epoch: 5)
        #expect(st.conflicts["b"]?.base == 3)
        #expect(st.local.map { $0.content.markdown } == ["alpha", "beta!?"])
    }

    @Test func rejectedTheServersTextComesBack() {
        var st = Self.start()
        _ = st.refresh(Self.server, parked: Self.parked, epoch: 4)
        // rejected: out of the queue, the server still says "beta"
        let r = st.refresh(Self.server, parked: [], epoch: 5)
        #expect(r.changed)
        #expect(st.local.map { $0.content.markdown } == ["alpha", "beta"])
        #expect(st.conflicts.isEmpty)
    }

    @Test func acceptedItIsJustText() {
        var st = Self.start()
        _ = st.refresh(Self.server, parked: Self.parked, epoch: 4)
        let accepted = [Self.b("a", "alpha", key: "i"), Self.b("b", "beta!", key: "r")]
        let r = st.refresh(accepted, parked: [], epoch: 5)
        #expect(!r.changed)
        #expect(st.conflicts.isEmpty)
        st.local[1].content = .paragraph(AttributedString("beta!!"))
        #expect(st.normalOps() == [.replace(target: "b", content: "beta!!")], "back on the normal path")
    }

    @Test func rejectedAfterMoreTypingIsAConflictToResolve() {
        var st = Self.start()
        _ = st.refresh(Self.server, parked: Self.parked, epoch: 4)
        st.local[1].content = .paragraph(AttributedString("beta!? unsaved"))
        _ = st.refresh(Self.server, parked: [], epoch: 5)
        #expect(st.local.map { $0.content.markdown } == ["alpha", "beta!? unsaved"], "typed text is never dropped")
        #expect(st.conflicts["b"]?.theirs == "beta", "Keep mine / Take theirs")
    }

    @Test func aParkedInsertStaysAndIsInConflict() {
        var st = Self.start()
        st.local.append(("n", .paragraph(AttributedString("new"))))
        st.baseline.append(DocDiff.BaselineBlock(id: "n", content: "new", parent: nil, orderKey: "v"))
        let ins = [ParkedOp(kind: .insert(blockID: "n", parentID: nil, orderKey: "v", type: .paragraph, content: "new"), baseEpoch: 3, opID: "op2")]
        _ = st.refresh([Self.b("a", "alpha", key: "i"), Self.b("b", "beta!", key: "r")], parked: ins, epoch: 4)
        #expect(st.local.map(\.id) == ["a", "b", "n"])
        #expect(st.conflicts["n"]?.base == 3)
        // rejected: it goes
        _ = st.refresh([Self.b("a", "alpha", key: "i"), Self.b("b", "beta!", key: "r")], parked: [], epoch: 5)
        #expect(st.local.map(\.id) == ["a", "b"])
    }
}
