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
    /// what the cache says this doc is (nil until it said)
    @State private var kind: DocKind?
    @State private var opening = false
    /// the body isn't in the offline cache: the page shows read-only
    @State private var surfaceFailed = false
    @State private var movingWorkspace = false // workspaces
    /// on screen (not under a pushed doc)
    @State private var appeared = false
    @AppStorage(DocTextSize.key) private var textSize = DocTextSize.actual
    @Environment(\.horizontalSizeClass) private var sizeClass
    @Environment(\.colorScheme) private var colorScheme
    /// the inspector: a column on iPad and the Mac, a sheet on iPhone
    @State private var showInspector = false
    /// the doc's op ledger (newest first): the byline and the inspector's History
    @State private var history: [DocHistoryEntry] = []
    /// the signed-in person's name (their avatar's initial in the byline)
    @State private var me: String?

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
        // who wrote it (the byline, History): again when the doc moves on
        .task(id: HistoryKey(doc: docID, epoch: model.index.byID[docID]?.currentEpoch ?? 0)) { await loadHistory() }
        // the surface opens once the body is cached and we know what it may be
        .task(id: OpenKey(doc: docID, loaded: page != nil, kindKnown: kind != nil)) { await openSurface() }
        // queued edits show while they wait (offline, a live session)
        .task(id: model.pendingWrites) { await reloadPage() }
        // a sync write drops the cached meta: load it again
        .task(id: MetaKey(doc: docID, missing: model.editMeta[docID] == nil || model.staleMeta.contains(docID))) { await model.loadEditMeta(docID) }
        .onChange(of: scenePhase) { _, phase in
            // finish writing to the outbox even if iOS suspends us mid-flush
            if phase != .active, let surface {
                surface.stop()
                let id = UIApplication.shared.beginBackgroundTask(withName: "editor flush")
                Task {
                    await surface.flush(final: true)
                    UIApplication.shared.endBackgroundTask(id)
                }
            } else if phase == .active, let surface, appeared {
                // back in the foreground: catch up, then watch again
                surface.start()
            }
        }
        // back on screen (a pushed doc popped): catch up, then watch again
        .onAppear {
            appeared = true
            if let surface {
                surface.start()
                router.snapshotPrep[docID] = { [weak surface] in await surface?.flush(final: true) }
            }
        }
        .onDisappear {
            appeared = false
            router.snapshotPrep[docID] = nil
            // back again before the flush ends (a quick Back): keep it running
            if let surface { Task { await surface.flush(final: true); if !appeared { surface.stop() }; await model.replayOutbox() } }
        }
        // ADR 0004: made a viewer while editing: what's typed is saved
        // (the server says no, and the chip shows it), then read-only in place
        .onChange(of: canWrite) { _, can in
            guard let surface else { return }
            Task {
                // made a viewer: setEditable saves what's typed first
                await surface.setEditable(can)
                await model.replayOutbox()
            }
        }
        // Share link…, Export as PDF… and link comments (menus, the menu bar, a comment push)
        .modifier(DocShareActions(docID: docID, canShare: access.canEdit, pageLoaded: page != nil, onFind: { surface?.presentFind() }))
    }

    /// ADR 0004: what you may change here (a viewer reads only; pins are local)
    private var access: EditAccess { model.access(for: docID) }

    /// Editable here: you may edit it, and it isn't a canvas or a mirror.
    private var canWrite: Bool { DocWriteGate.canWrite(isCanvas: kind?.canvas, isMirror: kind?.mirror, canEdit: access.canEdit) }

    private func surfaceView(_ surface: SurfaceModel) -> some View {
        DocSurfaceContent(model: surface)
            .navigationBarTitleDisplayMode(.inline)
            // iPhone while typing: the doc runs to the keyboard (the artboard);
            // the glass keyboard would otherwise show the tab bar through it
            .toolbarVisibility(surface.isEditingText && !router.isPad ? .hidden : .automatic, for: .tabBar)
            .sheet(isPresented: $movingWorkspace) { MoveToWorkspaceSheet(docIDs: [docID]) }
            .onChange(of: chrome, initial: true) { _, c in
                // the header and children list, reachable for VoiceOver from the text
                var actions: [(String, () -> Void)] = []
                if c.linkComments != nil { actions.append(("Link comments", { router.request(.linkComments, on: docID) })) }
                actions.append(("Inspector", { showInspector = true }))
                for kid in c.children.cards { actions.append(("Open \(kid.title)", { router.open(.doc(kid.id)) })) }
                surface.setAccessibilityChrome(title: c.title, actions: actions)
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
            .inspector(isPresented: $showInspector) {
                DocInspector(
                    docID: docID, surface: surface, history: history, page: page,
                    asSheet: sizeClass != .regular, me: me,
                    menu: AnyView(moreMenuItems(surface)),
                    onClose: { showInspector = false }
                )
                .inspectorColumnWidth(min: 290, ideal: 330, max: 400)
                .environment(model)
                .environment(router)
            }
            // after .inspector: the items sit in the doc's own bar
            .toolbar { surfaceToolbar(surface) }
    }

    /// The doc's bar. iPhone: the title and save state in the middle; while
    /// typing Undo, the inspector and Done, otherwise the inspector and ….
    /// iPad and the Mac: the crumbs, then share, … and the inspector toggle.
    @ToolbarContentBuilder
    private func surfaceToolbar(_ surface: SurfaceModel) -> some ToolbarContent {
        // the Mac's bar shows no principal item: the title leads there (Mac-Main)
        #if targetEnvironment(macCatalyst)
        ToolbarItem(placement: .navigation) {
            DocBarTitle(title: title, crumbs: model.index.ancestors(of: docID).map(\.title),
                        workspace: model.workspaceBadge(for: docID)?.name,
                        save: saveState(surface), wide: sizeClass == .regular)
        }
        .sharedBackgroundVisibility(.hidden)
        #else
        ToolbarItem(placement: .principal) {
            DocBarTitle(title: title, crumbs: model.index.ancestors(of: docID).map(\.title),
                        workspace: model.workspaceBadge(for: docID)?.name,
                        save: saveState(surface), wide: sizeClass == .regular)
        }
        #endif
        if sizeClass == .regular {
            ToolbarItemGroup(placement: .primaryAction) {
                if model.shareLinks.isAvailable, access.canEdit, page != nil {
                    Button { shareLink(surface) } label: { Label("Share link", systemImage: "square.and.arrow.up") }
                        .help("Share link…")
                }
                Menu { moreMenuItems(surface) } label: { Label("More", systemImage: "ellipsis") }
                    .accessibilityLabel("More")
                inspectorButton(surface)
            }
        } else if surface.isEditingText {
            ToolbarItemGroup(placement: .primaryAction) {
                Button { surface.undo?.undo() } label: { Label("Undo", systemImage: "arrow.uturn.backward") }
                    .disabled(!surface.canUndo)
                    .accessibilityIdentifier("doc.undo")
                inspectorButton(surface)
            }
            ToolbarSpacer(.fixed, placement: .primaryAction)
            ToolbarItem(placement: .primaryAction) {
                Button {
                    surface.textView.resignFirstResponder()
                } label: {
                    Label("Done editing", systemImage: "checkmark")
                }
                .buttonStyle(.glassProminent)
                .tint(Color.lapis.accent)
                .accessibilityIdentifier("doc.done")
            }
        } else {
            ToolbarItemGroup(placement: .primaryAction) {
                inspectorButton(surface)
                Menu { moreMenuItems(surface) } label: { Label("More", systemImage: "ellipsis") }
                    .accessibilityLabel("More")
            }
        }
    }

    /// The inspector toggle; a yellow dot while changes wait for a look.
    private func inspectorButton(_ surface: SurfaceModel) -> some View {
        let waiting = surface.reviewItems.count
        return Button {
            showInspector.toggle()
        } label: {
            Label {
                Text("Inspector")
            } icon: {
                // a yellow dot while changes wait (drawn into the symbol: the
                // bar keeps only a label's image)
                Image(uiImage: InspectorGlyph.image(dot: waiting > 0, dark: colorScheme == .dark))
            }
        }
        .tint(showInspector && sizeClass == .regular ? Color.lapis.accent : Color.lapis.ink)
        .accessibilityLabel(waiting > 0 ? "Inspector: \(waiting == 1 ? "1 change" : "\(waiting) changes") to review" : "Inspector")
        .accessibilityIdentifier("doc.inspector")
        .help("Inspector")
    }

    /// The … menu: find, pin, move, share, export.
    @ViewBuilder private func moreMenuItems(_ surface: SurfaceModel?) -> some View {
        if let surface {
            Button("Find in Doc", systemImage: "magnifyingglass") { surface.presentFind() }
        }
        Button(model.isPinned(docID) ? "Unpin from Today" : "Pin to Today",
               systemImage: model.isPinned(docID) ? "pin.slash" : "pin") { model.togglePin(docID) }
        if model.hasWorkspaces, access.canMove {
            Button("Move to workspace\u{2026}", systemImage: "square.stack") { movingWorkspace = true }
        }
        if model.shareLinks.isAvailable, page != nil {
            Divider()
            if access.canEdit {
                Button("Share link\u{2026}", systemImage: "link") { router.request(.shareLink, on: docID) }
                    .accessibilityIdentifier("doc.shareLink")
            }
            Button("Export as PDF\u{2026}", systemImage: "doc.richtext") { router.request(.exportPDF, on: docID) }
        }
    }

    /// The editor's chip as a save state: a mark and a word.
    private func saveState(_ surface: SurfaceModel) -> (SaveState, String?) {
        guard surface.editable else { return (.readonly, access.canEdit ? "Read only" : "View only") }
        let chip = surface.chip
        switch chip.tone {
        case .saved: return (.saved, nil)
        case .busy: return (.saving, nil)
        case .offline: return (.unsaved, chip.text)
        case .review: return (.waiting, nil)
        case .conflict: return (.waiting, chip.text)
        case .failed: return (.failed, chip.text)
        }
    }

    /// (the snapshot preparation flushes the surface first)
    private func shareLink(_ surface: SurfaceModel) { router.request(.shareLink, on: docID) }

    private func exportPDF(_ surface: SurfaceModel) { router.request(.exportPDF, on: docID) }

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
            status: page?.status,
            byline: history.isEmpty ? nil : DocByline.make(history: history, me: me, version: model.index.byID[docID]?.currentEpoch),
            me: me,
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
            if kind == nil, model.cache == nil { kind = DocKind(canvas: false, mirror: true) }
            return
        }
        kind = DocKind(canvas: rec.isCanvas, mirror: rec.mirrorPermission != nil)
    }

    /// The surface over the cached body (fetched first if need be). Nil
    /// while it isn't cached: the reading states show meanwhile.
    private func openSurface() async {
        if kind == nil, page != nil { await checkEditable() }
        guard surface == nil, !opening, page != nil, kind != nil else { return }
        opening = true
        defer { opening = false }
        guard let s = await SurfaceModel.open(docID, app: model, editable: canWrite) else {
            surfaceFailed = true
            return
        }
        surfaceFailed = false
        s.onOpenWiki = { target in openWiki(target) }
        // development launch arguments (screenshots, UI runs): `-showInspector YES`, `-editDoc YES`
        if UserDefaults.standard.bool(forKey: "showInspector") {
            Task { try? await Task.sleep(for: .milliseconds(600)); showInspector = true }
        }
        if UserDefaults.standard.bool(forKey: "editDoc"), canWrite { model.pendingEditDoc = docID }
        s.chartTables = page?.chartTables ?? ChartTables()
        if docID == model.todoDocID {
            // the To-do doc's boxes are to-do items: they go through the to-do API
            s.onTodoCheckbox = { block, index, checked in toggleTodo(block, index, checked) }
        }
        surface = s
        // Share link / Export as PDF (menus, menu bar) flush it first
        router.snapshotPrep[docID] = { [weak s] in await s?.flush(final: true) }
        // a doc just created here opens with the caret at its start
        if canWrite, model.pendingEditDoc == docID {
            model.pendingEditDoc = nil
            s.focusAtStart()
        }
    }

    private func loadHistory() async {
        guard let api = model.api else { return }
        if let rows = try? await api.docHistory(docID), rows != history { history = rows }
        if me == nil, let name = try? await api.profile().name, !name.isEmpty { me = name }
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

/// The inspector's bar symbol, with a yellow dot when changes wait.
enum InspectorGlyph {
    @MainActor static func image(dot: Bool, dark: Bool) -> UIImage {
        let config = UIImage.SymbolConfiguration(textStyle: .body, scale: .large)
        guard let base = UIImage(systemName: "sidebar.right", withConfiguration: config) else { return UIImage() }
        guard dot else { return base.withRenderingMode(.alwaysTemplate) }
        let size = base.size
        let r = UIGraphicsImageRenderer(size: CGSize(width: size.width + 3, height: size.height + 3))
        let img = r.image { ctx in
            let traits = UITraitCollection(userInterfaceStyle: dark ? .dark : .light)
            base.withTintColor(UIColor.lapis.ink.resolvedColor(with: traits)).draw(at: CGPoint(x: 0, y: 3))
            let d: CGFloat = 9
            let rect = CGRect(x: size.width + 3 - d, y: 0, width: d, height: d)
            UIColor.lapis.yellowMark.resolvedColor(with: traits).setFill()
            ctx.cgContext.fillEllipse(in: rect)
        }
        return img.withRenderingMode(.alwaysOriginal)
    }
}

/// What a doc is, for the write gate.
struct DocKind: Hashable {
    var canvas: Bool
    var mirror: Bool
}

/// Who gets an editable surface: you may edit the doc (ADR 0004: not a
/// viewer), it isn't a canvas or a federation mirror, and the cache has
/// said which (unknown reads only).
enum DocWriteGate {
    static func canWrite(isCanvas: Bool?, isMirror: Bool?, canEdit: Bool) -> Bool {
        guard let isCanvas, let isMirror else { return false }
        return canEdit && !isCanvas && !isMirror
    }
}

private struct OpenKey: Hashable {
    var doc: DocID
    var loaded: Bool
    var kindKnown: Bool
}

private struct HistoryKey: Hashable {
    var doc: DocID
    var epoch: Int
}

private struct MetaKey: Hashable {
    var doc: DocID
    var missing: Bool
}

/// What the surface's header and footer show.
struct DocChrome: Equatable {
    var title: String
    var tags: [String]
    /// the frontmatter's status ("decided")
    var status: String? = nil
    /// who wrote it, the version, when (from the op ledger)
    var byline: DocByline? = nil
    /// your name, for your avatar's initial
    var me: String? = nil
    var meta: EditMeta?
    var accessNote: String?
    var workspace: WorkspaceBadge?
    var linkComments: String?
    var children: DocChildrenLayout
}

/// The doc's title block (the Lapis doc artboards): status and tag pills,
/// the serif title, then who wrote it ("You and keeper:API · v14 · edited
/// 12:41"); "View only" where you may only read; the link-comments entry.
struct DocHeader: View {
    let chrome: DocChrome
    var now: Date = .now
    var onOpenLinkComments: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 10) {
                if chrome.status != nil || !chrome.tags.isEmpty || chrome.accessNote != nil {
                    FlowRow(spacing: 6) {
                        if let status = chrome.status { DocPill(text: status, dot: DocPill.statusColor(status)) }
                        ForEach(chrome.tags, id: \.self) { DocPill(text: "#\($0)").accessibilityLabel("Tag \($0)") }
                        if let n = chrome.accessNote { ViewOnlyNote(text: n) }
                    }
                }
                Text(chrome.title)
                    .lapisFont(.docTitle)
                    .foregroundStyle(Color.lapis.ink)
                    .accessibilityAddTraits(.isHeader)
                    .fixedSize(horizontal: false, vertical: true)
                byline
            }
            if let linkComments = chrome.linkComments {
                Button(action: onOpenLinkComments) {
                    Label(linkComments, systemImage: "bubble.left.and.text.bubble.right")
                        .font(.footnote.weight(.medium))
                        .foregroundStyle(Color.lapis.accent)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .card(Color.lapis.card)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("doc.linkComments")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.bottom, 4)
    }

    @ViewBuilder private var byline: some View {
        if let b = chrome.byline, !b.authors.isEmpty {
            HStack(spacing: 8) {
                HStack(spacing: -6) {
                    ForEach(Array(b.authors.prefix(3).enumerated()), id: \.offset) { _, name in
                        LapisAvatar(name: name == "You" ? (chrome.me ?? name) : name, agent: Principal.isAgent(name), size: 24)
                            .background(Circle().fill(Color.lapis.bg).padding(-2))
                    }
                }
                .accessibilityHidden(true)
                Text(bylineText(b))
                    .font(.footnote)
                    .foregroundStyle(Color.lapis.ink3)
                    .lineLimit(2)
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("doc.byline")
        } else if let meta = chrome.meta {
            Text("edited by \(meta.author) \(RelativeTime.string(meta.date, now: now))")
                .font(.footnote)
                .foregroundStyle(Color.lapis.ink3)
        }
    }

    /// "You and **keeper:API** · v14 · edited 12:41", agents in mono violet.
    private func bylineText(_ b: DocByline) -> AttributedString {
        var out = AttributedString()
        for (i, name) in b.authors.enumerated() {
            if i > 0 { out += AttributedString(i == b.authors.count - 1 ? " and " : ", ") }
            var part = AttributedString(name)
            if Principal.isAgent(name) {
                part.font = .lapisMono(.footnote, weight: .medium)
                part.foregroundColor = Color.lapis.agent
            } else {
                part.foregroundColor = Color.lapis.ink2
            }
            out += part
        }
        if let v = b.version, v > 0 { out += AttributedString(" · v\(v)") }
        if let d = b.edited { out += AttributedString(" · edited \(ShortWhen.string(d, now: now))") }
        return out
    }
}

/// A quiet pill over the title: a status with its dot, or a tag.
struct DocPill: View {
    let text: String
    var dot: Color?

    var body: some View {
        HStack(spacing: 5) {
            if let dot { Circle().fill(dot).frame(width: 7, height: 7).accessibilityHidden(true) }
            Text(text).lineLimit(1)
        }
        .font(.footnote)
        .foregroundStyle(Color.lapis.ink2)
        .padding(.horizontal, 10)
        .frame(minHeight: 26)
        .background(Color.lapis.hover, in: .capsule)
    }

    /// decided / done: green; draft / proposed: yellow; anything else quiet.
    static func statusColor(_ status: String) -> Color {
        switch status.lowercased() {
        case "decided", "done", "accepted", "final", "published": Color.lapis.green
        case "draft", "proposed", "open", "review", "wip": Color.lapis.yellowMark
        case "superseded", "rejected", "cut", "deprecated": Color.lapis.red
        default: Color.lapis.ink3
        }
    }
}

/// The doc bar's middle. iPhone: the title over the save state. iPad and
/// the Mac: the crumbs ("Work › Taisce › Architecture"), then the save state.
struct DocBarTitle: View {
    let title: String
    let crumbs: [String]
    var workspace: String?
    let save: (SaveState, String?)
    var wide = false

    var body: some View {
        if wide {
            HStack(spacing: 10) {
                Text(crumbText)
                    .font(.subheadline)
                    .lineLimit(1)
                    .truncationMode(.head)
                SaveStateDot(state: save.0, text: save.1)
            }
            .accessibilityElement(children: .combine)
        } else {
            VStack(spacing: 1) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Color.lapis.ink)
                    .lineLimit(1)
                SaveStateDot(state: save.0, text: save.1)
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("editor.chip")
        }
    }

    private var crumbText: AttributedString {
        var out = AttributedString()
        for c in ([workspace].compactMap { $0 } + crumbs) {
            var part = AttributedString(c + DocIndex.separator)
            part.foregroundColor = Color.lapis.ink3
            out += part
        }
        var t = AttributedString(title)
        t.foregroundColor = Color.lapis.ink
        t.font = .subheadline.weight(.semibold)
        out += t
        return out
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
            .foregroundStyle(Color.lapis.ink3)
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
            .padding(.horizontal, Lapis.Layout.gutter)
            .padding(.top, 4)
            .padding(.bottom, 40)
            .frame(maxWidth: Lapis.Layout.readingWidth, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .groundBackground()
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Text(breadcrumb ?? title)
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(Color.lapis.ink3)
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
                Button("Try again", action: onRetry).buttonStyle(.bordered).tint(Color.lapis.accent)
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
