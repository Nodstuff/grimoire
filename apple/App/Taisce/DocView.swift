import TaisceKit
import SwiftUI

/// Read-only doc: renders the cached blocks and refreshes them from the
/// server when missing or stale. Re-renders whenever sync rewrites them.
struct DocView: View {
    @Environment(AppModel.self) private var model
    let docID: DocID
    @Binding var selection: SidebarItem?
    @State private var blocks: [RenderedBlock] = []
    @State private var loadError: String?

    var title: String { model.docs.first { $0.id == docID }?.title ?? "" }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                ForEach(blocks) { block in
                    ForEach(Array(block.nodes.enumerated()), id: \.offset) { _, node in
                        RenderNodeView(node: node)
                    }
                }
                if let loadError, blocks.isEmpty {
                    ContentUnavailableView("Couldn't load", systemImage: "wifi.slash", description: Text(loadError))
                }
            }
            .padding()
            .frame(maxWidth: 760, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .navigationTitle(title)
        .environment(\.openURL, OpenURLAction { url in
            if let target = InlineMarkdown.wikiTarget(url) {
                if let doc = model.doc(titled: target) { selection = .doc(doc.id) }
                return .handled
            }
            return .systemAction
        })
        .task(id: docID) { await observe() }
        .task(id: docID) { await refreshIfStale() }
    }

    private func observe() async {
        guard let cache = model.cache else { return }
        do {
            for try await records in cache.observeBlocks(of: docID) {
                blocks = BlockRenderer.render(records)
            }
        } catch {
            loadError = error.localizedDescription
        }
    }

    private func refreshIfStale() async {
        guard let cache = model.cache, let sync = model.sync else { return }
        let record = try? await cache.doc(docID)
        guard record?.isBodyStale ?? true else { return }
        do {
            try await sync.refresh(docID)
            loadError = nil
        } catch {
            loadError = error.localizedDescription
        }
    }
}
