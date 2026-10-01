import SwiftUI
import TaisceKit

/// The doc tree, with disclosure. Pull to refresh catches up with the server.
struct LibraryScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        LibraryContent(
            nodes: model.library,
            loaded: model.treeLoaded,
            pins: Set(model.pins),
            onTogglePin: model.togglePin
        )
        .refreshable { try? await model.sync?.catchUp() }
    }
}

struct LibraryContent: View {
    let nodes: [LibraryNode]
    let loaded: Bool
    var pins: Set<DocID> = []
    var onTogglePin: (DocID) -> Void = { _ in }

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
                    OutlineGroup(nodes, children: \.children) { node in
                        NavigationLink(value: Route.doc(node.id)) {
                            LibraryRow(node: node, pinned: pins.contains(node.id))
                        }
                        .contextMenu {
                            Button(pins.contains(node.id) ? "Unpin from Today" : "Pin to Today",
                                   systemImage: pins.contains(node.id) ? "pin.slash" : "pin") { onTogglePin(node.id) }
                        }
                        .listRowBackground(Theme.surface)
                    }
                }
            } header: {
                ScreenHeader(title: "Library")
                    .textCase(nil)
                    .padding(.bottom, 8)
                    .listRowInsets(EdgeInsets(top: 0, leading: 4, bottom: 0, trailing: 4))
            }
        }
        .listStyle(.insetGrouped)
        .tint(Theme.accent)
        .groundBackground()
        .toolbarVisibility(.hidden, for: .navigationBar)
    }
}

struct LibraryRow: View {
    let node: LibraryNode
    var pinned = false

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: node.isFolder ? "folder" : "doc.text")
                .foregroundStyle(node.isFolder ? Theme.accent : Theme.secondary)
                .frame(width: 24)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(node.doc.title).font(.body).foregroundStyle(Theme.text).lineLimit(2)
                if let sub = node.subtitle {
                    Text(sub).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
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
