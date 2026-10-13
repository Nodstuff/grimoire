import Foundation
import Observation
import SwiftUI
import UIKit
import UniformTypeIdentifiers
import TaisceKit

/// One open doc: ONE always-live text surface (`SurfaceTextView`), its
/// saves and its sync state. The text view holds the doc (TaisceKit
/// `Surface` attributes carry the block ids); a save diffs it against the
/// baseline (`DocDiff`, the web editor's diff) and queues the ops in the
/// outbox. Read-only (a viewer, a canvas, a mirror) is the same surface,
/// not editable, links tap-to-follow.
@MainActor @Observable
final class SurfaceModel: NSObject {
    struct Completion: Equatable {
        var query: String
        /// the caret, in window coordinates
        var anchor: CGRect
        var results: [WikiCandidate]
        var selected: Int
    }

    let docID: DocID
    private(set) var editable: Bool
    private(set) var completion: Completion?
    private(set) var conflicts: [BlockID: SurfaceConflict] = [:]
    private(set) var reviews: [BlockID: BlockReview] = [:]
    private(set) var failedBlocks: Set<BlockID> = []
    private(set) var outbox = DocOutboxState()
    /// a save waiting on the debounce
    private(set) var unsaved = 0
    private(set) var activeMarks = InlineMarks()
    /// the non-text block whose source sheet is open
    var sourceEditing: BlockID?
    var lastError: String?
    /// outbox writes that kept failing: their text stays on screen, the chip says so
    private(set) var persistFailures = 0

    // what attachments render with
    @ObservationIgnored var chartTables = ChartTables()
    @ObservationIgnored var docScale: CGFloat = 1
    /// a wikilink or URL was followed
    @ObservationIgnored var onOpenWiki: (String) -> Void = { _ in }
    /// a checkbox in the To-do doc: (block, index in block, checked) → handled elsewhere
    @ObservationIgnored var onTodoCheckbox: ((BlockID, Int, Bool) -> Void)?

    @ObservationIgnored let textView: SurfaceTextView
    @ObservationIgnored private(set) var baseline: [DocDiff.BaselineBlock] = []
    @ObservationIgnored private(set) var baseEpoch: Int
    @ObservationIgnored private var blockTypes: [BlockID: BlockType] = [:]
    @ObservationIgnored let cache: Cache?
    @ObservationIgnored private let api: APIClient?
    @ObservationIgnored private(set) weak var app: AppModel?
    @ObservationIgnored private var saver: SaveScheduler<Int>!
    @ObservationIgnored private var candidates: [WikiCandidate] = []
    @ObservationIgnored private var queue: Task<Void, Never>?
    @ObservationIgnored private var observeTask: Task<Void, Never>?
    @ObservationIgnored private var pollTask: Task<Void, Never>?
    /// the doc's review queue as the server last gave it (nil: never fetched)
    @ObservationIgnored private var openOps: Set<String>?
    /// blocks whose shown text is our parked edit (`ParkedEdits`)
    @ObservationIgnored private var parkedBlocks: Set<BlockID> = []
    /// parked blocks the user settled with Keep mine: shown and saved as theirs to decide no more
    @ObservationIgnored private var released: Set<BlockID> = []
    /// For each block in conflict, its flagged write: the outbox row's key
    /// ("" when it went out before this session) and what it said. One
    /// disputed block has at most one pending flagged write: later saves
    /// rewrite it in place; once it has gone out, more typing in the block
    /// waits for a final flush (leaving the doc, backgrounding, demotion),
    /// and the same text is never proposed twice.
    @ObservationIgnored private var conflictWrites: [BlockID: (key: String, text: String)] = [:]
    /// the blocks the surface leaves out (frontmatter, comments), in place
    @ObservationIgnored private var hidden: [DocDiff.HiddenBlock] = []
    @ObservationIgnored private var lastReviewFetch = Date.distantPast
    @ObservationIgnored var pendingMarks: InlineMarks?
    @ObservationIgnored var reloading = false
    @ObservationIgnored private var attachments: [String: BlockAttachment] = [:]
    @ObservationIgnored private var dynamicType: DynamicTypeSize = .large
    @ObservationIgnored private var wantsFocus = false
    @ObservationIgnored private var pendingRemote: DocEditor?
    @ObservationIgnored lazy var formattingBar: FormattingBar = FormattingBar(model: self)
    @ObservationIgnored private var headerHost: UIHostingController<AnyView>?
    @ObservationIgnored private var footerHost: UIHostingController<AnyView>?
    /// the surface's own tap (links, checkboxes); the text view's taps
    /// stand back where it lands on one (`SurfaceTextView`)
    @ObservationIgnored weak var ownTap: UITapGestureRecognizer?
    /// a touch on a link or a checkbox: no editing begins for it until then
    @ObservationIgnored var focusHeldUntil: Date?
    /// what the last `shouldChangeTextIn` let UIKit insert (for shortcuts)
    @ObservationIgnored var lastTyped: String?
    /// where UIKit is about to insert typed text (scopes the normalise after it)
    @ObservationIgnored var typedRange: NSRange?
    /// the keystroke UIKit is about to apply: what it replaces, the doc's
    /// length and the selection before (registered as an undo step after)
    @ObservationIgnored var pendingTyping: (range: NSRange, old: NSAttributedString, length: Int, selection: NSRange)?
    /// an edit that is shown but not saved from here (a To-do box)
    @ObservationIgnored private var quiet = false

    init(docID: DocID, editor: DocEditor, cache: Cache?, api: APIClient?, app: AppModel?, editable: Bool,
         conflicts: [BlockID: Int] = [:], saveDelay: Duration = .milliseconds(800)) {
        self.docID = docID
        self.cache = cache
        self.api = api
        self.app = app
        self.editable = editable
        baseEpoch = editor.baseEpoch
        textView = SurfaceTextView(usingTextLayoutManager: true)
        super.init()
        saver = SaveScheduler(delay: saveDelay) { [weak self] _ in self?.save() }
        replay = { [weak app] in await app?.replayOutbox() }
        if let index = app?.index {
            candidates = index.docs.filter { $0.id != docID }.map { WikiCandidate(id: $0.id, title: $0.title, breadcrumb: index.breadcrumb(of: $0.id)) }
        }
        for (b, base) in conflicts { self.conflicts[b] = SurfaceConflict(base: base) }
        configureTextView()
        load(editor)
    }

    /// The cached body (plus everything queued) as a surface; nil when the
    /// body was never fetched.
    static func open(_ docID: DocID, app: AppModel, editable: Bool) async -> SurfaceModel? {
        guard let cache = app.cache else { return nil }
        if (try? await cache.doc(docID))?.bodyEpoch == nil { try? await app.sync?.refresh(docID) }
        // straight from the cache: the review queue comes with the first
        // poll, in the background (parked edits show when it arrives)
        let model = await open(docID, cache: cache, api: app.api, app: app, editable: editable, reviewQueue: nil)
        model?.start()
        return model
    }

    /// The surface over the cached body (plus everything queued), with the
    /// conflicts the outbox remembers and, given the server's review queue,
    /// our parked edits shown. Nil when the body was never fetched.
    static func open(_ docID: DocID, cache: Cache, api: APIClient?, app: AppModel?, editable: Bool,
                     reviewQueue: Set<String>?, saveDelay: Duration = .milliseconds(800)) async -> SurfaceModel? {
        guard var editor = try? await cache.editor(for: docID) else { return nil }
        var conflicts: [BlockID: SurfaceConflict] = [:]
        for (b, base) in (try? await cache.conflictedBlocks(docID)) ?? [:] { conflicts[b] = SurfaceConflict(base: base) }
        // our parked edits show from the start (when the server's review
        // queue is known now; otherwise the first poll that gets it)
        var parked: Set<BlockID> = []
        if editable, let queue = reviewQueue, let ops = try? await cache.parkedOps(for: docID, openOps: queue) {
            parked = ParkedEdits.apply(ops, on: &editor, conflicts: &conflicts, parkedBefore: [])
        } else if editable, let held = try? await cache.unappliedBlocks(for: docID) {
            // the queue isn't known yet: blocks our unapplied ops wrote are
            // held (in conflict on their epoch), so no save quietly reverts
            // a parked edit; the queue, when it comes, settles them
            for (b, base) in held where conflicts[b] == nil { conflicts[b] = SurfaceConflict(base: base) }
            parked = Set(held.keys)
        }
        let model = SurfaceModel(docID: docID, editor: editor, cache: cache, api: api, app: app, editable: editable, saveDelay: saveDelay)
        // these conflicts' flagged writes went out before: nothing re-sent
        // until there's something new and the doc is left
        for b in conflicts.keys { model.conflictWrites[b] = ("", "") }
        model.conflicts = conflicts
        model.openOps = reviewQueue
        model.parkedBlocks = parked
        if reviewQueue != nil { model.lastReviewFetch = Date() }
        return model
    }

    // MARK: setup

    private func configureTextView() {
        let tv = textView
        tv.model = self
        tv.delegate = self
        tv.backgroundColor = .clear
        tv.alwaysBounceVertical = true
        tv.keyboardDismissMode = .interactive
        tv.textContainer.lineFragmentPadding = 0
        tv.allowsEditingTextAttributes = false
        tv.dataDetectorTypes = []
        tv.isEditable = editable
        tv.isSelectable = true
        tv.gutter = Theme.gutter
        tv.readingWidth = Theme.readingWidth
        tv.accessibilityIdentifier = "surface.text"
        tv.textDragInteraction?.isEnabled = false
        // dropped text goes through Surface (block ids), not UIKit's insert
        tv.textDropDelegate = self
        // Writing Tools: nothing documents that its rewrites (Replace) go
        // through the delegate, and a rewrite UIKit applies itself would lose
        // block ids. Off on the surface until it can be routed through Surface
        tv.writingToolsBehavior = .none
        if editable, !EditorChrome.barIsInline { tv.inputAccessoryView = formattingBar }
        let tap = UITapGestureRecognizer(target: self, action: #selector(tapped(_:)))
        tap.delegate = self
        tap.cancelsTouchesInView = false
        tv.addGestureRecognizer(tap)
        ownTap = tap
    }

    /// Blocks as the surface shows them: the editor's visible blocks; a
    /// read-only surface also shows canvases (as cards).
    private func surfaceBlocks(_ editor: DocEditor) -> (blocks: [(id: BlockID, content: EditorBlockContent)], baseline: [DocDiff.BaselineBlock]) {
        blockTypes = Dictionary(editor.blocks.values.map { ($0.id, $0.blockType) }, uniquingKeysWith: { a, _ in a })
        if editable { return Surface.load(editor) }
        var blocks: [(id: BlockID, content: EditorBlockContent)] = []
        var baseline: [DocDiff.BaselineBlock] = []
        for (b, _) in editor.ordered() where b.blockType != .comment && !b.content.hasPrefix("---") {
            let c = EditorBlockContent.parse(b)
            blocks.append((b.id, c))
            // what it shows is its baseline, canvases too, so a refresh updates them
            baseline.append(DocDiff.BaselineBlock(id: b.id, content: c.markdown, parent: b.parentID, orderKey: b.orderKey))
        }
        return (blocks, baseline)
    }

    private func load(_ editor: DocEditor) {
        let (blocks, base) = surfaceBlocks(editor)
        baseline = base
        hidden = Surface.hidden(editor)
        baseEpoch = editor.baseEpoch
        reloading = true
        let doc = Surface.document(blocks)
        textView.textStorage.setAttributedString(doc)
        restyle()
        textView.selectedRange = NSRange(location: 0, length: 0)
        reloading = false
        forgetUndo()
    }

    /// Watch the cached body and poll the outbox and review queue. Safe
    /// to call again (back on screen after a pushed doc, the app active
    /// again): the old watchers go, the doc catches up at once, then
    /// they resume. Polling backs off (3 s → 30 s) while nothing changes.
    func start() {
        observeTask?.cancel()
        pollTask?.cancel()
        observeTask = Task { [weak self, cache, docID] in
            guard let cache else { return }
            do {
                // the first value is the body as it is now: what changed
                // while we weren't watching merges in straight away
                for try await _ in cache.observeBlocks(of: docID) {
                    guard !Task.isCancelled else { return }
                    await self?.remoteChanged()
                }
            } catch {}
        }
        pollTask = Task { [weak self] in
            var wait: Duration = .seconds(3)
            while !Task.isCancelled {
                guard let self else { return }
                let changed = await self.poll()
                wait = changed ? .seconds(3) : min(wait * 2, .seconds(30))
                try? await Task.sleep(for: wait)
            }
        }
    }

    var isWatching: Bool { observeTask != nil && !(observeTask?.isCancelled ?? true) }

    func stop() {
        observeTask?.cancel()
        pollTask?.cancel()
        observeTask = nil
        pollTask = nil
        saver.cancelAll()
    }

    /// The doc header and children list, as subviews of the text view.
    func setChrome(header: AnyView, footer: AnyView?) {
        if let h = headerHost {
            h.rootView = header
        } else {
            let h = UIHostingController(rootView: header)
            h.view.backgroundColor = .clear
            h.sizingOptions = [.intrinsicContentSize]
            headerHost = h
            textView.header = h.view
        }
        if let footer {
            if let f = footerHost {
                f.rootView = footer
            } else {
                let f = UIHostingController(rootView: footer)
                f.view.backgroundColor = .clear
                f.sizingOptions = [.intrinsicContentSize]
                footerHost = f
                textView.footer = f.view
            }
        } else if footerHost != nil {
            footerHost = nil
            textView.footer = nil
        }
        textView.chromeDirty = true
        textView.setNeedsLayout()
    }

    /// A doc just created here: the caret at the start, keyboard up.
    func focusAtStart() {
        guard editable else { return }
        if textView.window == nil {
            wantsFocus = true
            Task { @MainActor [weak self] in
                for _ in 0..<20 {
                    try? await Task.sleep(for: .milliseconds(50))
                    guard let self, self.wantsFocus else { return }
                    if self.textView.window != nil {
                        self.wantsFocus = false
                        self.textView.becomeFirstResponder()
                        self.textView.selectedRange = NSRange(location: 0, length: 0)
                        return
                    }
                }
            }
            return
        }
        textView.becomeFirstResponder()
        textView.selectedRange = NSRange(location: 0, length: 0)
    }

    /// A viewer now (ADR 0004) or an editor again: the same surface,
    /// editable or not. Callers flush first.
    func setEditable(_ on: Bool) async {
        guard on != editable else { return }
        // made a viewer: what's typed goes to the outbox first (the server
        // says no and the chip shows it; nothing typed is dropped)
        if !on { await flush(final: true) }
        if !on {
            textView.resignFirstResponder()
            completion = nil
            // ⌘Z must not change a read-only surface
            forgetUndo()
        }
        editable = on
        textView.isEditable = on
        textView.inputAccessoryView = on && !EditorChrome.barIsInline ? formattingBar : nil
        attachments = [:]
        restyle()
    }

    func setDynamicType(_ size: DynamicTypeSize) {
        guard size != dynamicType else { return }
        dynamicType = size
        #if targetEnvironment(macCatalyst)
        docScale = DocTextSize.scale(for: size)
        #endif
        attachments = [:]
        restyle()
    }

    // MARK: look

    var traits: UITraitCollection {
        textView.traitCollection.modifyingTraits {
            $0.preferredContentSizeCategory = UIContentSizeCategory(dynamicType)
            #if targetEnvironment(macCatalyst)
            $0.docScale = DocTextSize.scale(for: dynamicType)
            #endif
        }
    }

    var lookContext: SurfaceLook.Context {
        var tints: [BlockID: UIColor] = [:]
        for (id, r) in reviews { tints[id] = UIColor(r.verdict == .red ? Theme.rose : Theme.amber).withAlphaComponent(0.16) }
        for id in failedBlocks { tints[id] = UIColor(Theme.rose).withAlphaComponent(0.16) }
        for (id, c) in conflicts where c.theirs != nil { tints[id] = UIColor(Theme.rose).withAlphaComponent(0.16) }
        return SurfaceLook.Context(traits: traits, editable: editable, tints: tints) { [weak self] id, src in
            self?.attachment(id, source: src)
        }
    }

    /// Restyle lines touching `range` (everything when nil).
    func restyle(_ range: NSRange? = nil) {
        let storage = textView.textStorage
        let sel = textView.selectedRange
        let was = reloading
        reloading = true
        storage.beginEditing()
        SurfaceLook.restyle(storage, range: range, ctx: lookContext)
        storage.endEditing()
        if textView.selectedRange != sel, sel.location + sel.length <= storage.length { textView.selectedRange = sel }
        reloading = was
        updateTyping()
    }

    private func attachment(_ id: BlockID, source: String) -> NSTextAttachment {
        let key = "\(id)\u{0}\(source.hashValue)"
        if let a = attachments[key] { return a }
        let type = blockTypes[id] ?? .paragraph
        let nodes: [RenderNode]
        if case .canvasScene = type {
            nodes = BlockRenderer.render(Block(id: id, docID: docID, parentID: nil, orderKey: "i", blockType: type, content: source))
        } else {
            nodes = BlockRenderer.render(markdown: source)
        }
        let a = BlockAttachment(blockID: id, source: source, nodes: nodes.isEmpty ? [.paragraph(inline: source)] : nodes, host: self)
        attachments[key] = a
        return a
    }

    func attachmentContent(_ a: BlockAttachment) -> AnyView {
        AnyView(AttachmentContent(attachment: a, model: self, app: app))
    }

    /// A hosted block changed height (a diagram finished): lay it out again.
    func attachmentResized(_ a: BlockAttachment, height: CGFloat) {
        // the hosted view reports its ideal height (it is fixed-size inside)
        guard height > 0, !a.heights.isEmpty, a.heights.values.contains(where: { abs($0 - height) > 1 }) else { return }
        for k in a.heights.keys { a.heights[k] = height }
        guard let tlm = textView.textLayoutManager else { return }
        let storage = textView.textStorage
        var found: NSRange?
        storage.enumerateAttribute(.attachment, in: NSRange(location: 0, length: storage.length)) { v, r, stop in
            if (v as? BlockAttachment) === a { found = r; stop.pointee = true }
        }
        guard let r = found, let range = textRange(r) else { return }
        tlm.invalidateLayout(for: range)
        textView.setNeedsLayout()
    }

    func textRange(_ r: NSRange) -> NSTextRange? {
        guard let tlm = textView.textLayoutManager, let cm = tlm.textContentManager,
              let start = cm.location(cm.documentRange.location, offsetBy: r.location),
              let end = cm.location(start, offsetBy: r.length) else { return nil }
        return NSTextRange(location: start, end: end)
    }

    // MARK: edits

    /// Carry out an edit as one undoable step; the doc is normalised, the
    /// changed lines restyled, and a save scheduled.
    @discardableResult
    func perform(_ e: Surface.Edit?) -> Bool {
        guard let e else { return false }
        if e.isSelectionOnly {
            textView.selectedRange = e.selection
            return true
        }
        apply(e, undoSelection: textView.selectedRange)
        return true
    }

    private func apply(_ e: Surface.Edit, undoSelection: NSRange) {
        // read-only (a viewer now): nothing changes the text, undo included
        guard editable || quiet else { return }
        let storage = textView.textStorage
        let before = NSAttributedString(attributedString: storage)
        reloading = true
        storage.beginEditing()
        storage.replaceCharacters(in: e.range, with: e.replacement)
        var sel = e.selection
        Surface.normalize(storage, selection: &sel)
        storage.endEditing()
        // the region that changed (the edit, plus what normalising touched)
        let old = before.string as NSString
        let new = storage.mutableString
        var p = 0
        let minLen = min(old.length, new.length)
        while p < minLen, old.character(at: p) == new.character(at: p) { p += 1 }
        var q = 0
        while q < minLen - p, old.character(at: old.length - 1 - q) == new.character(at: new.length - 1 - q) { q += 1 }
        let lo = min(p, e.range.location)
        let hi = max(old.length - q, e.range.location + e.range.length)
        let oldRange = NSRange(location: lo, length: max(0, hi - lo))
        let newRange = NSRange(location: lo, length: max(0, oldRange.length + new.length - old.length))
        let oldText = before.attributedSubstring(from: oldRange)
        let redoSelection = sel
        undoStep { um in
            register(UndoRecord(range: newRange, replacement: oldText, selection: undoSelection, redoSelection: redoSelection), on: um)
        }
        SurfaceLook.restyle(storage, range: NSRange(location: newRange.location, length: newRange.length), ctx: lookContext)
        textView.selectedRange = sel
        reloading = false
        updateTyping()
        changed()
        textView.scrollRangeToVisible(textView.selectedRange)
    }

    // MARK: undo

    /// The surface owns its undo stack (UIKit doesn't record typing on
    /// it): every change is registered here, so undo never replays a stale
    /// range. Typing coalesces into one step per run of keystrokes.
    @ObservationIgnored private var typingGroupOpen = false

    var undo: UndoManager? { textView.undoManager }
    var typingRunHasActions: Bool { typingGroupOpen }

    /// One undoable step (inside an undo or redo, the manager groups it).
    private func undoStep(_ register: (UndoManager) -> Void) {
        guard let um = undo else { return }
        if um.isUndoing || um.isRedoing {
            register(um)
            return
        }
        closeTyping()
        um.beginUndoGrouping()
        register(um)
        um.endUndoGrouping()
    }

    /// The typing run ends (a caret jump, a command, a word typed).
    func closeTyping() {
        guard typingGroupOpen, let um = undo else { return }
        typingGroupOpen = false
        if um.groupingLevel > 0 { um.endUndoGrouping() }
    }

    /// A keystroke UIKit applied: `range` (before) became `length` characters.
    private func registerTyping(_ range: NSRange, old: NSAttributedString, length: Int, selectionBefore: NSRange) {
        guard let um = undo, !um.isUndoing, !um.isRedoing else { return }
        // (an undo closes an open group itself)
        if !typingGroupOpen || um.groupingLevel == 0 {
            um.beginUndoGrouping()
            typingGroupOpen = true
        }
        let newRange = NSRange(location: range.location, length: max(0, length))
        register(UndoRecord(range: newRange, replacement: old, selection: selectionBefore, redoSelection: textView.selectedRange), on: um)
    }

    /// One registered undo (or redo) action. It is its own target, so a
    /// refresh can shift it or drop just it (`remapUndo`).
    final class UndoRecord {
        var range: NSRange
        let replacement: NSAttributedString
        var selection: NSRange
        var redoSelection: NSRange
        /// registered while undoing: a redo action
        var isRedo = false

        init(range: NSRange, replacement: NSAttributedString, selection: NSRange, redoSelection: NSRange) {
            self.range = range
            self.replacement = replacement
            self.selection = selection
            self.redoSelection = redoSelection
        }
    }

    /// live records, oldest first (the manager doesn't keep its targets)
    @ObservationIgnored private var records: [UndoRecord] = []

    private func register(_ r: UndoRecord, on um: UndoManager) {
        r.isRedo = um.isUndoing
        records.append(r)
        um.registerUndo(withTarget: r) { [weak self] rec in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.records.removeAll { $0 === rec }
                self.closeTyping()
                self.apply(Surface.Edit(range: rec.range, replacement: rec.replacement, selection: rec.selection), undoSelection: rec.redoSelection)
            }
        }
    }

    /// A refresh replaced `span` (current text) with `newLength` characters.
    /// Undo steps that don't touch it survive (shifted past it); the first
    /// one that does, and every older one, go (they would undo text that
    /// isn't there any more). Redo steps go.
    func remapUndo(span: NSRange, newLength: Int) {
        closeTyping()
        guard let um = undo else { return }
        // ids the refreshed text now holds: a step that would put one back
        // (a block the refresh resurrected) can't run either
        var fresh: Set<BlockID> = []
        let st = textView.textStorage
        let newSpan = NSRange(location: span.location, length: min(newLength, max(0, st.length - span.location)))
        if newSpan.length > 0 {
            st.enumerateAttribute(Surface.block, in: newSpan) { v, _, _ in if let id = v as? String { fresh.insert(id) } }
        }
        func carries(_ r: UndoRecord) -> Bool {
            guard r.replacement.length > 0, !fresh.isEmpty else { return false }
            var hit = false
            r.replacement.enumerateAttribute(Surface.block, in: NSRange(location: 0, length: r.replacement.length)) { v, _, stop in
                if let id = v as? String, fresh.contains(id) { hit = true; stop.pointee = true }
            }
            return hit
        }
        func drop(_ r: UndoRecord) {
            um.removeAllActions(withTarget: r)
            records.removeAll { $0 === r }
        }
        for r in records where r.isRedo { drop(r) }
        var x = span
        let delta = newLength - span.length
        var dead = false
        for r in records.reversed() {
            if dead || carries(r) { dead = true; drop(r); continue }
            let end = r.range.location + r.range.length
            if end <= x.location {
                // undoing r shifts where the refreshed text sits
                x.location += r.replacement.length - r.range.length
            } else if r.range.location >= x.location + x.length {
                r.range.location += delta
                if r.selection.location >= x.location + x.length { r.selection.location += delta }
                if r.redoSelection.location >= x.location + x.length { r.redoSelection.location += delta }
            } else {
                dead = true
                drop(r)
            }
        }
    }

    /// A change nothing recorded (an input method's composition): the
    /// history no longer lines up with the text.
    func forgetUndo() {
        closeTyping()
        undo?.removeAllActions()
        records = []
    }

    /// The text changed: save after a pause.
    func changed() {
        // the children list follows the text's new height
        textView.footerPlacementDirty = true
        textView.setNeedsLayout()
        guard editable, !quiet else { return }
        unsaved = 1
        saver.touch(0)
    }

    /// The text view's own change (typing, autocorrect, an undo of typing):
    /// normalise and restyle the lines around it.
    func didChangeText() {
        guard !reloading else { return }
        if textView.markedTextRange != nil {
            changed()
            return
        }
        let storage = textView.textStorage
        var sel = textView.selectedRange
        let typed = typedRange
        typedRange = nil
        let keystroke = pendingTyping
        pendingTyping = nil
        reloading = true
        storage.beginEditing()
        // typing in a line: only its lines can be out of shape; anything
        // else (an undo of typing): the whole doc is checked
        let reshaped = Surface.normalize(storage, selection: &sel, around: typed)
        if let typed {
            SurfaceLook.restyle(storage, range: NSRange(location: typed.location, length: max(typed.length, 1)), ctx: lookContext)
        } else if reshaped {
            SurfaceLook.restyle(storage, ctx: lookContext)
        } else if let l = Surface.line(at: min(sel.location, max(0, storage.length - 1)), in: storage) {
            SurfaceLook.restyle(storage, range: l.range, ctx: lookContext)
        }
        storage.endEditing()
        if textView.selectedRange != sel { textView.selectedRange = sel }
        reloading = false
        if let k = keystroke {
            registerTyping(k.range, old: k.old, length: k.range.length + storage.length - k.length, selectionBefore: k.selection)
            // a word ends a step
            if lastTyped == " " { closeTyping() }
        } else {
            forgetUndo()
        }
        // a markdown shortcut just typed (`# `, `- `, …)
        if lastTyped == " ", let e = Surface.shortcut(storage, at: textView.selectedRange.location) {
            lastTyped = nil
            perform(e)
        }
        lastTyped = nil
        updateTyping()
        updateCompletion()
        changed()
    }

    /// What typing inserts next.
    func updateTyping() {
        guard editable, textView.textStorage.length > 0 else { return }
        let loc = min(textView.selectedRange.location, textView.textStorage.length - 1)
        let a = SurfaceLook.typing(textView.textStorage, at: loc, pending: pendingMarks, ctx: lookContext)
        textView.typingAttributes = a
        let m = InlineMarks(rawValue: (a[Surface.marks] as? Int) ?? 0)
        if m != activeMarks { activeMarks = m }
        textView.accessibilityHint = reviewHint(for: (a[Surface.block] as? String))
        formattingBar.update(for: currentBlock?.content ?? .paragraph(AttributedString()), marks: m)
    }

    /// Whether the doc has any text or block (an empty doc is one empty line).
    var hasContent: Bool { textView.textStorage.length > 1 }

    /// The block under the caret, found locally (no whole-doc scan).
    var currentBlock: Surface.SurfaceBlock? {
        let s = textView.textStorage
        guard s.length > 0 else { return nil }
        return Surface.block(at: min(textView.selectedRange.location, s.length - 1), in: s)
    }

    // MARK: keys and commands

    func handleReturn(_ range: NSRange) {
        if range.length > 0 { perform(Surface.replace(textView.textStorage, range, with: "")) }
        let before = currentBlock
        perform(Surface.returnKey(textView.textStorage, at: textView.selectedRange.location))
        // ``` + Return made a code block: its source opens at once
        if let b = before, !b.content.isRaw,
           Surface.blocks(textView.textStorage).first(where: { $0.id == b.id })?.content.isRaw == true {
            editSource(b.id)
        }
    }

    /// Backspace with an empty selection: at a line's start, the block
    /// command; inside a wikilink, the whole link.
    func handleBackspace() -> Bool {
        guard editable, textView.markedTextRange == nil else { return false }
        let sel = textView.selectedRange
        guard sel.length == 0 else { return false }
        let s = textView.textStorage
        if let e = Surface.backspace(s, at: sel.location) { return perform(e) }
        if sel.location > 0, s.attribute(Surface.wiki, at: sel.location - 1, effectiveRange: nil) != nil {
            var eff = NSRange()
            _ = s.attribute(Surface.wiki, at: sel.location - 1, longestEffectiveRange: &eff, in: NSRange(location: 0, length: s.length))
            return perform(Surface.replace(s, eff, with: ""))
        }
        return false
    }

    /// Paste: markdown into an empty line becomes blocks; anything else
    /// is plain text in the line's look (lines become paragraphs).
    /// a short note at the bottom of the doc, gone after a few seconds
    var notice: String?

    /// Paste: markdown becomes blocks at a line's start and keeps its
    /// inline formatting inside a line (`Surface.paste`); rich text from
    /// another app (Safari) comes as markdown with its links; an image
    /// says it can't be pasted here (yet).
    func handlePaste() -> Bool {
        guard editable else { return false }
        let pb = UIPasteboard.general
        if let e = pasteCutBlocks() { return perform(e) }
        var text = pb.string
        if text == nil || !pb.contains(pasteboardTypes: [Self.markdownType]),
           let rich = RichPaste.markdown(from: pb), !rich.isEmpty, rich != text {
            text = rich
        }
        guard let text else {
            if pb.hasImages { notice = "Images can't be pasted into a doc here yet." }
            return true
        }
        let sel = textView.selectedRange
        let s = textView.textStorage
        if sel.length > 0 {
            // what's selected goes first, then the paste lands at its start
            perform(Surface.replace(s, Surface.expandDeletion(sel, in: s), with: "", typing: textView.typingAttributes))
        }
        return perform(Surface.paste(textView.textStorage, textView.selectedRange, markdown: text, typing: textView.typingAttributes))
    }

    static let markdownType = "net.daringfireball.markdown"

    /// The selection on the pasteboard as markdown (plain text and the
    /// markdown type): structure, bold and links survive into another
    /// doc, and the web or Slack get readable markdown.
    func copyMarkdown(_ r: NSRange) -> String {
        let md = Surface.markdown(of: textView.textStorage, range: r)
        UIPasteboard.general.items = [[UTType.plainText.identifier: md, UTType.utf8PlainText.identifier: md, Self.markdownType: md]]
        return md
    }

    /// The selection as markdown-ish plain text: non-text blocks as their
    /// source, markers dropped, soft breaks as line breaks.
    func plainText(_ r: NSRange) -> String {
        let s = textView.textStorage
        var out = ""
        s.enumerateAttributes(in: r) { a, run, _ in
            if a[Surface.marker] != nil { return }
            let t = (s.string as NSString).substring(with: run)
            if t == Surface.attachment, let src = a[Surface.raw] as? String { out += src; return }
            out += t.replacingOccurrences(of: Surface.softBreak, with: "\n")
        }
        return out
    }

    func handleCopy() -> Bool {
        let r = textView.selectedRange
        guard r.length > 0 else { return false }
        _ = copyMarkdown(r)
        return true
    }

    /// Whole blocks cut here: pasted back unchanged into this doc they keep
    /// their ids (a move, so comments and ^refs survive), not delete + insert.
    @ObservationIgnored private var cutBlocks: (text: String, lines: NSAttributedString, changeCount: Int)?

    func handleCut() -> Bool {
        guard editable else { return false }
        let s = textView.textStorage
        let r = Surface.expandDeletion(textView.selectedRange, in: s)
        guard r.length > 0 else { return false }
        let text = copyMarkdown(textView.selectedRange)
        let ns = s.mutableString
        let startsLine = r.location == 0 || ns.character(at: r.location - 1) == 10
        let endsLine = ns.character(at: r.location + r.length - 1) == 10
        cutBlocks = startsLine && endsLine
            ? (text, s.attributedSubstring(from: r), UIPasteboard.general.changeCount)
            : nil
        return perform(Surface.replace(s, r, with: ""))
    }

    /// Paste of the blocks just cut, unchanged, at a line's start: the
    /// lines go back with their ids. Nil when this isn't that paste.
    private func pasteCutBlocks() -> Surface.Edit? {
        guard let c = cutBlocks, UIPasteboard.general.changeCount == c.changeCount, UIPasteboard.general.string == c.text else { return nil }
        let s = textView.textStorage
        let sel = textView.selectedRange
        guard sel.length == 0, let line = Surface.line(at: sel.location, in: s), sel.location <= line.textRange.location,
              // at a block's first line only: between two items it would split the list
              Surface.block(at: sel.location, in: s)?.range.location == line.range.location else { return nil }
        let ids = Set(Surface.blocks(c.lines).map(\.id))
        // only while those ids aren't back already (a second paste is new text)
        guard Surface.blocks(s).allSatisfy({ !ids.contains($0.id) }) else { return nil }
        cutBlocks = nil
        let empty = line.textRange.length == 0 && line.style == .paragraph && !baseline.contains { $0.id == line.id }
        let at = NSRange(location: line.range.location, length: empty ? line.range.length : 0)
        return Surface.Edit(range: at, replacement: c.lines, selection: NSRange(location: line.range.location + c.lines.length - 1, length: 0))
    }

    func toggle(_ mark: InlineMarks) {
        guard editable else { return }
        let s = textView.textStorage
        let r = textView.selectedRange
        if r.length == 0 {
            let current = InlineMarks(rawValue: (textView.typingAttributes[Surface.marks] as? Int) ?? 0)
            pendingMarks = current.symmetricDifference(mark)
            updateTyping()
            return
        }
        let m = NSMutableAttributedString(attributedString: s.attributedSubstring(from: r))
        let all = NSRange(location: 0, length: m.length)
        var every = true
        m.enumerateAttributes(in: all) { a, sub, _ in
            if a[Surface.marker] != nil || a[Surface.raw] != nil || (m.string as NSString).substring(with: sub) == "\n" { return }
            if !InlineMarks(rawValue: (a[Surface.marks] as? Int) ?? 0).contains(mark) { every = false }
        }
        m.enumerateAttributes(in: all) { a, sub, _ in
            if a[Surface.marker] != nil || a[Surface.raw] != nil { return }
            var marks = InlineMarks(rawValue: (a[Surface.marks] as? Int) ?? 0)
            if every { marks.remove(mark) } else { marks.insert(mark) }
            if marks.isEmpty { m.removeAttribute(Surface.marks, range: sub) } else { m.addAttribute(Surface.marks, value: marks.rawValue, range: sub) }
        }
        perform(Surface.Edit(range: r, replacement: m, selection: r))
    }

    func link() {
        guard editable else { return }
        let r = textView.selectedRange
        if r.length == 0 {
            insertWikiOpener()
            return
        }
        askForLink { [weak self] url in
            guard let self, self.editable, let url, !url.isEmpty else { return }
            let m = NSMutableAttributedString(attributedString: self.textView.textStorage.attributedSubstring(from: r))
            m.addAttribute(Surface.link, value: url, range: NSRange(location: 0, length: m.length))
            self.perform(Surface.Edit(range: r, replacement: m, selection: NSRange(location: r.location + r.length, length: 0)))
        }
    }

    func insertWikiOpener() {
        guard editable else { return }
        perform(Surface.replace(textView.textStorage, textView.selectedRange, with: "[[", typing: textView.typingAttributes))
        updateCompletion()
    }

    func indent(_ by: Int) {
        guard editable else { return }
        if let b = currentBlock, b.content.isRaw { return }
        perform(Surface.transform(textView.textStorage, at: textView.selectedRange.location) { c, caret in
            by > 0 ? EditorCommands.indent(c, at: caret) : EditorCommands.outdent(c, at: caret)
        })
    }

    func setKind(_ choice: BlockKindChoice) {
        guard editable else { return }
        perform(Surface.transform(textView.textStorage, at: textView.selectedRange.location) { c, caret in
            EditorCommands.convert(c, to: choice, caret: caret).map { .update($0.0, caret: $0.1) } ?? .none
        })
    }

    func toggleTodo() {
        guard editable else { return }
        perform(Surface.transform(textView.textStorage, at: textView.selectedRange.location) { c, caret in
            EditorCommands.toggleTodo(c, at: caret).map { .update($0.0, caret: $0.1) } ?? .none
        })
    }

    /// Tick a list line's box.
    func setChecked(line li: Int, _ checked: Bool) {
        let s = textView.textStorage
        let ls = Surface.lines(s)
        guard ls.indices.contains(li) else { return }
        if let todo = onTodoCheckbox, let id = ls[li].id {
            // the To-do doc's boxes are to-do items: the to-do API writes them
            let first = ls.firstIndex { $0.id == id } ?? li
            let index = ls[first..<li].filter { $0.style.prefix?.checkbox != nil }.count
            todo(id, index, checked)
            guard let e = Surface.setChecked(s, line: li, checked, selection: textView.selectedRange) else { return }
            quiet = true // shown at once, never proposed from here
            perform(e)
            quiet = false
            // the baseline takes the tick only: anything else typed in the
            // list is still unsaved and still saves
            if let k = baseline.firstIndex(where: { $0.id == id }),
               let ticked = Checkbox.toggled(baseline[k].content, index: index, checked: checked) {
                baseline[k].content = EditorBlockContent.parse(markdown: ticked).markdown
            }
            return
        }
        guard editable else { return }
        perform(Surface.setChecked(s, line: li, checked, selection: textView.selectedRange))
    }

    // MARK: non-text blocks

    func editSource(_ id: BlockID) {
        guard editable else { return }
        sourceEditing = id
    }

    func source(of id: BlockID) -> String {
        Surface.blocks(textView.textStorage).first { $0.id == id }?.content.markdown ?? ""
    }

    /// The sheet saved: the block takes the new markdown (text, if it now is text).
    /// False when the block is gone (a refresh removed it meanwhile).
    @discardableResult
    func saveSource(_ id: BlockID, _ markdown: String) -> Bool {
        guard editable else { return false }
        let content = EditorBlockContent.parse(markdown: markdown)
        guard let e = Surface.replaceBlock(textView.textStorage, id: id, with: content, selection: textView.selectedRange) else { return false }
        return perform(e)
    }

    /// Markdown put in as new blocks at the caret's line (a block's source
    /// whose block was removed meanwhile, kept by choice).
    func insertBlocks(_ markdown: String) {
        guard editable else { return }
        let s = textView.textStorage
        let at = Surface.line(at: min(textView.selectedRange.location, max(0, s.length - 1)), in: s)?.range.location ?? 0
        perform(Surface.paste(s, NSRange(location: at, length: 0), markdown: markdown.contains("\n\n") || markdown.hasPrefix("```") ? markdown : markdown + "\n\n"))
    }

    /// Tick a box inside a non-text block (a callout, a quote, a complex
    /// list): its markdown, rewritten; no sheet, no keyboard.
    func toggleRawCheckbox(_ id: BlockID, index: Int, checked: Bool) {
        guard editable, let toggled = Checkbox.toggled(source(of: id), index: index, checked: checked) else { return }
        saveSource(id, toggled)
    }

    func deleteBlock(_ id: BlockID) {
        guard editable else { return }
        guard let b = Surface.blocks(textView.textStorage).first(where: { $0.id == id }) else { return }
        perform(Surface.Edit(range: b.range, replacement: NSAttributedString(), selection: NSRange(location: b.range.location, length: 0)))
    }

    // MARK: links

    /// Open a link: a wikilink in the app, anything else in the system.
    @discardableResult
    func follow(_ url: URL) -> Bool {
        if let target = InlineMarkdown.wikiTarget(url) {
            onOpenWiki(target)
            return true
        }
        UIApplication.shared.open(url)
        return true
    }

    /// The link under a location, if any.
    func link(at loc: Int) -> URL? {
        let s = textView.textStorage
        guard loc >= 0, loc < s.length else { return nil }
        let a = s.attributes(at: loc, effectiveRange: nil)
        return SurfaceLook.url(link: a[Surface.link] as? String, wiki: a[Surface.wiki] as? String)
    }

    func askForLink(_ done: @escaping (String?) -> Void) {
        let alert = UIAlertController(title: "Link", message: nil, preferredStyle: .alert)
        alert.addTextField { f in
            f.placeholder = "https://"
            f.keyboardType = .URL
            f.autocapitalizationType = .none
            f.autocorrectionType = .no
        }
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel) { _ in done(nil) })
        alert.addAction(UIAlertAction(title: "Add link", style: .default) { [weak alert] _ in
            done(alert?.textFields?.first?.text?.trimmingCharacters(in: .whitespaces))
        })
        var top = textView.window?.rootViewController
        while let p = top?.presentedViewController { top = p }
        top?.present(alert, animated: true)
    }

    // MARK: [[ completion

    func updateCompletion() {
        let tv = textView
        guard editable, tv.isFirstResponder, tv.selectedRange.length == 0, tv.markedTextRange == nil else {
            if completion != nil { completion = nil }
            return
        }
        let s = tv.textStorage
        guard let l = Surface.line(at: tv.selectedRange.location, in: s) else { return }
        let loc = tv.selectedRange.location
        guard loc >= l.textRange.location else { completion = nil; return }
        let before = (s.string as NSString).substring(with: NSRange(location: l.textRange.location, length: loc - l.textRange.location))
        guard let q = WikiCompletion.query(before: before) else {
            if completion != nil { completion = nil }
            return
        }
        var anchor = CGRect.zero
        if let r = tv.selectedTextRange { anchor = tv.convert(tv.caretRect(for: r.end), to: nil) }
        let results = WikiCompletion.rank(q, in: candidates)
        let selected = completion?.query == q ? (completion?.selected ?? 0) : 0
        completion = Completion(query: q, anchor: anchor, results: results, selected: min(selected, max(0, results.count - 1)))
    }

    func moveCompletion(_ by: Int) {
        guard var c = completion, !c.results.isEmpty else { return }
        c.selected = (c.selected + by + c.results.count) % c.results.count
        completion = c
    }

    func dismissCompletion() { completion = nil }

    func acceptCompletion(_ pick: WikiCandidate? = nil) {
        guard let c = completion else { return }
        completion = nil
        guard let chosen = pick ?? (c.results.indices.contains(c.selected) ? c.results[c.selected] : nil) else { return }
        insertWikiLink(WikiCompletion.target(for: chosen, among: candidates), query: c.query)
    }

    /// Replace `[[query` before the caret with the wikilink and a space.
    func insertWikiLink(_ target: String, query: String) {
        let s = textView.textStorage
        let loc = textView.selectedRange.location
        let len = (query as NSString).length + 2
        guard loc >= len else { return }
        let range = NSRange(location: loc - len, length: len)
        var a = Surface.typingAttributes(s, at: range.location)
        a[Surface.wiki] = target
        a[Surface.link] = nil
        let out = NSMutableAttributedString(string: InlineCodec.wikiDisplay(target), attributes: a)
        a[Surface.wiki] = nil
        out.append(NSAttributedString(string: " ", attributes: a))
        perform(Surface.Edit(range: range, replacement: out, selection: NSRange(location: range.location + out.length, length: 0)))
    }

    // MARK: saving

    /// The debounce ran out (or a flush): diff the surface against the
    /// baseline and queue the ops. Ops on a block in conflict go in their
    /// own propose on its older epoch, flagged.
    func save(final: Bool = false) {
        guard editable else { unsaved = 0; return }
        // an input method's composition isn't text yet: save once it commits
        if textView.markedTextRange != nil {
            saver.touch(0)
            return
        }
        unsaved = 0
        let entries = Surface.entries(textView.textStorage, known: Set(baseline.map(\.id)))
        let ops = DocDiff.computeOps(baseline: baseline, entries: entries, hidden: hidden)
        guard !ops.isEmpty else { return }
        var normal: [BlockOp] = []
        var disputed: [BlockID: [BlockOp]] = [:]
        for op in ops {
            if let t = op.target, conflicts[t] != nil { disputed[t, default: []].append(op) } else { normal.append(op) }
        }
        var held: Set<BlockID> = []
        for (t, o) in disputed.sorted(by: { $0.key < $1.key }) {
            guard let base = conflicts[t]?.base else { continue }
            let text = String(describing: o)
            if let w = conflictWrites[t] {
                // the same text again (a refresh moved the baseline): nothing to send
                if w.text == text { continue }
                if !w.key.isEmpty {
                    // still waiting to go: rewrite it; gone already: wait (or, final, send anew)
                    writeConflict(t, ops: o, base: base, text: text, rewriting: w.key, final: final)
                    continue
                }
                if !final {
                    held.insert(t)
                    continue
                }
            }
            writeConflict(t, ops: o, base: base, text: text, rewriting: nil, final: final)
        }
        if !normal.isEmpty {
            let single: Bool = if normal.count == 1, case .replace = normal[0] { true } else { false }
            enqueue(ProposeRequest(docID: docID, baseEpoch: baseEpoch, ops: normal, requestID: Surface.newID()), coalescing: single)
        }
        // a held block stays as it was in the baseline, so it still differs
        let before = Dictionary(baseline.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        baseline = DocDiff.rebuildBaseline(baseline, entries: entries, ops: ops.filter { $0.target.map { !held.contains($0) } ?? true })
        for t in held { if let k = baseline.firstIndex(where: { $0.id == t }), let old = before[t] { baseline[k] = old } }
    }

    /// One disputed block's flagged write, on its conflict's epoch: the
    /// pending row rewritten in place if it's still waiting; if it has gone
    /// out, a new row only on a final flush (otherwise the text waits).
    private func writeConflict(_ t: BlockID, ops: [BlockOp], base: Int, text: String, rewriting: String?, final: Bool) {
        let previous = conflictWrites[t]
        let req = ProposeRequest(docID: docID, baseEpoch: base, ops: ops, requestID: Surface.newID())
        conflictWrites[t] = (rewriting ?? req.requestID ?? "", text)
        let baselineBefore = baseline.first { $0.id == t }
        enqueue(req, coalescing: false, conflict: true) { [weak self] in
            guard let self, let cache = self.cache, let key = rewriting else { return false }
            if (try? await cache.rewriteUnsentPropose(key: key, ops: ops)) == true { return true }
            if final {
                // gone out: this one is new
                self.conflictWrites[t] = (req.requestID ?? "", text)
                return false
            }
            // gone out meanwhile: wait for the final flush, still dirty
            self.conflictWrites[t] = previous.map { ("", $0.text) }
            if let b = baselineBefore, let k = self.baseline.firstIndex(where: { $0.id == t }) { self.baseline[k] = b }
            return true
        }
    }

    /// How a propose is written to the outbox (tests swap it to fail).
    @ObservationIgnored lazy var persist: (ProposeRequest, Bool, Bool) async throws -> Void = { [cache] req, coalescing, conflict in
        guard let cache else { return }
        if conflict { try await cache.enqueue(req, conflict: true) } else if coalescing { try await cache.enqueueCoalescing(req) } else { try await cache.enqueue(req) }
    }
    /// waits between attempts at a failing outbox write
    @ObservationIgnored var persistRetryDelays: [Duration] = [.milliseconds(200), .seconds(1), .seconds(3)]
    @ObservationIgnored private(set) var abandoned: [ProposeRequest] = []
    /// queued proposes not yet written to the outbox, oldest first: a
    /// refresh overlays them, so nothing saved is rolled back on screen
    @ObservationIgnored private(set) var unpersisted: [ProposeRequest] = []
    /// how queued writes get sent (the app's outbox replay; tests swap it)
    @ObservationIgnored var replay: @MainActor () async -> Void = {}

    /// Writes reach the outbox in the order they were made; sending them
    /// is a separate, unawaited step.
    private func enqueue(_ req: ProposeRequest, coalescing: Bool, conflict: Bool = false,
                         handled: (@MainActor () async -> Bool)? = nil) {
        let prev = queue
        unpersisted.append(req)
        queue = Task { [weak self] in
            await prev?.value
            guard let self else { return }
            // already written another way (a rewrite in place) or held
            if let handled, await handled() {
                self.unpersisted.removeAll { $0.requestID == req.requestID }
                return
            }
            var attempt = 0
            while true {
                do {
                    try await self.persist(req, coalescing, conflict)
                    break
                } catch {
                    guard attempt < self.persistRetryDelays.count else {
                        self.unpersisted.removeAll { $0.requestID == req.requestID }
                        self.abandoned.append(req)
                        self.persistFailures = self.abandoned.count
                        self.lastError = "Couldn't save: \(error.localizedDescription)"
                        return
                    }
                    try? await Task.sleep(for: self.persistRetryDelays[attempt])
                    attempt += 1
                }
            }
            self.unpersisted.removeAll { $0.requestID == req.requestID }
            self.send()
        }
    }

    private func send() {
        let replay = replay
        Task { [weak self] in
            await replay()
            await self?.poll()
        }
    }

    /// Everything typed, written to the outbox before this returns (never
    /// waits on the network): leaving the doc, backgrounding, a demotion.
    func flush(final: Bool = false) async {
        if textView.markedTextRange != nil { textView.unmarkText() }
        let retry = abandoned
        abandoned = []
        persistFailures = 0
        for r in retry { enqueue(r, coalescing: false) }
        await saver.flush()
        save(final: final)
        await queue?.value
        unsaved = 0
    }

    // MARK: remote changes

    // MARK: hooks for tests (the same paths the app takes)

    /// The cached body changed (what the cache observer does).
    func refreshFromCache() async { await remoteChanged() }
    /// The review queue as if just fetched from the server.
    func setReviewQueue(_ ops: Set<String>) { openOps = ops }
    /// Every queued outbox write has landed in the outbox.
    func settle() async { await queue?.value }

    /// The cached body was rewritten (sync): merge it in.
    private func remoteChanged() async {
        guard let cache else { return }
        if let landed = try? await cache.lastLandedEpoch(docID), let rec = try? await cache.doc(docID),
           let body = rec.bodyEpoch, body < landed { return }
        let before = abandoned + unpersisted
        guard var fresh = try? await cache.editor(for: docID) else { return }
        // our edits parked for review stay on screen as ours, in conflict
        // on their propose's epoch, until a human decides. Only from a
        // review queue the server gave us: unknown (offline, not fetched
        // yet, a failed fetch with nothing known) overlays nothing
        if let open = openOps, let parked = try? await cache.parkedOps(for: docID, openOps: open) {
            var c = conflicts
            parkedBlocks = ParkedEdits.apply(parked, on: &fresh, conflicts: &c, parkedBefore: parkedBlocks, released: released)
            // a decision on a released block: nothing to hold back any more
            released = released.filter { b in parked.contains { $0.block == b } }
            if c != conflicts { conflicts = c }
        }
        // what the server itself has for blocks in conflict (its body, no
        // overlay of our queued writes): only that ends a conflict
        var own: [BlockID: String] = [:]
        if !conflicts.isEmpty, let records = try? await cache.blocks(of: docID) {
            for r in records where conflicts[r.block.id] != nil { own[r.block.id] = Surface.baselineContent(r.block) }
        }
        var seen: Set<String> = []
        for r in before + abandoned + unpersisted where seen.insert(r.requestID ?? "").inserted {
            fresh.overlay(r.ops.map(\.kind))
        }
        applyRemote(fresh, serverText: own)
    }

    /// Take a server view of the doc: untouched blocks take theirs, edited
    /// ones keep ours (in conflict when both changed), the caret and the
    /// scroll position stay.
    func applyRemote(_ fresh: DocEditor, serverText: [BlockID: String]? = nil) {
        guard textView.markedTextRange == nil else {
            // never under an open composition: after it
            pendingRemote = fresh
            return
        }
        pendingRemote = nil
        let storage = textView.textStorage
        let current = Surface.blocks(storage)
        let server = editable ? fresh.ordered().map(\.block).filter { !Surface.isHidden($0) }
            : fresh.ordered().map(\.block).filter { $0.blockType != .comment && !$0.content.hasPrefix("---") }
        for b in server { blockTypes[b.id] = b.blockType }
        let r = SurfaceMerge.merge(local: current.map { ($0.id, $0.content) }, baseline: baseline, conflicts: conflicts,
                                   oldEpoch: baseEpoch, server: server, parked: parkedBlocks, serverText: serverText)
        baseline = r.baseline
        hidden = Surface.hidden(fresh)
        // a conflict that ended takes its flagged-write record with it
        for b in conflictWrites.keys where r.conflicts[b] == nil { conflictWrites[b] = nil }
        if r.conflicts != conflicts { conflicts = r.conflicts }
        baseEpoch = fresh.baseEpoch
        guard r.changed else { return }
        rebuild(r.blocks, from: current)
    }

    /// Replace the blocks that differ, keeping the caret (by block and
    /// offset) and the scroll position. Undo history ends here: its ranges
    /// no longer line up.
    private func rebuild(_ pieces: [SurfaceMerge.Piece], from current: [Surface.SurfaceBlock]) {
        let storage = textView.textStorage
        let byID = Dictionary(current.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        // unchanged runs at both ends stay as they are
        func same(_ p: SurfaceMerge.Piece, _ b: Surface.SurfaceBlock) -> Bool { p.local && p.from == nil && p.id == b.id }
        var head = 0
        while head < pieces.count, head < current.count, same(pieces[head], current[head]) { head += 1 }
        var tail = 0
        while tail < pieces.count - head, tail < current.count - head,
              same(pieces[pieces.count - 1 - tail], current[current.count - 1 - tail]) { tail += 1 }
        let start = head < current.count ? current[head].range.location : storage.length
        let end = tail > 0 ? current[current.count - tail].range.location : storage.length
        let old = NSRange(location: start, length: end - start)
        let sel = textView.selectedRange
        let caretBlock = current.first { NSLocationInRange(sel.location, $0.range) }
        let out = NSMutableAttributedString()
        var caret: Int?
        for p in pieces[head..<(pieces.count - tail)] {
            let at = start + out.length
            if p.local, let b = byID[p.from ?? p.id] {
                let sub = NSMutableAttributedString(attributedString: storage.attributedSubstring(from: b.range))
                if p.from != nil { sub.addAttribute(Surface.block, value: p.id, range: NSRange(location: 0, length: sub.length)) }
                out.append(sub)
            } else {
                out.append(Surface.render(p.content, id: p.id))
            }
            if let cb = caretBlock, cb.id == (p.from ?? p.id) {
                caret = at + min(sel.location - cb.range.location, max(0, start + out.length - at - 1))
            }
        }
        let offset = textView.contentOffset
        reloading = true
        storage.beginEditing()
        storage.replaceCharacters(in: old, with: out)
        var s = NSRange(location: caret ?? Surface.shifted(sel, by: old, newLength: out.length).location, length: 0)
        if caret == nil, sel.location < old.location { s = sel }
        let lengthBefore = storage.length
        Surface.normalize(storage, selection: &s, around: NSRange(location: old.location, length: out.length))
        let outLength = out.length + storage.length - lengthBefore
        SurfaceLook.restyle(storage, range: NSRange(location: old.location, length: outLength), ctx: lookContext)
        storage.endEditing()
        textView.selectedRange = s
        textView.contentOffset = offset
        reloading = false
        // undo outside the refreshed blocks survives
        remapUndo(span: old, newLength: outLength)
        updateTyping()
    }

    /// After an input-method composition ends, a refresh that waited for it.
    func compositionEnded() {
        if let p = pendingRemote { applyRemote(p) }
    }

    // MARK: status

    /// The outbox state, refusals and review marks; the review queue from
    /// the server (at once the first time, then every 15 s, each fetch
    /// bounded to 10 s). True when anything changed.
    @discardableResult
    func poll() async -> Bool {
        guard let cache else { return false }
        let before = (reviews, failedBlocks, outbox, openOps)
        if let s = try? await cache.outboxState(for: docID), s != outbox { outbox = s }
        if let f = try? await cache.failedBlocks(docID), f != failedBlocks { failedBlocks = f }
        if let api, app?.isOnline == true, openOps == nil || Date().timeIntervalSince(lastReviewFetch) > 15 {
            lastReviewFetch = Date()
            // a failed or slow fetch keeps the last set the server gave us
            if let fetched = await Self.fetchReviewQueue(api, docID), fetched != openOps {
                openOps = fetched
                // a decision (or a new park) shows now, not at the next sync
                await remoteChanged()
            }
        }
        if let r = try? await cache.reviews(for: docID, openOps: openOps), r != reviews { reviews = r }
        if before.0 != reviews || before.1 != failedBlocks { restyle() }
        return before.0 != reviews || before.1 != failedBlocks || before.2 != outbox || before.3 != openOps
    }

    /// The doc's open review queue, or nil after 10 s (a captive network
    /// never holds the editor up).
    static func fetchReviewQueue(_ api: APIClient, _ docID: DocID, timeout: Duration = .seconds(10)) async -> Set<String>? {
        await withTaskGroup(of: Set<String>?.self) { group in
            group.addTask { try? await api.openReviewOps(docID) }
            group.addTask {
                try? await Task.sleep(for: timeout)
                return nil
            }
            let first = await group.next() ?? nil
            group.cancelAll()
            return first
        }
    }

    /// What VoiceOver says about the caret's block beyond its text: the
    /// colour of the tint, in words.
    func reviewHint(for id: BlockID?) -> String? {
        guard let id else { return nil }
        if failedBlocks.contains(id) { return "This block couldn't be saved." }
        if let c = conflicts[id] { return c.theirs != nil ? "Changed on another device while you edited it." : "Your edit here waits for review." }
        switch reviews[id]?.verdict {
        case .red?: return "Your edit here is waiting for review."
        case .yellow?: return "Your edit here was flagged for review."
        default: return nil
        }
    }

    /// The doc's title and what the header and children list offer, for
    /// VoiceOver on the text view (they are drawn inside it).
    func setAccessibilityChrome(title: String, actions: [(String, () -> Void)]) {
        textView.accessibilityLabel = title.isEmpty ? "Doc" : title
        textView.accessibilityCustomActions = actions.map { name, run in
            UIAccessibilityCustomAction(name: name) { _ in run(); return true }
        }
    }

    var chip: EditorChip {
        EditorChip.make(online: app?.isOnline ?? false, unsaved: unsaved, outbox: outbox, persistFailures: persistFailures,
                        reviews: reviews.values.map(\.verdict))
    }

    // MARK: conflicts and refusals

    /// The other side's text wins: our queued saves of the block go.
    func takeTheirs(_ id: BlockID) async {
        guard let theirs = conflicts[id]?.theirs else { return }
        await saver.flush()
        save()
        await queue?.value
        conflicts[id] = nil
        conflictWrites[id] = nil
        perform(Surface.replaceBlock(textView.textStorage, id: id, with: EditorBlockContent.parse(markdown: theirs), selection: textView.selectedRange))
        if let k = baseline.firstIndex(where: { $0.id == id }) { baseline[k].content = EditorBlockContent.parse(markdown: theirs).markdown }
        saver.cancelAll()
        unsaved = 0
        try? await cache?.discardPending(docID, block: id)
        try? await cache?.clearConflict(docID, block: id)
        restyle()
        await poll()
    }

    /// Ours wins, by choice: proposed on the current epoch.
    func keepMine(_ id: BlockID) async {
        guard let c = conflicts[id] else { return }
        await queue?.value
        conflicts[id] = nil
        conflictWrites[id] = nil
        // a parked edit settled here stays settled; a pending flagged write
        // for the block goes (the plain write below supersedes it)
        if parkedBlocks.contains(id) { released.insert(id) }
        try? await cache?.discardPending(docID, block: id)
        try? await cache?.clearConflict(docID, block: id)
        if let k = baseline.firstIndex(where: { $0.id == id }) { baseline[k].content = c.theirs ?? "\u{0}" }
        save()
        await queue?.value
        restyle()
    }

    /// Give up on refused saves: those blocks go back to the server's text.
    func discardFailed() async {
        let ids = failedBlocks
        await flush()
        try? await cache?.discardFailed(docID: docID)
        if let fresh = try? await cache?.editor(for: docID) {
            for id in ids {
                if let k = baseline.firstIndex(where: { $0.id == id }) { baseline[k].content = Surface.blocks(textView.textStorage).first { $0.id == id }?.content.markdown ?? "" }
                if let b = fresh.blocks[id] {
                    perform(Surface.replaceBlock(textView.textStorage, id: id, with: EditorBlockContent.parse(b), selection: textView.selectedRange))
                    if let k = baseline.firstIndex(where: { $0.id == id }) { baseline[k].content = Surface.baselineContent(b) }
                }
            }
            saver.cancelAll()
            unsaved = 0
        }
        await poll()
    }

    /// The chip's Retry: outbox writes that failed here, and saves the server refused.
    func retryFailed() async {
        if !abandoned.isEmpty { await flush() }
        try? await cache?.retryFailed(docID: docID)
        await app?.replayOutbox()
        await poll()
    }
}

/// Where the formatting bar lives: above the keyboard on iPhone and iPad,
/// pinned above the doc on the Mac (no software keyboard, and the Mac
/// idiom ignores input accessory views).
enum EditorChrome {
    static var barIsInline: Bool {
        #if targetEnvironment(macCatalyst)
        true
        #else
        false
        #endif
    }
}
