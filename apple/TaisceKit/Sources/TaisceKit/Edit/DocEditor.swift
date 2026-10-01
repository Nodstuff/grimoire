import Foundation

/// What the editor asks for, in block terms. Each becomes one or more
/// `BlockOp`s for `POST /api/propose`.
public enum BlockEdit: Sendable, Hashable {
    /// New text for a block (the server retypes it from the markdown: a
    /// paragraph edited into `## x` becomes a heading).
    case replaceText(BlockID, String)
    /// A new block as the next sibling of `after`, or first under `parent`
    /// when `after` is nil (nil parent = the doc's top level). `id` nil
    /// mints one here, so later queued edits can target the new block.
    case insert(after: BlockID?, parent: BlockID?, type: BlockType, content: String, id: BlockID? = nil)
    /// The block and everything under it (the server's delete does not
    /// cascade: a heading's section would be left parentless).
    case delete(BlockID)
    /// Re-home a block (and its subtree) after `after`, or first under
    /// `parent` when `after` is nil.
    case move(BlockID, after: BlockID?, parent: BlockID?)
}

public enum EditError: Error, Sendable, Equatable {
    case unknownBlock(BlockID)
    /// `after` is not a child of the given parent.
    case notASibling(BlockID)
    /// Moving a block under itself or its own descendant.
    case cycle(BlockID)
}

/// One doc's blocks as the editor sees them — the server's, plus this
/// device's edits not yet replayed — turning `BlockEdit`s into `BlockOp`s
/// with order keys between the right neighbours.
///
/// `baseEpoch` is the server epoch those blocks came from. Every propose
/// built here carries it; the outbox rebases chained proposes as each one
/// lands (`OutboxReplayer`), so a run of queued edits applies green rather
/// than scoring as stale against our own earlier writes.
public struct DocEditor: Sendable {
    public let docID: DocID
    public let baseEpoch: Int
    public private(set) var blocks: [BlockID: Block]

    public init(docID: DocID, baseEpoch: Int, blocks: [Block]) {
        self.docID = docID
        self.baseEpoch = baseEpoch
        self.blocks = Dictionary(blocks.filter { !$0.deleted }.map { ($0.id, $0) }, uniquingKeysWith: { _, b in b })
    }

    public init(_ tree: DocTree) {
        self.init(docID: tree.doc.id, baseEpoch: tree.doc.currentEpoch, blocks: tree.flattened().map(\.block))
    }

    /// Children of `parent` in order (nil = top level).
    public func children(of parent: BlockID?) -> [Block] {
        blocks.values.filter { $0.parentID == parent }.sorted { ($0.orderKey, $0.id) < ($1.orderKey, $1.id) }
    }

    /// Depth-first document order, like `DocTree.flattened`.
    public func ordered() -> [(block: Block, depth: Int)] {
        var out: [(Block, Int)] = []
        func walk(_ parent: BlockID?, _ depth: Int) {
            for b in children(of: parent) {
                out.append((b, depth))
                walk(b.id, depth + 1)
            }
        }
        walk(nil, 0)
        return out
    }

    /// Apply locally and return the ops to send.
    public mutating func apply(_ edit: BlockEdit) throws -> [BlockOp] {
        switch edit {
        case let .replaceText(id, text):
            guard blocks[id] != nil else { throw EditError.unknownBlock(id) }
            blocks[id]?.content = text
            return [.replace(target: id, content: text)]

        case let .insert(after, parent, type, content, id):
            let key = try slot(after: after, parent: parent, excluding: nil)
            let id = id ?? UUID().uuidString.lowercased()
            blocks[id] = Block(id: id, docID: docID, parentID: parent, orderKey: key, blockType: type, content: content)
            return [.insert(blockID: id, parentID: parent, orderKey: key, type: type, content: content)]

        case let .delete(id):
            guard blocks[id] != nil else { throw EditError.unknownBlock(id) }
            // children first, so no op ever leaves a live block under a deleted one
            var ops: [BlockOp] = []
            func remove(_ b: BlockID) {
                for c in children(of: b) { remove(c.id) }
                blocks[b] = nil
                ops.append(.delete(target: b))
            }
            remove(id)
            return ops

        case let .move(id, after, parent):
            guard blocks[id] != nil else { throw EditError.unknownBlock(id) }
            var cursor = parent
            while let p = cursor {
                if p == id { throw EditError.cycle(id) }
                guard let b = blocks[p] else { throw EditError.unknownBlock(p) }
                cursor = b.parentID
            }
            let key = try slot(after: after, parent: parent, excluding: id)
            blocks[id]?.parentID = parent
            blocks[id]?.orderKey = key
            return [.move(target: id, newParent: parent, newOrderKey: key)]
        }
    }

    /// Apply several edits and wrap them in one propose (one gate verdict
    /// per op, one epoch bump). Throws before mutating anything if any edit
    /// is invalid.
    public mutating func propose(_ edits: [BlockEdit], requestID: String = UUID().uuidString.lowercased()) throws -> ProposeRequest {
        var draft = self
        let ops = try edits.flatMap { try draft.apply($0) }
        self = draft
        return ProposeRequest(docID: docID, baseEpoch: baseEpoch, ops: ops, requestID: requestID)
    }

    /// Replay already-sent or queued ops onto the local view (the outbox
    /// overlay). Ops whose target is gone are skipped, as the server would
    /// refuse them.
    public mutating func overlay(_ ops: [BlockOp]) {
        for op in ops {
            switch op {
            case let .insert(id?, parent, key, type, content):
                blocks[id] = Block(id: id, docID: docID, parentID: parent, orderKey: key, blockType: type, content: content)
            case .insert(nil, _, _, _, _):
                // server-minted id: nothing to target until the body is refetched
                break
            case let .replace(target, content):
                blocks[target]?.content = content
            case let .delete(target):
                blocks[target] = nil
            case let .move(target, parent, key):
                blocks[target]?.parentID = parent
                blocks[target]?.orderKey = key
            }
        }
    }

    /// The order key for a slot right after `after` (or first) under `parent`.
    func slot(after: BlockID?, parent: BlockID?, excluding moving: BlockID?) throws -> String {
        if let parent, blocks[parent] == nil { throw EditError.unknownBlock(parent) }
        let siblings = children(of: parent).filter { $0.id != moving }
        guard let after else {
            return OrderKey.between(nil, siblings.first?.orderKey)
        }
        guard blocks[after] != nil else { throw EditError.unknownBlock(after) }
        guard let i = siblings.firstIndex(where: { $0.id == after }) else { throw EditError.notASibling(after) }
        let next = siblings.index(after: i) < siblings.endIndex ? siblings[siblings.index(after: i)].orderKey : nil
        return OrderKey.between(siblings[i].orderKey, next)
    }
}

extension Cache {
    /// An editor over the cached body plus every queued, unsent propose for
    /// the doc, so edits made offline build on each other. Nil when the body
    /// was never fetched.
    public func editor(for id: DocID) async throws -> DocEditor? {
        guard let rec = try await doc(id), let bodyEpoch = rec.bodyEpoch else { return nil }
        let queued = try await pendingOutbox().compactMap { e -> ProposeRequest? in
            guard e.path == "/api/propose", let body = e.body,
                  let req = try? JSONDecoder().decode(ProposeRequest.self, from: body), req.docID == id
            else { return nil }
            return req
        }
        // chained edits keep the queue's base so replay can rebase them together
        let base = queued.first?.baseEpoch ?? bodyEpoch
        var editor = DocEditor(docID: id, baseEpoch: base, blocks: try await blocks(of: id).map(\.block))
        editor.overlay(queued.flatMap { $0.ops.map(\.kind) })
        return editor
    }

    /// Queue edits for `editor`'s doc and fold them into it.
    @discardableResult
    public func enqueue(_ edits: [BlockEdit], on editor: inout DocEditor, now: Date = .now) async throws -> OutboxEntry {
        var draft = editor
        let entry = try await enqueue(try draft.propose(edits), now: now)
        editor = draft
        return entry
    }
}
