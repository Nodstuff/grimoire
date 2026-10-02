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
                onNewDoc: { router.newDoc(in: $0) }
            )
            .refreshable { try? await model.sync?.catchUp() }
            .safeAreaInset(edge: .top, spacing: 0) { WorkspaceBar(triage: true) }
            .modifier(WorkspaceMoveHost())
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
    /// folders start closed; the ones you open are remembered across launches
    @AppStorage("library.expanded") private var expandedStore = ""
    @State private var expanded: Set<DocID>?

    var body: some View {
        List {
            Section {
                if nodes.isEmpty {
                    Group {
                        if loaded {
                            EmptyCard(icon: "folder", title: "Your library is empty", hint: "Docs you create on the desktop or through an agent appear here.")
                        } else {
                            HStack { Spacer(); ProgressView(); Spacer() }.frame(minHeight: 120)
                        }
                    }
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets())
                } else {
                    LibraryTreeRows(nodes: nodes, pins: pins, expanded: $expanded, onTogglePin: onTogglePin, onNewDoc: onNewDoc)
                }
            } header: {
                ScreenHeader(title: "Library") {
                    if let onNewDoc {
                        CircleIconButton(systemImage: "plus", label: "New doc", filled: true) { onNewDoc(nil) }
                    }
                }
                    .textCase(nil)
                    .padding(.bottom, 8)
                    .listRowInsets(EdgeInsets(top: 0, leading: 4, bottom: 0, trailing: 4))
            }
        }
        .listStyle(.insetGrouped)
        // reading width on iPad, centred on the ground
        .frame(maxWidth: Theme.readingWidth)
        .frame(maxWidth: .infinity)
        .background(Theme.ground.ignoresSafeArea())
        .tint(Theme.accent)
        .groundBackground()
        .toolbarVisibility(.hidden, for: .navigationBar)
        .onAppear {
            if expanded == nil { expanded = Set(expandedStore.split(separator: ",").map(String.init)) }
        }
        .onChange(of: expanded) { _, now in
            expandedStore = (now ?? []).sorted().joined(separator: ",")
        }
    }
}

/// The tree as rows: folders disclose their children, every row opens its doc.
struct LibraryTreeRows: View {
    let nodes: [LibraryNode]
    let pins: Set<DocID>
    @Binding var expanded: Set<DocID>?
    let onTogglePin: (DocID) -> Void
    var onNewDoc: ((DocID?) -> Void)?
    @Environment(\.moveToWorkspace) private var moveToWorkspace

    var body: some View {
        ForEach(nodes) { node in
            if let kids = node.children {
                DisclosureGroup(isExpanded: binding(node.id)) {
                    LibraryTreeRows(nodes: kids, pins: pins, expanded: $expanded, onTogglePin: onTogglePin, onNewDoc: onNewDoc)
                } label: {
                    link(node)
                }
                .listRowBackground(Theme.surface)
            } else {
                link(node).listRowBackground(Theme.surface)
            }
        }
    }

    func binding(_ id: DocID) -> Binding<Bool> {
        Binding(
            get: { expanded?.contains(id) ?? false },
            set: { open in
                var set = expanded ?? []
                if open { set.insert(id) } else { set.remove(id) }
                expanded = set
            }
        )
    }

    func link(_ node: LibraryNode) -> some View {
        NavigationLink(value: Route.doc(node.id)) {
            LibraryRow(node: node, pinned: pins.contains(node.id))
        }
        .contextMenu {
            Button(pins.contains(node.id) ? "Unpin from Today" : "Pin to Today",
                   systemImage: pins.contains(node.id) ? "pin.slash" : "pin") { onTogglePin(node.id) }
            if let onNewDoc {
                Button("New doc here", systemImage: "plus") { onNewDoc(node.id) }
            }
            if let moveToWorkspace {
                Button("Move to workspace\u{2026}", systemImage: "square.stack") { moveToWorkspace(node.id) }
            }
        }
    }
}

struct LibraryRow: View {
    let node: LibraryNode
    var pinned = false

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: node.isFolder ? "folder" : "doc.text")
                .font(.subheadline)
                .foregroundStyle(node.isFolder ? Theme.accent : Theme.secondary)
                .frame(width: 24)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(node.doc.title).font(.subheadline).foregroundStyle(Theme.text).lineLimit(2)
                if let sub = node.subtitle {
                    Text(sub).font(.caption2).foregroundStyle(Theme.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
            if pinned {
                Image(systemName: "pin.fill").font(.caption).foregroundStyle(Theme.accent).accessibilityLabel("Pinned")
            }
        }
        .frame(minHeight: Theme.minTarget)
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
