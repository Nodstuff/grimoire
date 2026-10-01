import SwiftUI
import UIKit
import TaisceKit

/// Reading view: renders the cached blocks, refreshes them when missing or
/// stale, and re-renders whenever sync rewrites them. Checkboxes queue a
/// `replace` through the outbox.
struct DocScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.scenePhase) private var scenePhase
    let docID: DocID
    @State private var page: DocPage?
    @State private var loadError: String?
    @State private var overrides: [BlockID: [Int: Bool]] = [:]
    @State private var editor: EditorModel?
    @State private var editable = false
    @State private var opening = false

    var body: some View {
        Group {
            if let editor {
                editing(editor)
            } else {
                reading
            }
        }
        .task(id: docID) { await observe() }
        .task(id: docID) { await refresh(force: false) }
        .task(id: docID) { await checkEditable() }
        // queued edits show while they wait (offline, a live session)
        .task(id: model.pendingWrites) { await reloadPage() }
        // a sync write drops the cached meta: load it again
        .task(id: MetaKey(doc: docID, missing: model.editMeta[docID] == nil)) { await model.loadEditMeta(docID) }
        .onChange(of: scenePhase) { _, phase in
            // finish writing to the outbox even if iOS suspends us mid-flush
            if phase != .active, let editor {
                let id = UIApplication.shared.beginBackgroundTask(withName: "editor flush")
                Task {
                    await editor.flush()
                    UIApplication.shared.endBackgroundTask(id)
                }
            }
        }
        .onChange(of: editable, initial: true) { _, ok in
            // a doc just created here opens straight into edit mode
            if ok, model.pendingEditDoc == docID {
                model.pendingEditDoc = nil
                Task { await startEditing() }
            }
        }
        .onDisappear {
            if let editor { Task { await editor.flush(); editor.stop() } }
        }
    }

    private func editing(_ editor: EditorModel) -> some View {
        DocEditorView(title: model.index.byID[docID]?.title ?? "", model: editor)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    EditorChipView(chip: editor.chip) { Task { await editor.retryFailed() } }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { Task { await finishEditing() } }
                        .accessibilityIdentifier("editor.done")
                }
            }
            .environment(\.openURL, OpenURLAction { _ in .handled })
    }

    private var reading: some View {
        let doc = model.index.byID[docID]
        return DocContent(
            title: doc?.title ?? "",
            breadcrumb: model.index.breadcrumb(of: docID),
            page: page,
            meta: model.editMeta[docID],
            loadError: loadError,
            pinned: model.isPinned(docID),
            overrides: overrides,
            onToggle: toggle,
            onTogglePin: { model.togglePin(docID) },
            onRetry: { Task { await refresh(force: true) } },
            onEdit: editable && page != nil && !opening ? { Task { await startEditing() } } : nil
        )
        .environment(\.openURL, OpenURLAction { url in
            if let target = InlineMarkdown.wikiTarget(url) {
                if let doc = model.index.doc(titled: target) { router.open(.doc(doc.id)) }
                return .handled
            }
            return .systemAction
        })
    }

    /// Canvases and federation mirrors are read-only here.
    private func checkEditable() async {
        guard let cache = model.cache, let rec = try? await cache.doc(docID) else { return }
        editable = !rec.isCanvas && rec.mirrorPermission == nil
    }

    private func startEditing() async {
        guard editor == nil, !opening else { return }
        opening = true
        defer { opening = false }
        if let e = await EditorModel.open(docID, app: model) {
            editor = e
        } else {
            model.lastError = "This doc isn't available offline yet."
        }
    }

    private func finishEditing() async {
        guard let e = editor else { return }
        e.activeCoordinator?.textView?.resignFirstResponder()
        // only the local write is awaited; sending happens in the background
        await e.flush()
        e.stop()
        editor = nil
        await reloadPage()
        Task { await model.replayOutbox() }
    }

    private func reloadPage() async {
        guard let cache = model.cache, page != nil, let records = try? await cache.blocks(of: docID) else { return }
        page = await model.page(for: docID, records: records)
    }

    private func observe() async {
        guard let cache = model.cache else { return }
        do {
            for try await records in cache.observeBlocks(of: docID) {
                guard !records.isEmpty || page != nil else { continue }
                page = await model.page(for: docID, records: records)
                overrides = [:]
            }
        } catch {
            loadError = error.localizedDescription
        }
    }

    private func refresh(force: Bool) async {
        guard let cache = model.cache, let sync = model.sync else { return }
        let record = try? await cache.doc(docID)
        do {
            if force || (record?.isBodyStale ?? true) {
                try await sync.refresh(docID)
                loadError = nil
            }
            // an empty doc delivers no rows to observe(): settle the page here
            if page == nil { page = await model.page(for: docID, records: try await cache.blocks(of: docID)) }
        } catch {
            loadError = error.localizedDescription
        }
    }

    private func toggle(_ block: BlockID, _ index: Int, _ checked: Bool) {
        overrides[block, default: [:]][index] = checked
        Task {
            do {
                if docID == model.todoDocID {
                    guard let at = TodoDocAddress.locate(page?.blocks ?? [], block: block, index: index) else { throw TodoWriteError.notFound }
                    try await model.setTodoDone(date: at.date, position: at.position, done: checked)
                } else {
                    try await model.setCheckbox(doc: docID, block: block, index: index, checked: checked)
                }
            } catch {
                overrides[block]?[index] = nil
                model.lastError = error.localizedDescription
            }
        }
    }
}

private struct MetaKey: Hashable {
    var doc: DocID
    var missing: Bool
}

struct DocContent: View {
    let title: String
    let breadcrumb: String?
    /// nil while the first load is in flight
    let page: DocPage?
    var meta: EditMeta?
    var loadError: String?
    var pinned = false
    var overrides: [BlockID: [Int: Bool]] = [:]
    var now: Date = .now
    var onToggle: ((BlockID, Int, Bool) -> Void)?
    var onTogglePin: () -> Void = {}
    var onRetry: () -> Void = {}
    /// nil when the doc can't be edited here (yet)
    var onEdit: (() -> Void)?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 10) {
                    Text(title)
                        .font(Theme.serif(.largeTitle))
                        .foregroundStyle(Theme.text)
                        .accessibilityAddTraits(.isHeader)
                    if !metaLine.isEmpty {
                        // "#design #mobile · edited by claude 2 h ago"
                        FlowRow(spacing: 6) {
                            ForEach(page?.tags ?? [], id: \.self) { TagChip(tag: $0) }
                            if let meta {
                                Text("\(page?.tags.isEmpty == false ? "· " : "")edited by \(meta.author) \(RelativeTime.string(meta.date, now: now))")
                                    .font(.caption2)
                                    .foregroundStyle(Theme.secondary)
                            }
                        }
                    }
                }
                .padding(.bottom, 6)
                content
            }
            .padding(.horizontal, Theme.gutter)
            .padding(.top, 4)
            .padding(.bottom, 40)
            .frame(maxWidth: Theme.readingWidth, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .groundBackground()
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Text(breadcrumb ?? title)
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(Theme.secondary)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
            if let onEdit {
                ToolbarItem(placement: .primaryAction) {
                    Button("Edit", action: onEdit)
                        .accessibilityIdentifier("doc.edit")
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button(pinned ? "Unpin from Today" : "Pin to Today", systemImage: pinned ? "pin.slash" : "pin", action: onTogglePin)
                } label: {
                    Image(systemName: "ellipsis")
                }
                .accessibilityLabel("More")
            }
        }
    }

    /// whether the tags / "edited by" line has anything to show
    var metaLine: String {
        ((page?.tags ?? []) + (meta.map { [$0.author] } ?? [])).joined()
    }

    @ViewBuilder private var content: some View {
        if let page, !page.isEmpty {
            ForEach(page.blocks) { block in
                NodeStack(nodes: block.nodes)
                    .environment(\.checkboxToggle, CheckboxToggle(
                        overrides: overrides[block.id] ?? [:],
                        action: onToggle.map { f in { i, v in f(block.id, i, v) } }
                    ))
            }
        } else if let loadError {
            VStack(alignment: .leading, spacing: 12) {
                EmptyCard(icon: "wifi.slash", title: "Couldn't load this doc", hint: loadError)
                Button("Try again", action: onRetry).buttonStyle(.bordered).tint(Theme.accent)
            }
        } else if page == nil {
            ProgressView().frame(maxWidth: .infinity, minHeight: 160)
        } else {
            EmptyCard(icon: "doc.text", title: "Nothing here yet", hint: "This doc has no content.")
        }
    }
}

#if DEBUG
#Preview("Doc") {
    NavigationStack {
        DocContent(
            title: "Design spec", breadcrumb: "Grimoire › iOS app", page: PreviewData.page,
            meta: EditMeta(author: "claude", date: PreviewData.ago(7200)), overrides: [:], now: PreviewData.now,
            onToggle: { _, _, _ in }
        )
    }
    .preferredColorScheme(.dark)
}

#Preview("Doc, light") {
    NavigationStack {
        DocContent(title: "Design spec", breadcrumb: "Grimoire › iOS app", page: PreviewData.page, now: PreviewData.now)
    }
    .preferredColorScheme(.light)
}

#Preview("Doc, offline error") {
    NavigationStack {
        DocContent(title: "Roadmap", breadcrumb: "Grimoire", page: nil, loadError: "The Internet connection appears to be offline.")
    }
    .preferredColorScheme(.dark)
}
#endif
