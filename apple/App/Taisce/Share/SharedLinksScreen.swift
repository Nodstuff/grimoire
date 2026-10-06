import SwiftUI
import TaisceKit

/// Settings › Shared links: every link you've made, live ones first.
struct SharedLinksScreen: View {
    @Environment(AppModel.self) private var model
    @State private var error: String?
    @State private var loading = true

    var body: some View {
        let store = model.shareLinks
        let live = store.all.filter { $0.state() == .active }
        let ended = store.all.filter { $0.state() != .active }
        Form {
            if loading && store.all.isEmpty {
                Section { ProgressView().frame(maxWidth: .infinity) }.listRowBackground(Theme.surface)
            } else if store.all.isEmpty {
                Section {
                    Text("No shared links yet. Open a doc and choose Share link\u{2026} in its menu.")
                        .font(.footnote)
                        .foregroundStyle(Theme.secondary)
                }
                .listRowBackground(Theme.surface)
            }
            if !live.isEmpty {
                Section("Live") { ForEach(live) { row($0) } }.listRowBackground(Theme.surface)
            }
            if !ended.isEmpty {
                Section("Expired or revoked") { ForEach(ended) { row($0) } }.listRowBackground(Theme.surface)
            }
            if let error {
                Section { Text(error).font(.footnote).foregroundStyle(Theme.rose) }.listRowBackground(Theme.surface)
            }
        }
        .groundBackground()
        .navigationTitle("Shared links")
        .navigationBarTitleDisplayMode(.inline)
        .task { await reload() }
        .refreshable { await reload() }
    }

    private func row(_ share: Share) -> some View {
        NavigationLink {
            ShareLinkForm(docID: share.docID, only: share.id)
                .navigationTitle(title(share))
                .navigationBarTitleDisplayMode(.inline)
        } label: {
            SharedLinkRow(share: share, title: title(share))
        }
        .accessibilityIdentifier("sharedLinks.row")
    }

    private func title(_ share: Share) -> String {
        model.index.byID[share.docID]?.title ?? (share.title.isEmpty ? "Untitled" : share.title)
    }

    private func reload() async {
        do {
            try await model.shareLinks.loadAll()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        loading = false
    }
}

struct SharedLinkRow: View {
    let share: Share
    let title: String
    var now: Date = .now

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 3) {
                Text(title).foregroundStyle(Theme.text).lineLimit(1)
                Text("\(ShareLinkText.state(share, now: now)) · \(ShareLinkText.views(share, now: now))")
                    .font(.caption)
                    .foregroundStyle(share.state(now: now) == .active ? Theme.secondary : Theme.amber)
                if let c = ShareLinkText.comments(share) {
                    Text(c).font(.caption).foregroundStyle(Theme.secondary)
                }
            }
            Spacer()
            if share.unreadComments > 0 { UnreadBadge(count: share.unreadComments) }
        }
        .accessibilityElement(children: .combine)
    }
}
