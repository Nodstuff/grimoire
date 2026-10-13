import Foundation

/// A block the server changed while it was being edited here: its saves
/// go out on `base` (the older epoch), so the gate scores the conflict
/// instead of our text silently winning. `theirs` is the server's text
/// when it differs from ours (the editor offers Keep mine / Take theirs).
public struct SurfaceConflict: Hashable, Sendable {
    public var base: Int
    public var theirs: String?

    public init(base: Int, theirs: String? = nil) {
        self.base = base
        self.theirs = theirs
    }
}

/// Taking a server change into an open surface without moving the caret
/// or losing unsaved text. Pure: the app rebuilds its text from `blocks`.
public enum SurfaceMerge {
    public struct Piece: Hashable, Sendable {
        public var id: BlockID
        public var content: EditorBlockContent
        /// keep the surface's own text for this block (typed, unsaved, or unchanged)
        public var local: Bool
        /// the block the local text came from, when it moves to a new id
        /// (the server deleted a block edited here: it comes back as an insert)
        public var from: BlockID?
    }

    public struct Result: Sendable {
        public var blocks: [Piece]
        public var baseline: [DocDiff.BaselineBlock]
        public var conflicts: [BlockID: SurfaceConflict]
        /// whether the surface's text needs rebuilding
        public var changed: Bool
    }

    /// `local` is the surface as it stands (ids and contents in order);
    /// `server` the server's visible blocks (plus our queued writes).
    public static func merge(
        local: [(id: BlockID, content: EditorBlockContent)],
        baseline: [DocDiff.BaselineBlock],
        conflicts: [BlockID: SurfaceConflict],
        oldEpoch: Int,
        server: [Block],
        parked: Set<BlockID> = [],
        mint: () -> BlockID = Surface.newID
    ) -> Result {
        let base = Dictionary(baseline.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        let mine = Dictionary(local.map { ($0.id, $0.content) }, uniquingKeysWith: { a, _ in a })
        let theirIDs = Set(server.map(\.id))
        var conflicts = conflicts
        var pieces: [Piece] = []
        var newBase: [DocDiff.BaselineBlock] = []

        for s in server {
            let sc = Surface.baselineContent(s)
            let theirs = DocDiff.BaselineBlock(id: s.id, content: sc, parent: s.parentID, orderKey: s.orderKey)
            switch (base[s.id], mine[s.id]) {
            case let (b?, l?):
                let md = l.markdown
                if var c = conflicts[s.id] {
                    if parked.contains(s.id) {
                        // our parked edit (overlaid): in conflict until a human decides
                    } else if sc == md {
                        // the other side took ours (accepted on the desktop): over
                        conflicts[s.id] = nil
                    } else {
                        c.theirs = sc
                        conflicts[s.id] = c
                    }
                    pieces.append(Piece(id: s.id, content: l, local: true))
                } else if sc != b.content, md == b.content {
                    // changed there, untouched here: take it
                    pieces.append(Piece(id: s.id, content: EditorBlockContent.parse(s), local: false))
                } else if sc != b.content, md != sc {
                    // changed on both sides
                    conflicts[s.id] = SurfaceConflict(base: oldEpoch, theirs: sc)
                    pieces.append(Piece(id: s.id, content: l, local: true))
                } else {
                    pieces.append(Piece(id: s.id, content: l, local: true))
                }
                newBase.append(theirs)
            case let (b?, nil):
                // deleted here (or merged away), not saved yet: stays deleted.
                // If it changed there meanwhile, our delete goes out on the
                // older epoch, so the gate scores it instead of it winning
                if sc != b.content, conflicts[s.id] == nil { conflicts[s.id] = SurfaceConflict(base: oldEpoch) }
                newBase.append(theirs)
            case let (nil, l?):
                pieces.append(Piece(id: s.id, content: l, local: true))
                newBase.append(theirs)
            case (nil, nil):
                // new there
                pieces.append(Piece(id: s.id, content: EditorBlockContent.parse(s), local: false))
                newBase.append(theirs)
            }
        }

        // what only this side has, after its old neighbour
        for (k, l) in local.enumerated() where !theirIDs.contains(l.id) {
            var piece: Piece
            if let b = base[l.id] {
                // the server deleted it: an edited one comes back as a new block
                guard l.content.markdown != b.content else { continue }
                piece = Piece(id: mint(), content: l.content, local: true, from: l.id)
                conflicts[l.id] = nil
            } else {
                piece = Piece(id: l.id, content: l.content, local: true)
            }
            let placed = Set(pieces.map { $0.from ?? $0.id })
            let anchor = local[..<k].last { placed.contains($0.id) }?.id
            let at = anchor.flatMap { a in pieces.firstIndex { ($0.from ?? $0.id) == a } }.map { $0 + 1 } ?? 0
            pieces.insert(piece, at: at)
        }

        // a conflict on a block neither side still has is over
        let present = theirIDs.union(pieces.map(\.id))
        conflicts = conflicts.filter { present.contains($0.key) }

        let before = local.map(\.id)
        let after = pieces.map { $0.from ?? $0.id }
        let changed = before != after || pieces.contains { !$0.local && mine[$0.id] != $0.content } || pieces.contains { $0.from != nil }
        if pieces.isEmpty {
            pieces = [Piece(id: mint(), content: .paragraph(AttributedString()), local: false)]
        }
        return Result(blocks: pieces, baseline: newBase, conflicts: conflicts, changed: changed || pieces.count != local.count)
    }
}
