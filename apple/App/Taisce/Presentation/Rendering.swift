import Foundation
import Synchronization
import TaisceKit

/// Rendered nodes by (block id, content hash): a sync write that touches
/// one block re-parses that block, not the doc. Thread-safe; parsing runs
/// off the main actor, so this is shared across detached tasks.
final class RenderCache: Sendable {
    struct Key: Hashable, Sendable {
        var id: BlockID
        var type: String
        var content: Int
    }

    private let store = Mutex<[Key: [RenderNode]]>([:])
    /// beyond this many entries the cache starts over (a long session
    /// across many docs; re-parsing is the fallback, not an error)
    let limit: Int

    init(limit: Int = 4000) {
        self.limit = limit
    }

    var count: Int { store.withLock { $0.count } }

    func nodes(for block: Block) -> [RenderNode] {
        let key = Key(id: block.id, type: block.blockType.rawValue, content: block.content.hashValue)
        if let hit = store.withLock({ $0[key] }) { return hit }
        let nodes = BlockRenderer.render(block)
        store.withLock { cache in
            if cache.count >= limit { cache.removeAll(keepingCapacity: true) }
            cache[key] = nodes
        }
        return nodes
    }
}

/// Pure counts over the render model (not view statics: these run in
/// tests and background work, off the main actor).
enum RenderMetrics {
    /// Task checkboxes inside `node`, pre-order, as they appear in the markdown.
    static func checkboxCount(_ node: RenderNode) -> Int {
        switch node {
        case let .list(_, _, items):
            items.reduce(0) { $0 + ($1.checked == nil ? 0 : 1) + checkboxCount($1.children) }
        case let .quote(_, children):
            checkboxCount(children)
        default:
            0
        }
    }

    static func checkboxCount(_ nodes: [RenderNode]) -> Int {
        nodes.reduce(0) { $0 + checkboxCount($1) }
    }

    /// Where each node's checkboxes start, given the first index.
    static func checkboxOffsets(_ nodes: [RenderNode], base: Int = 0) -> [Int] {
        var out: [Int] = []
        var n = base
        for node in nodes {
            out.append(n)
            n += checkboxCount(node)
        }
        return out
    }

    /// The checkbox index of each list item, given the list's first index.
    static func itemOffsets(_ items: [RenderNode.ListItem], base: Int = 0) -> [Int] {
        var out: [Int] = []
        var n = base
        for item in items {
            out.append(n)
            n += (item.checked == nil ? 0 : 1) + checkboxCount(item.children)
        }
        return out
    }
}
