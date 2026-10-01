import TaisceKit
import SwiftUI

struct SidebarView: View {
    @Environment(AppModel.self) private var model
    @Binding var selection: SidebarItem?
    let query: String

    var body: some View {
        List(selection: $selection) {
            if query.isEmpty {
                Section {
                    Label("Today", systemImage: "checklist").tag(SidebarItem.today)
                    Label("Pinned", systemImage: "pin").tag(SidebarItem.pinned)
                }
                Section("Docs") {
                    OutlineGroup(model.tree, children: \.children) { node in
                        Label(node.doc.title, systemImage: node.children == nil ? "doc.text" : "folder")
                            .tag(SidebarItem.doc(node.id))
                    }
                }
            } else {
                SearchResults(query: query)
            }
        }
    }
}

/// Server search, falling back to the offline FTS index when unreachable.
private struct SearchResults: View {
    @Environment(AppModel.self) private var model
    let query: String
    @State private var hits: [Hit] = []
    @State private var offline = false

    struct Hit: Identifiable, Hashable {
        var id: BlockID
        var docID: DocID
        var title: String
        var snippet: String
    }

    var body: some View {
        Section(offline ? "Results (offline)" : "Results") {
            ForEach(hits) { hit in
                VStack(alignment: .leading, spacing: 2) {
                    Text(hit.title).font(.headline)
                    Text(hit.snippet).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
                .tag(SidebarItem.doc(hit.docID))
            }
        }
        .task(id: query) {
            try? await Task.sleep(for: .milliseconds(250)) // debounce typing
            guard !Task.isCancelled else { return }
            await run()
        }
    }

    private func run() async {
        if let api = model.api, let found = try? await api.search(query) {
            offline = false
            hits = found.map { Hit(id: $0.block.id, docID: $0.block.docID, title: $0.docTitle, snippet: $0.block.content) }
        } else if let cache = model.cache, let found = try? await cache.searchBlocks(query) {
            offline = true
            let titles = Dictionary(model.docs.map { ($0.id, $0.title) }, uniquingKeysWith: { a, _ in a })
            hits = found.map { Hit(id: $0.id, docID: $0.docID, title: titles[$0.docID] ?? "", snippet: $0.content) }
        }
    }
}
