import Foundation

/// One of our ops the gate parked (red, not applied), still in review.
public struct ParkedOp: Hashable, Sendable {
    public var kind: BlockOp
    /// the epoch its propose was made on
    public var baseEpoch: Int
    public var opID: String

    public init(kind: BlockOp, baseEpoch: Int, opID: String) {
        self.kind = kind
        self.baseEpoch = baseEpoch
        self.opID = opID
    }

    /// the block it writes
    public var block: BlockID? {
        if case let .insert(id, _, _, _, _) = kind { return id }
        return kind.target
    }
}

/// Our parked edits in an open editor: they stay on screen as ours, and
/// every later save of those blocks goes out on the parked propose's epoch,
/// flagged (the conflict path), so a keystroke never sneaks the parked text
/// past review on the normal path. When a human decides (the op leaves the
/// server's review queue) the overlay and the conflict go: accepted, the
/// server has our text; rejected, the server's text comes back.
public enum ParkedEdits {
    /// Overlay `parked` onto the server's view, and settle the conflicts:
    /// parked blocks are in conflict on their propose's epoch; blocks that
    /// were parked before but aren't now lose the conflict parking made.
    /// Returns the blocks now parked.
    @discardableResult
    public static func apply(
        _ parked: [ParkedOp], on editor: inout DocEditor,
        conflicts: inout [BlockID: SurfaceConflict], parkedBefore: Set<BlockID>
    ) -> Set<BlockID> {
        editor.overlay(parked.map(\.kind))
        var now: [BlockID: Int] = [:]
        for p in parked {
            guard let b = p.block else { continue }
            now[b] = min(now[b] ?? p.baseEpoch, p.baseEpoch)
        }
        for b in parkedBefore where now[b] == nil {
            conflicts[b] = nil
        }
        for (b, base) in now {
            var c = conflicts[b] ?? SurfaceConflict(base: base)
            c.base = min(c.base, base)
            conflicts[b] = c
        }
        return Set(now.keys)
    }
}
