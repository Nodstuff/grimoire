import SwiftUI
import TaisceKit

/// Reading view: renders the cached blocks, refreshes them when missing or
/// stale, and re-renders whenever sync rewrites them. Checkboxes queue a
/// `replace` through the outbox.
struct DocScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    let docID: DocID
    @State private var page: DocPage?
    @State private var loadError: String?
    @State private var overrides: [BlockID: [Int: Bool]] = [:]
    @State private var movingWorkspace = false // workspaces

    var body: some View {
        let doc = model.index.byID[docID]
        DocContent(
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
            workspace: model.workspaceBadge(for: docID),
            onMoveWorkspace: model.hasWorkspaces ? { movingWorkspace = true } : nil
        )
        .sheet(isPresented: $movingWorkspace) { MoveToWorkspaceSheet(docIDs: [docID]) }
        .environment(\.openURL, OpenURLAction { url in
            if let target = InlineMarkdown.wikiTarget(url) {
                if let doc = model.index.doc(titled: target) { router.open(.doc(doc.id)) }
                return .handled
            }
            return .systemAction
        })
        .task(id: docID) { await observe() }
        .task(id: docID) { await refresh(force: false) }
        // a sync write drops the cached meta: load it again
        .task(id: MetaKey(doc: docID, missing: model.editMeta[docID] == nil)) { await model.loadEditMeta(docID) }
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
    // workspaces: the header chip and the … menu's "Move to workspace…"
    var workspace: WorkspaceBadge?
    var onMoveWorkspace: (() -> Void)?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 10) {
                    if let workspace { WorkspaceChip(badge: workspace) }
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
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button(pinned ? "Unpin from Today" : "Pin to Today", systemImage: pinned ? "pin.slash" : "pin", action: onTogglePin)
                    if let onMoveWorkspace {
                        Button("Move to workspace\u{2026}", systemImage: "square.stack", action: onMoveWorkspace)
                    }
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
