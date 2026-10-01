import Foundation
import Observation
import UIKit
import TaisceKit

/// Edit mode for one doc: the `EditorSession`, which block has the caret,
/// debounced saves through the outbox, the `[[` popup, and what the sync
/// chip says. Text views report here; structure changes come back as a
/// focus request and reloaded blocks.
@MainActor @Observable
final class EditorModel {
    struct FocusRequest: Equatable {
        var id: BlockID
        var caret: EditorCaret
        /// the block revision the caret refers to (a reload must land first)
        var revision: Int
    }

    struct Completion: Equatable {
        var blockID: BlockID
        var query: String
        /// the caret, in window coordinates
        var anchor: CGRect
        var results: [WikiCandidate]
        var selected: Int
    }

    let docID: DocID
    private(set) var session: EditorSession
    private(set) var focusedID: BlockID?
    var pendingFocus: FocusRequest?
    private(set) var completion: Completion?
    private(set) var reviews: [BlockID: BlockReview] = [:]
    private(set) var outbox = DocOutboxState()
    /// saves waiting on the debounce
    private(set) var unsaved = 0
    private(set) var activeMarks = InlineMarks()
    var lastError: String?

    @ObservationIgnored private let cache: Cache
    @ObservationIgnored private let api: APIClient?
    @ObservationIgnored private weak var app: AppModel?
    @ObservationIgnored private var saver: SaveScheduler<BlockID>!
    @ObservationIgnored private var candidates: [WikiCandidate] = []
    /// typed content not yet folded into the session (folding on every
    /// keystroke would re-render the whole list)
    @ObservationIgnored private var live: [BlockID: EditorBlockContent] = [:]
    @ObservationIgnored private var coordinators: [BlockID: Weak] = [:]
    @ObservationIgnored private var queue: Task<Void, Never>?
    @ObservationIgnored private var observeTask: Task<Void, Never>?
    @ObservationIgnored private var pollTask: Task<Void, Never>?
    @ObservationIgnored private var openOps: Set<String>?
    @ObservationIgnored private var lastReviewFetch = Date.distantPast
    @ObservationIgnored lazy var formattingBar: FormattingBar = FormattingBar(model: self)

    private struct Weak { weak var c: BlockTextCoordinator? }

    init(docID: DocID, editor: DocEditor, cache: Cache, api: APIClient?, app: AppModel?, saveDelay: Duration = .milliseconds(600)) {
        self.docID = docID
        session = EditorSession(editor: editor)
        self.cache = cache
        self.api = api
        self.app = app
        saver = SaveScheduler(delay: saveDelay) { [weak self] ids in self?.save(ids) }
        replay = { [weak app] in await app?.replayOutbox() }
        if let index = app?.index {
            candidates = index.docs.filter { $0.id != docID }.map { WikiCandidate(id: $0.id, title: $0.title, breadcrumb: index.breadcrumb(of: $0.id)) }
        }
    }

    /// Open the doc for editing: the cached body plus everything queued.
    static func open(_ docID: DocID, app: AppModel) async -> EditorModel? {
        guard let cache = app.cache else { return nil }
        if (try? await cache.doc(docID))?.bodyEpoch == nil { try? await app.sync?.refresh(docID) }
        guard let editor = try? await cache.editor(for: docID) else { return nil }
        let model = EditorModel(docID: docID, editor: editor, cache: cache, api: app.api, app: app)
        model.start()
        return model
    }

    var items: [EditorSession.Item] { session.items }

    /// Every block's content as typed (the session plus unfolded keystrokes).
    var snapshot: [EditorBlockContent] { session.items.map { live[$0.id] ?? $0.content } }

    /// The block the last structure change or move sent the caret to.
    @ObservationIgnored private(set) var focusTarget: BlockID?

    func start() {
        observeTask = Task { [weak self, cache, docID] in
            do {
                var first = true
                for try await _ in cache.observeBlocks(of: docID) {
                    if first { first = false; continue }
                    await self?.remoteChanged()
                }
            } catch {}
        }
        // saves poll after themselves (send()); this catches the rest
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.poll()
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    func stop() {
        observeTask?.cancel()
        pollTask?.cancel()
        saver.cancelAll()
    }

    // MARK: text views

    func register(_ c: BlockTextCoordinator) { coordinators[c.id] = Weak(c: c) }
    func unregister(_ c: BlockTextCoordinator) {
        if coordinators[c.id]?.c === c { coordinators[c.id] = nil }
        if focusedID == c.id { focusedID = nil }
    }

    func didFocus(_ c: BlockTextCoordinator) {
        focusedID = c.id
        formattingBar.update(for: c.content, marks: activeMarks)
    }

    func didBlur(_ c: BlockTextCoordinator) {
        if focusedID == c.id { focusedID = nil }
        if completion?.blockID == c.id { completion = nil }
        // leaving a block saves it now
        Task { await saver.flush() }
    }

    func caretMoved(_ c: BlockTextCoordinator) {
        formattingBar.update(for: c.content, marks: activeMarks)
    }

    func marksChanged(_ m: InlineMarks) {
        guard m != activeMarks else { return }
        activeMarks = m
        if let id = focusedID, let c = coordinators[id]?.c { formattingBar.update(for: c.content, marks: m) }
    }

    var activeCoordinator: BlockTextCoordinator? { focusedID.flatMap { coordinators[$0]?.c } }

    func textChanged(_ id: BlockID, content: EditorBlockContent) {
        live[id] = content
        saver.touch(id)
        unsaved = saver.waiting.count
    }

    /// Fold typed text into the session.
    private func foldLive() {
        for (id, c) in live { session.update(id, content: c) }
        live = [:]
    }

    // MARK: saving

    /// The debounce ran out for `ids` (or a flush): one propose for them.
    /// Blocks in conflict wait for an explicit flush.
    func save(_ ids: Set<BlockID>) {
        foldLive()
        let reqs = session.commitText(ids)
        unsaved = saver.waiting.count
        for r in reqs { enqueue(r, coalescing: true) }
    }

    /// Committed proposes not yet written to the outbox, oldest first: a
    /// refresh overlays them, so nothing committed is ever rolled back.
    @ObservationIgnored private(set) var unpersisted: [ProposeRequest] = []

    /// Writes reach the outbox in the order they were made. Only the
    /// local writes are chained: sending them is a separate, unawaited
    /// step, so a slow or hanging network never holds a save in memory.
    private func enqueue(_ req: ProposeRequest, coalescing: Bool) {
        let prev = queue
        let cache = cache
        unpersisted.append(req)
        queue = Task { [weak self] in
            await prev?.value
            do {
                if coalescing { try await cache.enqueueCoalescing(req) } else { try await cache.enqueue(req) }
            } catch {
                self?.lastError = "Couldn't queue the edit: \(error.localizedDescription)"
            }
            self?.unpersisted.removeAll { $0.requestID == req.requestID }
            self?.send()
        }
    }

    /// Replay the outbox in the background (single-flight in the app model).
    private func send() {
        let replay = replay
        Task { [weak self] in
            await replay()
            await self?.poll()
        }
    }

    /// How queued writes get sent (the app's outbox replay; tests swap it).
    @ObservationIgnored var replay: @MainActor () async -> Void = {}

    /// Every pending save, conflicted blocks included (on their older
    /// epoch), written to the outbox before this returns. Never waits on
    /// the network (Done, leaving the screen, the app going to the background).
    func flush() async {
        activeCoordinator?.syncContent()
        await saver.flush()
        foldLive()
        for r in session.commitText(includeConflicts: true) { enqueue(r, coalescing: true) }
        await queue?.value
        unsaved = 0
    }

    // MARK: structure

    private func applyChange(_ change: EditorSession.Change?) {
        guard let change else { return }
        if let req = change.request { enqueue(req, coalescing: false) }
        let rev = session.item(change.focus)?.revision ?? 0
        focusTarget = change.focus
        pendingFocus = FocusRequest(id: change.focus, caret: change.caret, revision: rev)
        // a block that keeps its view (no reload) takes the caret now
        if let c = coordinators[change.focus]?.c, !change.reload.contains(change.focus) {
            pendingFocus = nil
            c.focus(change.caret)
        } else {
            DispatchQueue.main.async { [weak self] in self?.deliverFocus() }
        }
    }

    private func deliverFocus() {
        guard let f = pendingFocus, let c = coordinators[f.id]?.c else { return }
        c.applyPendingFocus()
    }

    /// Everything waiting to save goes first, so the outbox keeps the
    /// order the edits were made in.
    private func prepareStructure() {
        foldLive()
        saver.cancelAll()
        for r in session.commitText() { enqueue(r, coalescing: true) }
        unsaved = 0
    }

    func returnKey(_ id: BlockID, caret: EditorCaret) {
        prepareStructure()
        applyChange(session.returnKey(id, caret: caret))
    }

    func backspaceAtStart(_ id: BlockID, caret: EditorCaret) {
        prepareStructure()
        applyChange(session.backspaceAtStart(id, caret: caret))
    }

    /// Set a block's content from a bar action and save it at once.
    private func update(_ id: BlockID, _ result: (EditorBlockContent, EditorCaret)?) {
        guard let (content, caret) = result else { return }
        prepareStructure()
        applyChange(session.apply(.update(content, caret: caret), to: id))
        for r in session.commitText([id]) { enqueue(r, coalescing: true) }
    }

    func indent(_ id: BlockID, caret: EditorCaret, by: Int) {
        foldLive()
        guard let item = session.item(id) else { return }
        let r = by > 0 ? EditorCommands.indent(item.content, at: caret) : EditorCommands.outdent(item.content, at: caret)
        if case let .update(c, at) = r { update(id, (c, at)) }
    }

    func setKind(_ choice: BlockKindChoice) {
        guard let c = activeCoordinator else { return }
        c.syncContent()
        foldLive()
        guard let item = session.item(c.id) else { return }
        update(c.id, EditorCommands.convert(item.content, to: choice, caret: c.currentCaret()))
    }

    func toggleTodo() {
        guard let c = activeCoordinator else { return }
        c.syncContent()
        foldLive()
        guard let item = session.item(c.id) else { return }
        update(c.id, EditorCommands.toggleTodo(item.content, at: c.currentCaret()))
    }

    func setChecked(_ id: BlockID, line: Int, _ checked: Bool) {
        foldLive()
        guard let item = session.item(id) else { return }
        let caret = coordinators[id]?.c?.currentCaret() ?? EditorCaret(line: line, offset: 0)
        update(id, (EditorCommands.setChecked(item.content, item: line, checked), caret))
    }

    func moveFocus(from id: BlockID, by: Int) {
        guard let i = session.index(of: id) else { return }
        let j = i + by
        guard session.items.indices.contains(j) else { return }
        let target = session.items[j]
        let caret = by < 0 ? EditorCommands.endCaret(target.content) : .start
        focusTarget = target.id
        pendingFocus = FocusRequest(id: target.id, caret: caret, revision: target.revision)
        if let c = coordinators[target.id]?.c { pendingFocus = nil; c.focus(caret) }
    }

    /// Tap below the last block: a new paragraph at the end (or the
    /// caret into an empty last one).
    func tapAtEnd() {
        guard let last = session.items.last else { return }
        if case .paragraph = last.content, last.content.isEmpty {
            moveFocus(from: last.id, by: 0)
            return
        }
        prepareStructure()
        applyChange(session.apply(.split(current: last.content, inserted: [.paragraph(AttributedString())], focus: 1, caret: .start), to: last.id))
    }

    /// Markdown pasted into an empty paragraph: it becomes that block and
    /// the ones after it.
    func pasteMarkdown(_ id: BlockID, markdown: String) {
        let parts = MarkdownSegments.split(markdown).map(EditorBlockContent.parse(markdown:))
        guard let first = parts.first else { return }
        prepareStructure()
        applyChange(session.apply(.split(current: first, inserted: Array(parts.dropFirst()), focus: parts.count - 1, caret: .start), to: id).map { change in
            var c = change
            c.caret = EditorCommands.endCaret(session.item(c.focus)?.content ?? first)
            return c
        })
    }

    // MARK: links

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
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        var top = scene?.keyWindow?.rootViewController
        while let p = top?.presentedViewController { top = p }
        top?.present(alert, animated: true)
    }

    // MARK: [[ completion

    func updateCompletion(_ c: BlockTextCoordinator, query: String, anchor: CGRect) {
        let results = WikiCompletion.rank(query, in: candidates)
        let selected = completion?.query == query ? (completion?.selected ?? 0) : 0
        completion = Completion(blockID: c.id, query: query, anchor: anchor, results: results, selected: min(selected, max(0, results.count - 1)))
    }

    func moveCompletion(_ by: Int) {
        guard var c = completion, !c.results.isEmpty else { return }
        c.selected = (c.selected + by + c.results.count) % c.results.count
        completion = c
    }

    func dismissCompletion() { completion = nil }

    func acceptCompletion(_ pick: WikiCandidate? = nil) {
        guard let c = completion else { return }
        guard let chosen = pick ?? (c.results.indices.contains(c.selected) ? c.results[c.selected] : nil) else {
            completion = nil
            return
        }
        coordinators[c.blockID]?.c?.insertWikiLink(WikiCompletion.target(for: chosen, among: candidates))
    }

    // MARK: remote changes and status

    /// The cached body was rewritten (sync): take the new blocks, keeping
    /// the one under the caret and anything unsaved.
    private func remoteChanged() async {
        // a body older than our own landed write would roll it back on screen
        if let landed = try? await cache.lastLandedEpoch(docID), let rec = try? await cache.doc(docID),
           let body = rec.bodyEpoch, body < landed { return }
        // what's committed but maybe not yet in the outbox when the cache is read
        let before = unpersisted
        guard let editor = try? await cache.editor(for: docID) else { return }
        applyRemote(editor, overlaying: before)
    }

    /// Take a refreshed editor: our committed-but-unwritten proposes go on
    /// top of it, and the focused, typed-in and debounced blocks keep
    /// their text.
    func applyRemote(_ fresh: DocEditor, overlaying earlier: [ProposeRequest] = []) {
        var editor = fresh
        var seen: Set<String> = []
        for r in earlier + unpersisted where seen.insert(r.requestID ?? "").inserted {
            editor.overlay(r.ops.map(\.kind))
        }
        var keep = saver.waiting.union(live.keys)
        if let f = focusedID { keep.insert(f) }
        for r in earlier + unpersisted { for op in r.ops { if let t = op.kind.target { keep.insert(t) } } }
        foldLive()
        _ = session.refresh(editor, keep: keep)
    }

    func poll() async {
        if let s = try? await cache.outboxState(for: docID), s != outbox { outbox = s }
        // the open review queue, now and then, when online
        if let api, app?.isOnline == true, Date().timeIntervalSince(lastReviewFetch) > 15 {
            lastReviewFetch = Date()
            openOps = try? await api.openReviewOps(docID)
        }
        if let r = try? await cache.reviews(for: docID, openOps: openOps), r != reviews { reviews = r }
    }

    func retryFailed() async {
        try? await cache.retryFailed(docID: docID)
        await app?.replayOutbox()
        await poll()
    }

    var chip: EditorChip {
        EditorChip.make(
            online: app?.isOnline ?? false, unsaved: unsaved, outbox: outbox,
            reviews: reviews.values.map(\.verdict)
        )
    }
}
