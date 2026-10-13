import SwiftUI
import TaisceKit

/// The doc tree, with disclosure. Pull to refresh catches up with the server.
struct LibraryScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router

    var body: some View {
        // workspaces: an empty workspace says so; the bar switches and triages
        if let empty = model.workspaceEmptyState, model.treeLoaded, model.library.isEmpty {
            WorkspaceEmptyView(state: empty).refreshable { try? await model.sync?.catchUp() }
        } else {
            LibraryContent(
                nodes: model.library,
                loaded: model.treeLoaded,
                pins: Set(model.pins),
                onTogglePin: model.togglePin,
                // a workspace you only view takes no new docs (ADR 0004)
                onNewDoc: model.currentAccess.canCreate ? { router.newDoc(in: $0) } : nil,
                onOpen: { router.open(.doc($0)) }
            )
            .refreshable { try? await model.sync?.catchUp() }
            .safeAreaInset(edge: .top, spacing: 0) { WorkspaceBar(triage: true) }
            .modifier(WorkspaceMoveHost())
            .modifier(DocDeleteHost())
            .modifier(DocMoveHost())
        }
    }
}

struct LibraryContent: View {
    let nodes: [LibraryNode]
    let loaded: Bool
    var pins: Set<DocID> = []
    var onTogglePin: (DocID) -> Void = { _ in }
    /// nil parent = top level
    var onNewDoc: ((DocID?) -> Void)?
    var onOpen: (DocID) -> Void = { _ in }
    /// folders start closed; the ones you open are remembered across launches
    @AppStorage("library.expanded") private var expandedStore = ""
    @State private var expanded: Set<DocID> = []
    @State private var restored = false
    @State private var scroller = TreeScroller()
    @Environment(\.docMover) private var docMover
    @Environment(\.moveToWorkspace) private var moveToWorkspace
    @Environment(\.docDeletion) private var docDeletion

    var body: some View {
        // Not a List: List rows never receive drops, so a doc could be
        // picked up but put nowhere. The tree is drawn row by row.
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                ScreenHeader(title: "Library") {
                    if let onNewDoc {
                        CircleIconButton(systemImage: "plus", label: "New doc", filled: true) { onNewDoc(nil) }
                    }
                }
                // drop a dragged doc here: the top level
                .docTopLevelDrop(mover: docMover)
                .padding(.horizontal, 4)
                if nodes.isEmpty {
                    if loaded {
                        EmptyCard(icon: "folder", title: "Your library is empty", hint: "Docs you create on the desktop or through an agent appear here.")
                    } else {
                        HStack { Spacer(); ProgressView(); Spacer() }.frame(minHeight: 120)
                    }
                } else {
                    LibraryTree(nodes: nodes, expanded: $expanded, pins: pins, style: .library, onOpen: onOpen) { node in
                        menu(node)
                    }
                    .padding(6)
                    .background(Theme.surface, in: .rect(cornerRadius: 12, style: .continuous))
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        // drags near the top or bottom edge scroll it
        .treeScrolling(scroller)
        // reading width on iPad, centred on the ground
        .frame(maxWidth: Theme.readingWidth)
        .frame(maxWidth: .infinity)
        .background(Theme.ground.ignoresSafeArea())
        .tint(Theme.accent)
        .groundBackground()
        .toolbarVisibility(.hidden, for: .navigationBar)
        .onAppear {
            guard !restored else { return }
            restored = true
            expanded = Set(expandedStore.split(separator: ",").map(String.init))
        }
        .onChange(of: expanded) { _, now in
            expandedStore = now.sorted().joined(separator: ",")
        }
    }

    @ViewBuilder private func menu(_ node: LibraryNode) -> some View {
        Button(pins.contains(node.id) ? "Unpin from Today" : "Pin to Today",
               systemImage: pins.contains(node.id) ? "pin.slash" : "pin") { onTogglePin(node.id) }
        if let onNewDoc {
            Button("New doc here", systemImage: "plus") { onNewDoc(node.id) }
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

#if DEBUG
#Preview("Library") {
    NavigationStack {
        LibraryContent(nodes: LibraryNode.build(PreviewData.docs), loaded: true, pins: ["g-road"])
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
