import SwiftUI
import TaisceKit

/// The doc tree (the Lapis iPhone Library artboard): "LIBRARY" over the
/// workspace's name, which switches workspaces; who it's shared with and
/// how many docs; a filter; the tree in one card. New folder and New doc
/// ride the bar. Pull to refresh catches up with the server.
struct LibraryScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router

    var body: some View {
        LibraryContent(
            nodes: model.library,
            loaded: model.treeLoaded,
            pins: Set(model.pins),
            header: LibraryHeaderModel.make(model),
            emptyState: model.workspaceEmptyState,
            flags: model.reviewCounts.counts,
            edited: { model.editMeta[$0]?.date },
            onTogglePin: model.togglePin,
            // a workspace you only view takes no new docs (ADR 0004)
            onNewDoc: model.currentAccess.canCreate ? { router.newDoc(in: $0) } : nil,
            onNewFolder: model.currentAccess.canCreate ? { router.newFolder(in: $0) } : nil,
            onOpen: { router.open(.doc($0)) },
            onRowAppear: { id in Task { await model.loadEditMeta(id) } },
            reveal: router.revealDoc,
            onRevealed: { router.revealDoc = nil }
        )
        .refreshable {
            try? await model.sync?.catchUp()
            await model.reviewCounts.refresh(model.api, force: true)
        }
        .task(id: model.docs.count) { await model.reviewCounts.refresh(model.api) }
        .modifier(WorkspaceMoveHost())
        .modifier(DocDeleteHost())
        .modifier(DocMoveHost())
    }
}

/// What the Library's title block says: the workspace (or "Library"), how
/// many docs, who shares it; and what the title's menu offers.
struct LibraryHeaderModel: Equatable {
    var title: String
    /// "38 docs · only you"
    var meta: String
    var picker: WorkspacePicker?
    /// Unsorted with docs in it: "Select" files them
    var triage: Bool

    @MainActor static func make(_ model: AppModel) -> LibraryHeaderModel {
        guard model.hasWorkspaces, let scope = model.currentWorkspace else {
            return LibraryHeaderModel(title: "Library", meta: docs(model.docs.count), picker: nil, triage: false)
        }
        let picker = model.workspacePicker
        return LibraryHeaderModel(title: picker.name(scope), meta: subtitle(scope, picker: picker), picker: picker,
                                  triage: scope == .unsorted && !model.unsortedDocs.isEmpty)
    }

    static func docs(_ n: Int) -> String { n == 1 ? "1 doc" : "\(n) docs" }

    /// "38 docs · only you", "14 docs · shared", "9 docs · shared by Aoife · view only"
    static func subtitle(_ scope: WorkspaceScope, picker: WorkspacePicker) -> String {
        switch scope {
        case .unsorted:
            return "\(docs(picker.unsortedCount)) · not in a workspace"
        case .id(let id):
            guard let w = picker.workspace(id) else { return "" }
            var parts = [docs(w.docCount)]
            if w.shared {
                if !w.isOwn, let owner = w.ownerName, !owner.isEmpty { parts.append("shared by \(owner)") } else { parts.append("shared") }
                if w.isViewOnly { parts.append("view only") }
            } else {
                parts.append("only you")
            }
            return parts.joined(separator: " · ")
        }
    }
}

struct LibraryContent: View {
    let nodes: [LibraryNode]
    let loaded: Bool
    var pins: Set<DocID> = []
    var header = LibraryHeaderModel(title: "Library", meta: "", picker: nil, triage: false)
    /// the current workspace has no docs yet
    var emptyState: WorkspaceEmptyState?
    /// open review items per doc (the row's flag pill)
    var flags: [DocID: Int] = [:]
    /// when a doc was last edited, if known
    var edited: (DocID) -> Date? = { _ in nil }
    var onTogglePin: (DocID) -> Void = { _ in }
    /// nil parent = top level
    var onNewDoc: ((DocID?) -> Void)?
    var onNewFolder: ((DocID?) -> Void)?
    var onOpen: (DocID) -> Void = { _ in }
    /// a row came on screen (its "edited" time loads)
    var onRowAppear: (DocID) -> Void = { _ in }
    /// a doc to bring into view (a folder just made), and done when it is
    var reveal: DocID?
    var onRevealed: () -> Void = {}
    /// folders start closed; the ones you open are remembered across launches
    @AppStorage("library.expanded") private var expandedStore = ""
    @State private var expanded: Set<DocID> = []
    @State private var restored = false
    @State private var scroller = TreeScroller()
    @State private var filter = ""
    @State private var creatingWorkspace = false
    @State private var managingWorkspaces = false
    @State private var triaging = false
    @Environment(AppModel.self) private var model: AppModel?
    @Environment(\.docMover) private var docMover
    @Environment(\.moveToWorkspace) private var moveToWorkspace
    @Environment(\.docDeletion) private var docDeletion

    var body: some View {
        // Not a List: List rows never receive drops, so a doc could be
        // picked up but put nowhere. The tree is drawn row by row.
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                LibraryTitle(header: header, onSelect: { model?.selectWorkspace($0) },
                             onNew: { creatingWorkspace = true }, onManage: { managingWorkspaces = true },
                             onTriage: { triaging = true })
                    // drop a dragged doc here: the top level
                    .docTopLevelDrop(mover: docMover)
                    .padding(.horizontal, 4)
                if !nodes.isEmpty { filterField }
                content
            }
            .padding(.horizontal, 16)
            .padding(.top, 4)
            .padding(.bottom, 24)
        }
        // drags near the top or bottom edge scroll it
        .treeScrolling(scroller)
        // reading width on iPad, centred on the ground
        .frame(maxWidth: Lapis.Layout.readingWidth)
        .frame(maxWidth: .infinity)
        .background(Color.lapis.bg.ignoresSafeArea())
        .tint(Color.lapis.accent)
        .groundBackground()
        .scrollDismissesKeyboard(.immediately)
        .navigationTitle(header.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(removing: .title)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                if let onNewFolder {
                    Button { onNewFolder(nil) } label: { Label("New folder", systemImage: "folder.badge.plus") }
                        .accessibilityLabel("New folder")
                        .tint(Color.lapis.ink)
                }
                if let onNewDoc {
                    Button { onNewDoc(nil) } label: { Label("New doc", systemImage: "square.and.pencil") }
                        .accessibilityLabel("New doc")
                        .tint(Color.lapis.ink)
                }
            }
        }
        .sheet(isPresented: $creatingWorkspace) { NewWorkspaceSheet() }
        .sheet(isPresented: $managingWorkspaces) { ManageWorkspacesSheet() }
        .sheet(isPresented: $triaging) { UnsortedTriageSheet() }
        .onAppear {
            guard !restored else { return }
            restored = true
            expanded = Set(expandedStore.split(separator: ",").map(String.init))
        }
        .onChange(of: expanded) { _, now in
            expandedStore = now.sorted().joined(separator: ",")
        }
        // a folder just made: once the tree has it, the folders above it
        // open and the tree scrolls to it
        .onChange(of: reveal) { revealIfReady() }
        .onChange(of: nodes) { revealIfReady() }
    }

    @ViewBuilder private var content: some View {
        if nodes.isEmpty {
            if let emptyState, loaded {
                EmptyCard(icon: "folder", title: emptyState.title, hint: emptyState.hint)
            } else if loaded {
                EmptyCard(icon: "folder", title: "Your library is empty", hint: "Docs you create on the desktop or through an agent appear here.")
            } else {
                VStack(spacing: 0) {
                    ForEach(0..<6, id: \.self) { _ in
                        SkeletonBlock(height: 18).padding(.horizontal, 16).frame(height: 48)
                    }
                }
                .shimmering()
                .background(Color.lapis.card, in: .rect(cornerRadius: 24, style: .continuous))
                .accessibilityLabel("Loading the library")
            }
        } else if !filter.trimmingCharacters(in: .whitespaces).isEmpty {
            FilteredDocs(matches: LibraryFilter.matches(filter, in: nodes), flags: flags, onOpen: onOpen)
        } else {
            LibraryTree(nodes: nodes, expanded: $expanded, pins: pins, flags: flags, edited: edited, style: .library,
                        onOpen: onOpen, onRowAppear: onRowAppear) { node in
                menu(node)
            }
            .background(Color.lapis.card, in: .rect(cornerRadius: 24, style: .continuous))
            .clipShape(.rect(cornerRadius: 24, style: .continuous))
        }
    }

    /// Narrow the tree to docs whose title has these words.
    private var filterField: some View {
        HStack(spacing: 8) {
            Image(systemName: "line.3.horizontal.decrease").foregroundStyle(Color.lapis.ink3).accessibilityHidden(true)
            TextField("Filter by title", text: $filter)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.done)
                .accessibilityIdentifier("library.filter")
            if !filter.isEmpty {
                Button { filter = "" } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(Color.lapis.ink3)
                        .frame(width: 32, height: 32).contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear filter")
            }
        }
        .font(.subheadline)
        .padding(.horizontal, 12)
        .frame(minHeight: Lapis.Layout.minTarget)
        .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.field, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.field, style: .continuous).strokeBorder(Color.lapis.line2, lineWidth: 1))
    }

    private func revealIfReady() {
        guard let id = reveal, let above = LibraryNode.ancestors(of: id, in: nodes) else { return }
        onRevealed()
        filter = ""
        withAnimation(.snappy(duration: 0.2)) { expanded.formUnion(above) }
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(150))
            withAnimation { scroller.position.scrollTo(id: id, anchor: .center) }
        }
    }

    @ViewBuilder private func menu(_ node: LibraryNode) -> some View {
        Button(pins.contains(node.id) ? "Unpin from Today" : "Pin to Today",
               systemImage: pins.contains(node.id) ? "pin.slash" : "pin") { onTogglePin(node.id) }
        if let onNewDoc {
            Button("New doc here", systemImage: "plus") { onNewDoc(node.id) }
        }
        if let onNewFolder {
            Button("New folder here", systemImage: "folder.badge.plus") { onNewFolder(node.id) }
        }
        if let docMover, docMover.canMove(node.id) {
            Button("Move to\u{2026}", systemImage: "folder") { docMover.ask(node.id) }
        }
        if let moveToWorkspace {
            Button("Move to workspace\u{2026}", systemImage: "square.stack") { moveToWorkspace(node.id) }
        }
        if let docDeletion, docDeletion.canDelete(node.id) {
            Divider()
            Button("Delete\u{2026}", systemImage: "trash", role: .destructive) { docDeletion.ask(node.id) }
        }
    }
}

/// "LIBRARY" over the workspace's serif name; the name opens the
/// workspace menu (each with its doc count and who shares it).
private struct LibraryTitle: View {
    let header: LibraryHeaderModel
    var onSelect: (WorkspaceScope) -> Void = { _ in }
    var onNew: () -> Void = {}
    var onManage: () -> Void = {}
    var onTriage: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Library")
                .lapisFont(.label)
                .textCase(.uppercase)
                .foregroundStyle(Color.lapis.ink3)
                .accessibilityHidden(header.picker == nil)
            HStack(alignment: .center, spacing: 8) {
                if let picker = header.picker, let current = picker.current {
                    Menu {
                        Picker("Workspace", selection: Binding(get: { current }, set: { onSelect($0) })) {
                            ForEach(picker.options, id: \.self) { scope in
                                Button {} label: {
                                    Text(picker.name(scope))
                                    Text(LibraryHeaderModel.subtitle(scope, picker: picker))
                                }
                                .tag(scope)
                                .accessibilityLabel(picker.accessibilityName(scope))
                            }
                        }
                        .pickerStyle(.inline)
                        Divider()
                        Button("New workspace\u{2026}", systemImage: "plus", action: onNew)
                        Button("Manage workspaces", systemImage: "slider.horizontal.3", action: onManage)
                    } label: {
                        HStack(spacing: 8) {
                            Text(header.title)
                                .lapisFont(.screenTitle)
                                .foregroundStyle(Color.lapis.ink)
                                .lineLimit(1)
                            Image(systemName: "chevron.down")
                                .font(.footnote.weight(.bold))
                                .foregroundStyle(Color.lapis.ink2)
                                .frame(width: 28, height: 28)
                                .background(Color.lapis.hover, in: .circle)
                                .padding(.top, 4)
                        }
                        .frame(minHeight: Lapis.Layout.minTarget)
                        .contentShape(.rect)
                    }
                    .accessibilityLabel("Workspace: \(picker.accessibilityName(current))")
                    .accessibilityHint("Switches workspace")
                    .accessibilityAddTraits(.isHeader)
                } else {
                    Text(header.title)
                        .lapisFont(.screenTitle)
                        .foregroundStyle(Color.lapis.ink)
                        .accessibilityAddTraits(.isHeader)
                }
                Spacer(minLength: 8)
                if header.triage {
                    Button("Select", action: onTriage)
                        .buttonStyle(.lapis(.ghost))
                }
            }
            if !header.meta.isEmpty {
                Text(header.meta)
                    .font(.footnote)
                    .foregroundStyle(Color.lapis.ink3)
                    .padding(.top, 2)
            }
        }
    }
}

/// The filter's matches: docs whose title has every word, with where they live.
enum LibraryFilter {
    struct Match: Identifiable, Hashable {
        var node: LibraryNode
        var path: String?
        var id: DocID { node.id }
    }

    static func matches(_ query: String, in nodes: [LibraryNode]) -> [Match] {
        let words = query.lowercased().split(whereSeparator: \.isWhitespace).map(String.init)
        guard !words.isEmpty else { return [] }
        var out: [Match] = []
        func walk(_ ns: [LibraryNode], _ path: [String]) {
            for n in ns {
                let t = n.doc.title.lowercased()
                if words.allSatisfy({ t.contains($0) }) {
                    out.append(Match(node: n, path: path.isEmpty ? nil : path.joined(separator: DocIndex.separator)))
                }
                if let kids = n.children { walk(kids, path + [n.doc.title]) }
            }
        }
        walk(nodes, [])
        return out
    }
}

private struct FilteredDocs: View {
    let matches: [LibraryFilter.Match]
    let flags: [DocID: Int]
    let onOpen: (DocID) -> Void

    var body: some View {
        if matches.isEmpty {
            EmptyCard(icon: "magnifyingglass", title: "No titles match", hint: "Search looks inside docs too.")
        } else {
            VStack(spacing: 0) {
                ForEach(Array(matches.enumerated()), id: \.element.id) { i, m in
                    Button { onOpen(m.id) } label: {
                        HStack(spacing: 12) {
                            DocTreeIcon(isFolder: m.node.isFolder, isOpen: false, size: .library)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(m.node.doc.title).font(.body).foregroundStyle(Color.lapis.ink).lineLimit(1)
                                if let p = m.path { Text(p).font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(1).truncationMode(.head) }
                            }
                            Spacer(minLength: 0)
                            FlagPill(count: flags[m.id] ?? 0)
                        }
                        .padding(.horizontal, 14)
                        .frame(maxWidth: .infinity, minHeight: 52, alignment: .leading)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .overlay(alignment: .top) { if i > 0 { Rectangle().fill(Color.lapis.line2).frame(height: 1).padding(.leading, 48) } }
                    .accessibilityIdentifier("tree.\(m.node.doc.title)")
                }
            }
            .background(Color.lapis.card, in: .rect(cornerRadius: 24, style: .continuous))
        }
    }
}

#if DEBUG
#Preview("Library") {
    NavigationStack {
        LibraryContent(nodes: LibraryNode.build(PreviewData.docs), loaded: true, pins: ["g-road"],
                       header: LibraryHeaderModel(title: "Work", meta: "9 docs · only you", picker: nil, triage: false),
                       flags: ["g-arch": 1])
    }
    .preferredColorScheme(.dark)
}

#Preview("Library, empty, light") {
    NavigationStack {
        LibraryContent(nodes: [], loaded: true)
    }
    .preferredColorScheme(.light)
}
#endif
