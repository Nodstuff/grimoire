import Foundation

/// One doc in edit mode: the visible blocks in document order with their
/// editor content, over a `DocEditor` that turns every change into block
/// ops. Text edits are committed on demand (the app debounces them);
/// structure changes (split, merge, insert) build their propose at once.
///
/// Comments, canvases and frontmatter stay out of the editor (as on the
/// desktop) but keep their places in the tree.
public struct EditorSession: Sendable {
    public struct Item: Identifiable, Hashable, Sendable {
        public var id: BlockID
        public var content: EditorBlockContent
        /// the markdown last queued for it, or the server's
        public var saved: String
        /// `saved` as the editor writes it: a block is dirty once its
        /// content serialises to something else (an untouched block whose
        /// source isn't canonical is not)
        public var baseline: String
        /// exists only here so far: its first save is an insert
        public var isDraft: Bool
        /// bumped whenever the content is replaced from outside the text
        /// view (a split, a remote change), so the view reloads it
        public var revision: Int
        /// the server changed this block while it was being edited here:
        /// its next save is proposed against this older epoch, so the gate
        /// scores it as a conflict instead of it silently winning
        public var staleBase: Int?
        /// a conflicted save has gone out: until the server shows this
        /// block's text as ours (accepted on the desktop), only an explicit
        /// flush saves it again, still on `staleBase`
        public var conflictSent = false
        public var inConflict: Bool { staleBase != nil }

        public var isDirty: Bool { isDraft || content.markdown != baseline }

        init(id: BlockID, content: EditorBlockContent, saved: String, isDraft: Bool, revision: Int) {
            self.id = id
            self.content = content
            self.saved = saved
            baseline = isDraft ? "" : content.markdown
            self.isDraft = isDraft
            self.revision = revision
        }
    }

    /// Who should get the caret, and which blocks to reload.
    public struct Change: Sendable, Hashable {
        public var request: ProposeRequest?
        public var focus: BlockID
        public var caret: EditorCaret
        public var reload: Set<BlockID>
    }

    public private(set) var editor: DocEditor
    public private(set) var items: [Item]
    public var docID: DocID { editor.docID }

    public init(editor: DocEditor) {
        self.editor = editor
        items = Self.visible(editor).map { Item(id: $0.id, content: EditorBlockContent.parse($0), saved: $0.content, isDraft: false, revision: 0) }
        if items.isEmpty { items = [Self.draft()] }
    }

    /// `is_editor_hidden` (crates/store/src/lib.rs): comments, canvases, frontmatter.
    public static func isHidden(_ b: Block) -> Bool {
        b.blockType == .comment || b.blockType == .canvasScene || b.content.hasPrefix("---")
    }

    static func visible(_ editor: DocEditor) -> [Block] {
        editor.ordered().map(\.block).filter { !isHidden($0) }
    }

    static func draft(_ content: EditorBlockContent = .paragraph(AttributedString())) -> Item {
        Item(id: newID(), content: content, saved: "", isDraft: true, revision: 0)
    }

    static func newID() -> BlockID { UUID().uuidString.lowercased() }

    public func index(of id: BlockID) -> Int? { items.firstIndex { $0.id == id } }
    public func item(_ id: BlockID) -> Item? { index(of: id).map { items[$0] } }
    public var dirtyIDs: Set<BlockID> { Set(items.filter(\.isDirty).map(\.id)) }

    // MARK: text

    /// The text view's latest content for a block (no ops yet).
    public mutating func update(_ id: BlockID, content: EditorBlockContent) {
        guard let i = index(of: id) else { return }
        items[i].content = content
    }

    /// Set a block's content from outside its text view (a kind change,
    /// an indent): the view reloads it.
    public mutating func replace(_ id: BlockID, content: EditorBlockContent) {
        guard let i = index(of: id) else { return }
        items[i].content = content
        items[i].revision += 1
    }

    /// One propose for every changed block among `ids` (all when nil): a
    /// replace per edited block, an insert per draft. Blocks changed
    /// underneath on the server go out in their own propose on the older
    /// epoch (see `staleBase`). Empty when nothing changed.
    public mutating func commitText(_ ids: Set<BlockID>? = nil, includeConflicts: Bool = false) -> [ProposeRequest] {
        var batch = editor
        var ops: [BlockOp] = []
        var stale: [Int: [BlockOp]] = [:]
        for i in items.indices where ids?.contains(items[i].id) ?? true {
            guard items[i].isDirty, !(items[i].conflictSent && !includeConflicts) else { continue }
            guard let edit = textEdit(at: i, in: batch), let o = try? batch.apply(edit) else { continue }
            if let base = items[i].staleBase {
                // every save of a conflicted block stays on the older epoch
                stale[base, default: []] += o
                items[i].conflictSent = true
            } else {
                ops += o
            }
        }
        editor = batch
        var out = stale.sorted { $0.key < $1.key }.map { ProposeRequest(docID: docID, baseEpoch: $0.key, ops: $0.value, requestID: Self.newID()) }
        if !ops.isEmpty { out.append(ProposeRequest(docID: docID, baseEpoch: editor.baseEpoch, ops: ops, requestID: Self.newID())) }
        return out
    }

    /// The edit that saves item `i` as it stands (against `ed`), marking it saved.
    mutating func textEdit(at i: Int, in ed: DocEditor) -> BlockEdit? {
        let md = items[i].content.markdown
        if items[i].isDraft {
            let (after, parent) = draftSlot(at: i, in: ed)
            items[i].saved = md
            items[i].baseline = md
            items[i].isDraft = false
            return .insert(after: after, parent: parent, type: Self.type(of: items[i].content), content: md, id: items[i].id)
        }
        guard ed.blocks[items[i].id] != nil else { return nil }
        items[i].saved = md
        items[i].baseline = md
        return .replaceText(items[i].id, md)
    }

    /// Where a draft goes: after the nearest block before it the editor
    /// has, else after any hidden blocks leading the doc (frontmatter stays first).
    func draftSlot(at i: Int, in ed: DocEditor) -> (BlockID?, BlockID?) {
        for j in stride(from: i - 1, through: 0, by: -1) where ed.blocks[items[j].id] != nil {
            return Self.slot(after: items[j].id, in: ed)
        }
        let leading = ed.children(of: nil).prefix { Self.isHidden($0) }
        return (leading.last?.id, nil)
    }

    /// The slot right after `id` in document order: its first child when
    /// it heads a section (or already has children), else its next sibling.
    static func slot(after id: BlockID, in ed: DocEditor) -> (BlockID?, BlockID?) {
        guard let b = ed.blocks[id] else { return (nil, nil) }
        if b.blockType == .heading || !ed.children(of: id).isEmpty {
            return (nil, id)
        }
        return (id, b.parentID)
    }

    static func type(of content: EditorBlockContent) -> BlockType {
        switch content {
        case .heading: .heading
        case let .raw(s): s.hasPrefix("```mermaid") ? .diagramMermaid : s.hasPrefix("```d2") ? .diagramD2 : s.hasPrefix("```") || s.hasPrefix("~~~") ? .code : .paragraph
        default: .paragraph
        }
    }

    // MARK: structure

    public mutating func returnKey(_ id: BlockID, caret: EditorCaret) -> Change? {
        guard let item = item(id) else { return nil }
        return apply(EditorCommands.returnKey(item.content, at: caret), to: id)
    }

    public mutating func backspaceAtStart(_ id: BlockID, caret: EditorCaret) -> Change? {
        guard let item = item(id) else { return nil }
        return apply(EditorCommands.backspaceAtStart(item.content, at: caret), to: id)
    }

    /// Carry out a command's result on block `id`.
    public mutating func apply(_ result: BlockCommandResult, to id: BlockID) -> Change? {
        guard let i = index(of: id) else { return nil }
        switch result {
        case .none:
            return nil

        case let .update(content, caret):
            items[i].content = content
            items[i].revision += 1
            return Change(request: nil, focus: id, caret: caret, reload: [id])

        case let .split(current, inserted, focus, caret):
            var batch = editor
            var ops: [BlockOp] = []
            items[i].content = current
            if items[i].isDirty, let e = textEdit(at: i, in: batch), let o = try? batch.apply(e) { ops += o }
            var newItems: [Item] = []
            var (after, parent) = Self.slot(after: id, in: batch)
            for content in inserted {
                let new = Self.newID()
                let md = content.markdown
                guard let o = try? batch.apply(.insert(after: after, parent: parent, type: Self.type(of: content), content: md, id: new)) else { return nil }
                ops += o
                newItems.append(Item(id: new, content: content, saved: md, isDraft: false, revision: 0))
                (after, parent) = (new, parent)
            }
            items[i].revision += 1
            items.insert(contentsOf: newItems, at: i + 1)
            let target = focus == 0 || newItems.isEmpty ? id : newItems[min(focus, newItems.count) - 1].id
            return Change(request: commit(batch, ops), focus: target, caret: caret, reload: Set([id] + newItems.map(\.id)))

        case let .insertBefore(content):
            var batch = editor
            var ops: [BlockOp] = []
            if items[i].isDirty, let e = textEdit(at: i, in: batch), let o = try? batch.apply(e) { ops += o }
            guard let b = batch.blocks[id] else { return nil }
            let siblings = batch.children(of: b.parentID)
            let prev = siblings.firstIndex { $0.id == id }.flatMap { $0 > 0 ? siblings[$0 - 1].id : nil }
            let new = Self.newID()
            let md = content.markdown
            guard let o = try? batch.apply(.insert(after: prev, parent: b.parentID, type: Self.type(of: content), content: md, id: new)) else { return nil }
            ops += o
            items.insert(Item(id: new, content: content, saved: md, isDraft: false, revision: 0), at: i)
            return Change(request: commit(batch, ops), focus: id, caret: .start, reload: [new])

        case .mergeWithPrevious:
            guard i > 0 else { return nil }
            let prev = items[i - 1]
            let cur = items[i]
            guard let (merged, caret) = EditorCommands.merge(prev.content, cur.content) else {
                // raw source above: just move there (an empty block goes away)
                if cur.content.isEmpty {
                    return remove(at: i, focus: prev.id, caret: EditorCommands.endCaret(prev.content))
                }
                return Change(request: nil, focus: prev.id, caret: EditorCommands.endCaret(prev.content), reload: [])
            }
            var batch = editor
            var ops: [BlockOp] = []
            items[i - 1].content = merged
            items[i - 1].revision += 1
            if items[i - 1].isDirty, let e = textEdit(at: i - 1, in: batch), let o = try? batch.apply(e) { ops += o }
            for e in Self.removal(of: cur, in: batch) {
                guard let o = try? batch.apply(e) else { return nil }
                ops += o
            }
            items.remove(at: i)
            return Change(request: commit(batch, ops), focus: prev.id, caret: caret, reload: [prev.id])
        }
    }

    /// Adopt a batch's editor; its ops as one propose (nil when empty).
    mutating func commit(_ batch: DocEditor, _ ops: [BlockOp]) -> ProposeRequest? {
        editor = batch
        return ops.isEmpty ? nil : ProposeRequest(docID: docID, baseEpoch: editor.baseEpoch, ops: ops, requestID: Self.newID())
    }

    /// Drop item `i` (its block, if the server has one).
    mutating func remove(at i: Int, focus: BlockID, caret: EditorCaret) -> Change? {
        var batch = editor
        var ops: [BlockOp] = []
        for e in Self.removal(of: items[i], in: batch) {
            guard let o = try? batch.apply(e) else { return nil }
            ops += o
        }
        items.remove(at: i)
        return Change(request: commit(batch, ops), focus: focus, caret: caret, reload: [])
    }

    /// Delete a block without taking its section with it: its children
    /// move up to follow it first (the server's delete does not cascade,
    /// and ours removes the subtree).
    static func removal(of item: Item, in ed: DocEditor) -> [BlockEdit] {
        guard let b = ed.blocks[item.id] else { return [] }
        var edits: [BlockEdit] = []
        var after = item.id
        for c in ed.children(of: item.id) {
            edits.append(.move(c.id, after: after, parent: b.parentID))
            after = c.id
        }
        edits.append(.delete(item.id))
        return edits
    }

    // MARK: remote changes

    /// The doc changed on the server (or the queue moved): take the new
    /// blocks, keeping what's being typed. Blocks in `keep` (the focused
    /// one) and unsaved ones keep their local content; one the server
    /// changed underneath them is marked `staleBase`, and one the server
    /// deleted becomes a draft again, so no typed text is lost. Returns
    /// the blocks to reload.
    @discardableResult
    public mutating func refresh(_ newEditor: DocEditor, keep: Set<BlockID>) -> Set<BlockID> {
        let oldBase = editor.baseEpoch
        let old = Dictionary(items.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        let held = Set(items.filter { keep.contains($0.id) || $0.isDirty || $0.inConflict }.map(\.id))
        var reload: Set<BlockID> = []
        var next: [Item] = []
        for b in Self.visible(newEditor) {
            if var mine = old[b.id], held.contains(b.id) {
                if mine.inConflict, b.content == mine.content.markdown {
                    // the desktop accepted ours: the conflict is over
                    mine.staleBase = nil
                    mine.conflictSent = false
                    mine.saved = b.content
                    mine.baseline = b.content
                } else if b.content != mine.saved, !mine.isDraft {
                    // someone else wrote this block while it was open here
                    if mine.content.markdown != b.content { mine.staleBase = mine.staleBase ?? oldBase }
                    mine.saved = b.content
                }
                mine.isDraft = false
                next.append(mine)
            } else if var mine = old[b.id] {
                if b.content != mine.saved || EditorBlockContent.parse(b) != mine.content {
                    mine.content = EditorBlockContent.parse(b)
                    mine.saved = b.content
                    mine.baseline = mine.content.markdown
                    mine.revision += 1
                    reload.insert(b.id)
                }
                mine.isDraft = false
                mine.staleBase = nil
                next.append(mine)
            } else {
                next.append(Item(id: b.id, content: EditorBlockContent.parse(b), saved: b.content, isDraft: false, revision: 0))
                reload.insert(b.id)
            }
        }
        // held blocks the server no longer has (and unsaved drafts) stay, after their old neighbour
        let present = Set(next.map(\.id))
        for (k, it) in items.enumerated() where !present.contains(it.id) && (held.contains(it.id) || it.isDraft) {
            var kept = it
            kept.isDraft = true
            kept.staleBase = nil
            let placed = Set(next.map(\.id))
            let anchor = items[..<k].last { placed.contains($0.id) }?.id
            let at = anchor.flatMap { a in next.firstIndex { $0.id == a } }.map { $0 + 1 } ?? 0
            next.insert(kept, at: at)
        }
        if next.isEmpty { next = [Self.draft()] }
        editor = newEditor
        items = next
        return reload
    }
}
