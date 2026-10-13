import Foundation
import Testing
@testable import TaisceKit

/// The doc diff (port of ui/src/editor/diff.ts). The shared vectors in
/// ui/src/editor/diff.vectors.json are generated from diff.ts and read by
/// both test suites, so the two editors emit the same ops.
@Suite struct DocDiffTests {
    struct Vectors: Decodable {
        struct Case: Decodable {
            var name: String
            var baseline: [DocDiff.BaselineBlock]
            var entries: [DocDiff.Entry]
            var hidden: [DocDiff.HiddenBlock]?
            var ops: [BlockOp]
        }
        var cases: [Case]
    }

    static func vectors() throws -> Vectors {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // TaisceKitTests
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // TaisceKit
            .deletingLastPathComponent() // apple
            .deletingLastPathComponent() // repo root
            .appendingPathComponent("ui/src/editor/diff.vectors.json")
        return try JSONDecoder().decode(Vectors.self, from: Data(contentsOf: url))
    }

    @Test func sharedVectorsMatchTheWebEditor() throws {
        let v = try Self.vectors()
        #expect(v.cases.count > 10)
        for c in v.cases {
            var n = 0
            let ops = DocDiff.computeOps(baseline: c.baseline, entries: c.entries, hidden: c.hidden ?? []) {
                n += 1
                return "new-\(n)"
            }
            #expect(ops == c.ops, "\(c.name)")
            var k = 0
            let want = c.entries.map { e -> BlockID in
                if let id = e.id { return id }
                k += 1
                return "new-\(k)"
            }
            let hidden = c.hidden ?? []
            let full = DocDiff.withHiddenBlocks(c.baseline, want.map { DocDiff.Entry(id: $0, content: "", level: 0) }, hidden).1.compactMap(\.id)
            #expect(Self.serverOrder(c.baseline + hidden.map(\.plain), ops) == full, "the server tree reads in the editor's order: \(c.name)")
        }
    }

    /// The server's blocks after `ops`, read in pre-order (siblings by key).
    static func serverOrder(_ baseline: [DocDiff.BaselineBlock], _ ops: [BlockOp]) -> [BlockID] {
        var blocks: [BlockID: (parent: BlockID?, key: String)] = [:]
        for b in baseline { blocks[b.id] = (b.parent, b.orderKey) }
        for op in ops {
            switch op {
            case let .delete(t): blocks[t] = nil
            case let .insert(id?, parent, key, _, _): blocks[id] = (parent, key)
            case let .move(t, parent, key): blocks[t] = (parent, key)
            default: break
            }
        }
        var kids: [BlockID?: [BlockID]] = [:]
        for (id, b) in blocks {
            let p = b.parent.flatMap { blocks[$0] != nil ? $0 : nil }
            kids[p, default: []].append(id)
        }
        var out: [BlockID] = []
        func walk(_ p: BlockID?) {
            for id in (kids[p] ?? []).sorted(by: { (blocks[$0]!.key, $0) < (blocks[$1]!.key, $1) }) {
                out.append(id)
                walk(id)
            }
        }
        walk(nil)
        return out
    }

    // MARK: keyBetween (diff.ts's vectors)

    @Test func keyBetweenMatchesDiffTs() {
        let vectors: [(String?, String?, String)] = [
            (nil, nil, "i"), ("i", nil, "r"), (nil, "i", "9"), ("i", "r", "m"), ("i", "j", "ii"), ("z", nil, "zi"),
            ("i", "i", "ii"), ("zz", "zz", "zzi"), ("ii", "ij", "iii"), ("iz", "j", "izi"), (nil, "1", "0i"),
        ]
        for (a, b, want) in vectors {
            #expect(DocDiff.keyBetween(a, b) == want, "keyBetween(\(a ?? "nil"), \(b ?? "nil"))")
        }
        let k = DocDiff.keyBetween("r", "i")
        #expect(k > "r" && !k.hasSuffix("0"))
    }

    @Test func keyBetweenStaysInsideItsBounds() {
        var hi: String? = "i"
        var prev = "i"
        for _ in 0..<500 {
            let k = DocDiff.keyBetween(nil, hi)
            #expect(k < prev && !k.hasSuffix("0"))
            prev = k
            hi = k
        }
        var lo = "i"
        for _ in 0..<500 {
            let k = DocDiff.keyBetween(lo, "j")
            #expect(k > lo && k < "j")
            lo = k
        }
    }

    @Test func inferredTypes() {
        #expect(DocDiff.inferBlockType("```mermaid\nx\n```") == .diagramMermaid)
        #expect(DocDiff.inferBlockType("```d2\nx\n```") == .diagramD2)
        #expect(DocDiff.inferBlockType("```\nx\n```") == .code)
        #expect(DocDiff.inferBlockType("## Two") == .heading)
        #expect(DocDiff.inferBlockType("####### seven") == .paragraph)
        #expect(DocDiff.inferBlockType("#tag") == .paragraph)
        #expect(DocDiff.inferBlockType("DECISION: x") == .decision)
    }

    // MARK: rebuildBaseline

    @Test func savedEntriesBecomeTheBaseline() {
        let base = [
            DocDiff.BaselineBlock(id: "h", content: "# H", parent: nil, orderKey: "i"),
            DocDiff.BaselineBlock(id: "a", content: "a", parent: "h", orderKey: "i"),
            DocDiff.BaselineBlock(id: "b", content: "b", parent: "h", orderKey: "r"),
        ]
        let entries = [
            DocDiff.Entry(id: "h", content: "# H", level: 1),
            DocDiff.Entry(id: "b", content: "b2", level: 0),
            DocDiff.Entry(id: nil, content: "new", level: 0),
            DocDiff.Entry(id: "minted", content: "mine", level: 0),
        ]
        var n = 0
        let ops = DocDiff.computeOps(baseline: base, entries: entries) { n += 1; return "new-\(n)" }
        let next = DocDiff.rebuildBaseline(base, entries: entries, ops: ops)
        #expect(next.map(\.id) == ["h", "b", "new-1", "minted"])
        #expect(next.map(\.content) == ["# H", "b2", "new", "mine"])
        #expect(next.allSatisfy { $0.id == "h" ? $0.parent == nil : $0.parent == "h" })
        // a second diff against it is empty
        #expect(DocDiff.computeOps(baseline: next, entries: [
            DocDiff.Entry(id: "h", content: "# H", level: 1),
            DocDiff.Entry(id: "b", content: "b2", level: 0),
            DocDiff.Entry(id: "new-1", content: "new", level: 0),
            DocDiff.Entry(id: "minted", content: "mine", level: 0),
        ]).isEmpty)
    }
}
