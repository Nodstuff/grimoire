import Foundation

/// The editor → gate diff, a port of the web editor's `ui/src/editor/diff.ts`:
/// compare the surface's top-level entries against the loaded baseline and
/// emit block ops. Structure rules mirror the importer: nesting is derived
/// from heading levels; order keys are per-sibling-list base-36 fractions.
///
/// Both ports read the shared vectors in `ui/src/editor/diff.vectors.json`
/// (`DocDiffTests` here, `diff.vectors.test.ts` there), so for the same
/// baseline and entries they produce the same ops.
public enum DocDiff {
    public struct BaselineBlock: Hashable, Sendable, Codable {
        public var id: BlockID
        /// the comparison form: the block as the surface writes it
        public var content: String
        public var parent: BlockID?
        public var orderKey: String

        public init(id: BlockID, content: String, parent: BlockID?, orderKey: String) {
            self.id = id
            self.content = content
            self.parent = parent
            self.orderKey = orderKey
        }

        enum CodingKeys: String, CodingKey {
            case id, content, parent
            case orderKey = "order_key"
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = try c.decode(BlockID.self, forKey: .id)
            content = try c.decode(String.self, forKey: .content)
            parent = try c.decodeIfPresent(BlockID.self, forKey: .parent)
            orderKey = try c.decode(String.self, forKey: .orderKey)
        }
    }

    public struct Entry: Hashable, Sendable, Codable {
        /// nil (or an id the baseline doesn't have) = new: an insert
        public var id: BlockID?
        public var content: String
        /// heading level when the entry IS a heading, else 0
        public var level: Int

        public init(id: BlockID?, content: String, level: Int) {
            self.id = id
            self.content = content
            self.level = level
        }
    }

    static let digits = Array("0123456789abcdefghijklmnopqrstuvwxyz")
    static let mid: Character = digits[18] // "i"

    static func digit(_ c: Character) -> Int { digits.firstIndex(of: c) ?? 0 }

    /// A base-36 fractional key strictly between `a` and `b` (nil = open
    /// end), exactly as diff.ts's `keyBetween`: when the bounds are inverted
    /// or equal the answer is `a` + a mid digit. Never ends in "0".
    /// (`OrderKey.between` is the store's, with its own fallback for bad
    /// bounds; this one keeps the two editors' ops identical.)
    public static func keyBetween(_ a: String?, _ b: String?) -> String {
        if let a, let b, a >= b { return a + String(mid) }
        let av = Array(a ?? "")
        let bv = b.map(Array.init)
        var out = ""
        var i = 0
        while true {
            let da = i < av.count ? digit(av[i]) : 0
            let db = bv.map { i < $0.count ? digit($0[i]) : 0 } ?? 36
            if da == db {
                out.append(digits[da])
                i += 1
                continue
            }
            if db - da > 1 { return out + String(digits[(da + db) >> 1]) }
            out.append(digits[da])
            i += 1
            while true {
                let d = i < av.count ? digit(av[i]) : 0
                if 36 - d > 1 { return out + String(digits[(d + 36) >> 1]) }
                out.append(digits[d])
                i += 1
            }
        }
    }

    /// diff.ts `inferBlockType`.
    public static func inferBlockType(_ content: String) -> BlockType {
        if content.hasPrefix("```mermaid") { return .diagramMermaid }
        if content.hasPrefix("```d2") { return .diagramD2 }
        if content.hasPrefix("```") || content.hasPrefix("---") { return .code }
        if isHeading(content) { return .heading }
        if content.hasPrefix("DECISION:") { return .decision }
        return .paragraph
    }

    /// `/^#{1,6} /`
    static func isHeading(_ s: String) -> Bool {
        let hashes = s.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes) else { return false }
        return s.dropFirst(hashes).first == " "
    }

    private struct Placed {
        var entry: Entry
        var parent: BlockID?
        var tempID: BlockID
    }

    /// The ops turning `baseline` into `entries` (diff.ts `computeOps`).
    public static func computeOps(
        baseline: [BaselineBlock], entries: [Entry],
        newID: () -> BlockID = { UUID().uuidString.lowercased() }
    ) -> [BlockOp] {
        let old = Dictionary(baseline.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        var ops: [BlockOp] = []

        // 1. deletes: baseline ids that vanished
        let live = Set(entries.compactMap(\.id))
        for b in baseline where !live.contains(b.id) {
            ops.append(.delete(target: b.id))
        }

        // 2. parents via the importer's heading-stack rule
        var placed: [Placed] = []
        var stack: [(level: Int, id: BlockID)] = []
        for e in entries {
            if e.level > 0 {
                while let top = stack.last, top.level >= e.level { stack.removeLast() }
            }
            let parent = stack.last?.id
            let temp = e.id ?? newID()
            placed.append(Placed(entry: e, parent: parent, tempID: temp))
            if e.level > 0 { stack.append((e.level, temp)) }
        }

        // 3. group by parent (first-appearance order), keep a greedy
        //    increasing run of undisturbed survivors, key the rest between
        //    their neighbours
        var order: [BlockID?] = []
        var groups: [BlockID?: [Placed]] = [:]
        for p in placed {
            if groups[p.parent] == nil {
                order.append(p.parent)
                groups[p.parent] = []
            }
            groups[p.parent]!.append(p)
        }

        for parentKey in order {
            let group = groups[parentKey]!
            // pass 1: stable entries (id exists, parent unchanged, keys increasing)
            var stable = Array(repeating: false, count: group.count)
            var lastKey = ""
            for (i, p) in group.enumerated() {
                guard let id = p.entry.id, let prev = old[id], prev.parent == p.parent else { continue }
                if prev.orderKey > lastKey {
                    stable[i] = true
                    lastKey = prev.orderKey
                }
            }
            // pass 2: keys and ops
            var keys: [String?] = group.indices.map { stable[$0] ? old[group[$0].entry.id!]!.orderKey : nil }
            for (i, p) in group.enumerated() {
                let prevKey = i > 0 ? keys[i - 1] : nil
                if keys[i] == nil {
                    let next = keys[(i + 1)...].first { $0 != nil } ?? nil
                    keys[i] = keyBetween(prevKey, next)
                }
                let key = keys[i]!
                guard let id = p.entry.id, let prev = old[id] else {
                    ops.append(.insert(blockID: p.tempID, parentID: p.parent, orderKey: key,
                                       type: inferBlockType(p.entry.content), content: p.entry.content))
                    continue
                }
                if !stable[i] || prev.parent != p.parent {
                    ops.append(.move(target: id, newParent: p.parent, newOrderKey: key))
                }
                if prev.content != p.entry.content {
                    ops.append(.replace(target: id, content: p.entry.content))
                }
            }
        }
        return ops
    }

    /// After a save the written entries ARE the new baseline (DocEditor.tsx
    /// `rebuildBaseline`).
    public static func rebuildBaseline(_ old: [BaselineBlock], entries: [Entry], ops: [BlockOp]) -> [BaselineBlock] {
        let byID = Dictionary(old.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        var inserts: [BlockID: (parent: BlockID?, key: String)] = [:]
        var insertOrder: [(id: BlockID, parent: BlockID?, key: String)] = []
        var moves: [BlockID: (parent: BlockID?, key: String)] = [:]
        for op in ops {
            switch op {
            case let .insert(id?, parent, key, _, _):
                inserts[id] = (parent, key)
                insertOrder.append((id, parent, key))
            case let .move(target, parent, key):
                moves[target] = (parent, key)
            default: break
            }
        }
        var cursor = 0
        var out: [BaselineBlock] = []
        for e in entries {
            if let id = e.id, let prev = byID[id] {
                let mv = moves[id]
                out.append(BaselineBlock(id: id, content: e.content, parent: mv.map(\.parent) ?? prev.parent, orderKey: mv?.key ?? prev.orderKey))
            } else if let id = e.id, let ins = inserts[id] {
                // an entry that carried its own new id
                out.append(BaselineBlock(id: id, content: e.content, parent: ins.parent, orderKey: ins.key))
            } else if e.id == nil {
                // nil-id entries take the minted inserts in order
                while cursor < insertOrder.count, entries.contains(where: { $0.id == insertOrder[cursor].id }) { cursor += 1 }
                guard cursor < insertOrder.count else { continue }
                let ins = insertOrder[cursor]
                cursor += 1
                out.append(BaselineBlock(id: ins.id, content: e.content, parent: ins.parent, orderKey: ins.key))
            }
        }
        return out
    }
}
