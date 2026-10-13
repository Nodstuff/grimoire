import SwiftUI
import UIKit
import TaisceKit

/// One doc: an always-live text surface (`SurfaceModel`) once the body is
/// in the offline cache. You may edit it → editable, the caret where you
/// click; a viewer, a canvas or a mirror → the same surface read-only,
/// links tap to follow. Before the body is cached: loading, an error, or
/// a folder view (`DocContent`).
struct DocScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.scenePhase) private var scenePhase
    let docID: DocID
    @State private var page: DocPage?
    @State private var loadError: String?
    @State private var surface: SurfaceModel?
    /// not a canvas or a mirror (nil until the cache said)
    @State private var editableKind: Bool?
    @State private var opening = false
    /// the body isn't in the offline cache: the page shows read-only
    @State private var surfaceFailed = false
    @State private var movingWorkspace = false // workspaces
    @AppStorage(DocTextSize.key) private var textSize = DocTextSize.actual

    var body: some View {
        Group {
            if let surface {
                surfaceView(surface)
            } else {
                reading
            }
        }
        #if targetEnvironment(macCatalyst)
        // View › Bigger / Smaller Text; ⌘= works as well as ⌘+ (no shift)
        .dynamicTypeSize(DocTextSize.size(textSize))
        .environment(\.docScale, DocTextSize.scale(textSize))
        .background {
            Button("") { textSize = DocTextSize.bigger(textSize) }
                .keyboardShortcut("=")
                .hidden()
        }
        #endif
        .task(id: docID) { await observe() }
        .task(id: docID) { await refresh(force: false) }
        .task(id: docID) { await checkEditable() }
        // the surface opens once the body is cached and we know what it may be
        .task(id: OpenKey(doc: docID, loaded: page != nil, kindKnown: editableKind != nil)) { await openSurface() }
        // queued edits show while they wait (offline, a live session)
        .task(id: model.pendingWrites) { await reloadPage() }
        // a sync write drops the cached meta: load it again
        .task(id: MetaKey(doc: docID, missing: model.editMeta[docID] == nil)) { await model.loadEditMeta(docID) }
        .onChange(of: scenePhase) { _, phase in
            // finish writing to the outbox even if iOS suspends us mid-flush
            if phase != .active, let surface {
                let id = UIApplication.shared.beginBackgroundTask(withName: "editor flush")
                Task {
                    await surface.flush()
                    UIApplication.shared.endBackgroundTask(id)
                }
            }
        }
        .onDisappear {
            if let surface { Task { await surface.flush(); surface.stop(); await model.replayOutbox() } }
        }
        // ADR 0004: made a viewer while editing: what's typed is saved
        // (the server says no, and the chip shows it), then read-only in place
        .onChange(of: canWrite) { _, can in
            guard let surface else { return }
            Task {
                if !can { await surface.flush() }
                surface.setEditable(can)
                await model.replayOutbox()
            }
        }
        // Share link…, Export as PDF… and link comments (menus, the menu bar, a comment push)
        .modifier(DocShareActions(docID: docID, canShare: access.canEdit, pageLoaded: page != nil))
    }

    /// ADR 0004: what you may change here (a viewer reads only; pins are local)
    private var access: EditAccess { model.access(for: docID) }

    /// Editable here: you may edit it, and it isn't a canvas or a mirror.
    private var canWrite: Bool { (editableKind ?? false) && access.canEdit }

    private func surfaceView(_ surface: SurfaceModel) -> some View {
        DocSurfaceContent(model: surface)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    HStack(spacing: 8) {
                        Text(model.index.breadcrumb(of: docID) ?? title)
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(Theme.secondary)
                            .lineLimit(1)
                            .truncationMode(.head)
                        if surface.editable {
                            EditorChipView(chip: surface.chip) { Task { await surface.retryFailed() } }
                        }
                    }
                }
            }
            .modifier(DocMenu(
                pinned: model.isPinned(docID),
                onTogglePin: { model.togglePin(docID) },
                onMoveWorkspace: model.hasWorkspaces && access.canMove ? { movingWorkspace = true } : nil,
                onShareLink: model.shareLinks.isAvailable && access.canEdit && page != nil ? { shareLink(surface) } : nil,
                onExportPDF: model.shareLinks.isAvailable && page != nil ? { exportPDF(surface) } : nil
            ))
            .sheet(isPresented: $movingWorkspace) { MoveToWorkspaceSheet(docIDs: [docID]) }
            .onChange(of: chrome, initial: true) { _, c in
                surface.setChrome(
                    header: hosted(DocHeader(chrome: c, onOpenLinkComments: { router.request(.linkComments, on: docID) })),
                    footer: c.children.cards.isEmpty ? nil : hosted(DocChildrenList(
                        layout: c.children, childCounts: model.index.childCount, onOpen: { router.open(.doc($0)) },
                        onNewDocHere: access.canCreate ? { router.newDoc(in: docID) } : nil
                    ))
                )
            }
            .onChange(of: page) { _, p in surface.chartTables = p?.chartTables ?? ChartTables() }
            .task(id: docID) {
                let kids = DocChildrenLayout.children(of: docID, index: model.index, meta: [:]).prefix(20)
                for kid in kids { await model.loadEditMeta(kid.id) }
            }
    }

    /// What's typed is in the outbox before a link snapshots the doc.
    private func shareLink(_ surface: SurfaceModel) {
        Task {
            await surface.flush()
            router.request(.shareLink, on: docID)
        }
    }

    private func exportPDF(_ surface: SurfaceModel) {
        Task {
            await surface.flush()
            router.request(.exportPDF, on: docID)
        }
    }

    /// A SwiftUI view for the text view's header or footer, with what it reads.
    private func hosted(_ v: some View) -> AnyView {
        #if targetEnvironment(macCatalyst)
        AnyView(v.environment(model).environment(router)
            .dynamicTypeSize(DocTextSize.size(textSize))
            .environment(\.docScale, DocTextSize.scale(textSize)))
        #else
        AnyView(v.environment(model).environment(router))
        #endif
    }

    private var title: String { model.index.byID[docID]?.title ?? "" }

    /// What the header and footer show: an empty body with children is a
    /// folder (New doc here), else "Inside this doc".
    private var chrome: DocChrome {
        let empty = surface.map { s in !s.hasContent } ?? false
        return DocChrome(
            title: title,
            tags: page?.tags ?? [],
            meta: model.editMeta[docID],
            accessNote: access.note,
            workspace: model.workspaceBadge(for: docID),
            linkComments: model.shareLinks.isAvailable ? model.shareLinks.commentSummary(for: docID).title : nil,
            children: DocChildrenLayout.make(
                pageEmpty: empty,
                children: DocChildrenLayout.children(of: docID, index: model.index, meta: model.editMeta)
            )
        )
    }

    private var reading: some View {
        let doc = model.index.byID[docID]
        let access = access
        let onNewDocHere: (() -> Void)? = access.canCreate ? { router.newDoc(in: docID) } : nil
        return DocContent(
            title: doc?.title ?? "",
            breadcrumb: model.index.breadcrumb(of: docID),
            // the body shows in the surface; this view waits for it (or
            // shows the page when the body can't be cached)
            page: surfaceFailed ? page : nil,
            meta: model.editMeta[docID],
            loadError: loadError,
            pinned: model.isPinned(docID),
            onTogglePin: { model.togglePin(docID) },
            onRetry: { Task { await refresh(force: true) } },
            accessNote: access.note,
            workspace: model.workspaceBadge(for: docID),
            onMoveWorkspace: model.hasWorkspaces && access.canMove ? { movingWorkspace = true } : nil,
            children: DocChildrenLayout.make(
                pageEmpty: page.map(\.isEmpty),
                children: DocChildrenLayout.children(of: docID, index: model.index, meta: model.editMeta)
            ),
            childCounts: model.index.childCount,
            onOpenChild: { router.open(.doc($0)) },
            onNewDocHere: onNewDocHere,
            linkComments: model.shareLinks.isAvailable ? model.shareLinks.commentSummary(for: docID).title : nil,
            onOpenLinkComments: { router.request(.linkComments, on: docID) },
            // ADR 0004: creating or updating a link needs owner or editor
            onShareLink: model.shareLinks.isAvailable && access.canEdit && page != nil ? { router.request(.shareLink, on: docID) } : nil,
            onExportPDF: model.shareLinks.isAvailable && page != nil ? { router.request(.exportPDF, on: docID) } : nil
        )
        .sheet(isPresented: $movingWorkspace) { MoveToWorkspaceSheet(docIDs: [docID]) }
    }

    private func openWiki(_ target: String) {
        if let doc = model.index.doc(titled: target) { router.open(.doc(doc.id)) }
    }

    /// Canvases and federation mirrors are read-only here.
    private func checkEditable() async {
        guard let cache = model.cache, let rec = try? await cache.doc(docID) else {
            if editableKind == nil { editableKind = model.cache == nil ? false : nil }
            return
        }
        editableKind = !rec.isCanvas && rec.mirrorPermission == nil
    }

    /// The surface over the cached body (fetched first if need be). Nil
    /// while it isn't cached: the reading states show meanwhile.
    private func openSurface() async {
        if editableKind == nil, page != nil { await checkEditable() }
        guard surface == nil, !opening, page != nil, editableKind != nil else { return }
        opening = true
        defer { opening = false }
        guard let s = await SurfaceModel.open(docID, app: model, editable: canWrite) else {
            surfaceFailed = true
            return
        }
        surfaceFailed = false
        s.onOpenWiki = { target in openWiki(target) }
        s.chartTables = page?.chartTables ?? ChartTables()
        if docID == model.todoDocID {
            // the To-do doc's boxes are to-do items: they go through the to-do API
            s.onTodoCheckbox = { block, index, checked in toggleTodo(block, index, checked) }
        }
        surface = s
        // a doc just created here opens with the caret at its start
        if canWrite, model.pendingEditDoc == docID {
            model.pendingEditDoc = nil
            s.focusAtStart()
        }
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

    /// A To-do doc box: the to-do API, not a block edit.
    private func toggleTodo(_ block: BlockID, _ index: Int, _ checked: Bool) {
        Task {
            do {
                guard let at = TodoDocAddress.locate(page?.blocks ?? [], block: block, index: index) else { throw TodoWriteError.notFound }
                try await model.setTodoDone(date: at.date, position: at.position, done: checked)
            } catch {
                model.lastError = error.localizedDescription
            }
        }
    }
}

private struct OpenKey: Hashable {
    var doc: DocID
    var loaded: Bool
    var kindKnown: Bool
}

private struct MetaKey: Hashable {
    var doc: DocID
    var missing: Bool
}

/// What the surface's header and footer show.
struct DocChrome: Equatable {
    var title: String
    var tags: [String]
    var meta: EditMeta?
    var accessNote: String?
    var workspace: WorkspaceBadge?
    var linkComments: String?
    var children: DocChildrenLayout
}

/// The doc's title block: workspace chip or "View only", title, tags and
/// "edited by", and the link-comments entry.
struct DocHeader: View {
    let chrome: DocChrome
    var now: Date = .now
    var onOpenLinkComments: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 10) {
                if chrome.workspace != nil || chrome.accessNote != nil {
                    HStack(spacing: 8) {
                        if let w = chrome.workspace { WorkspaceChip(badge: w) }
                        if let n = chrome.accessNote { ViewOnlyNote(text: n) }
                    }
                }
                Text(chrome.title)
                    .docFont(.largeTitle, design: .serif, weight: .semibold)
                    .foregroundStyle(Theme.text)
                    .accessibilityAddTraits(.isHeader)
                    .fixedSize(horizontal: false, vertical: true)
                if !chrome.tags.isEmpty || chrome.meta != nil {
                    // "#design #mobile · edited by claude 2 h ago"
                    FlowRow(spacing: 6) {
                        ForEach(chrome.tags, id: \.self) { TagChip(tag: $0) }
                        if let meta = chrome.meta {
                            Text("\(chrome.tags.isEmpty ? "" : "· ")edited by \(meta.author) \(RelativeTime.string(meta.date, now: now))")
                                .font(.caption2)
                                .foregroundStyle(Theme.secondary)
                        }
                    }
                }
            }
            if let linkComments = chrome.linkComments {
                Button(action: onOpenLinkComments) {
                    Label(linkComments, systemImage: "bubble.left.and.text.bubble.right")
                        .font(.footnote.weight(.medium))
                        .foregroundStyle(Theme.accent)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .card(Theme.surface)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("doc.linkComments")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.bottom, 4)
    }
}

/// The doc's … menu: pin, move to workspace, share link, export.
struct DocMenu: ViewModifier {
    var pinned: Bool
    var onTogglePin: () -> Void
    var onMoveWorkspace: (() -> Void)?
    var onShareLink: (() -> Void)?
    var onExportPDF: (() -> Void)?
    var exportingPDF = false

    func body(content: Content) -> some View {
        content.toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button(pinned ? "Unpin from Today" : "Pin to Today", systemImage: pinned ? "pin.slash" : "pin", action: onTogglePin)
                    if let onMoveWorkspace {
                        Button("Move to workspace\u{2026}", systemImage: "square.stack", action: onMoveWorkspace)
                    }
                    if onShareLink != nil || onExportPDF != nil || exportingPDF {
                        Divider()
                    }
                    if let onShareLink {
                        Button("Share link\u{2026}", systemImage: "link", action: onShareLink)
                            .accessibilityIdentifier("doc.shareLink")
                    }
                    if let onExportPDF {
                        Button("Export as PDF\u{2026}", systemImage: "doc.richtext", action: onExportPDF)
                    } else if exportingPDF {
                        Text("Exporting PDF\u{2026}")
                    }
                } label: {
                    Image(systemName: "ellipsis")
                }
                .accessibilityLabel("More")
            }
        }
    }
}

/// The doc header's "View only": you can read this workspace, not change it.
struct ViewOnlyNote: View {
    let text: String

    var body: some View {
        Label(text, systemImage: "eye")
            .font(.caption2.weight(.medium))
            .foregroundStyle(Theme.secondary)
            .accessibilityElement(children: .combine)
            .accessibilityLabel("\(text): you can read this workspace but not change it")
            .accessibilityIdentifier("doc.viewOnly")
    }
}

/// A doc rendered read-only by the SwiftUI renderer: the loading and error
/// states before the surface opens, a folder's children, and previews.
struct DocContent: View {
    let title: String
    let breadcrumb: String?
    /// nil while the first load is in flight
    let page: DocPage?
    var meta: EditMeta?
    var loadError: String?
    var pinned = false
    var now: Date = .now
    var onTogglePin: () -> Void = {}
    var onRetry: () -> Void = {}
    /// "View only" in a workspace where you are a viewer (ADR 0004)
    var accessNote: String?
    // workspaces: the header chip and the … menu's "Move to workspace…"
    var workspace: WorkspaceBadge?
    var onMoveWorkspace: (() -> Void)?
    // children: a folder view for an empty body, else "Inside this doc"
    var children: DocChildrenLayout = .none
    var childCounts: [DocID: Int] = [:]
    var onOpenChild: (DocID) -> Void = { _ in }
    var onNewDocHere: (() -> Void)?
    // share links (SERVER mode): the comments entry and the … menu's items
    var linkComments: String?
    var onOpenLinkComments: () -> Void = {}
    var onShareLink: (() -> Void)?
    var onExportPDF: (() -> Void)?
    var exportingPDF = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                DocHeader(
                    chrome: DocChrome(title: title, tags: page?.tags ?? [], meta: meta, accessNote: accessNote,
                                      workspace: workspace, linkComments: linkComments, children: children),
                    now: now, onOpenLinkComments: onOpenLinkComments
                )
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
        }
        .modifier(DocMenu(pinned: pinned, onTogglePin: onTogglePin, onMoveWorkspace: onMoveWorkspace,
                          onShareLink: onShareLink, onExportPDF: onExportPDF, exportingPDF: exportingPDF))
    }

    @ViewBuilder private var content: some View {
        if let page, !page.isEmpty {
            ForEach(page.blocks) { block in
                NodeStack(nodes: block.nodes)
            }
            .environment(\.docTables, page.chartTables)
            DocChildrenList(layout: children, childCounts: childCounts, now: now, onOpen: onOpenChild)
        } else if case .folder = children {
            DocChildrenList(layout: children, childCounts: childCounts, now: now, onOpen: onOpenChild, onNewDocHere: onNewDocHere)
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
            meta: EditMeta(author: "claude", date: PreviewData.ago(7200)), now: PreviewData.now
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
