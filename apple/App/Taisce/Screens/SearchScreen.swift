import SwiftUI
import TaisceKit

/// Server search, with the cached FTS index as the offline fallback.
struct SearchScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @State private var state: SearchState?
    @State private var tag: String?
    /// workspaces: false = this workspace only
    @State private var everywhere = false
    /// the iPad sidebar has its own field; the detail then shows results only
    var showsField = true

    var body: some View {
        @Bindable var router = router
        SearchContent(
            query: $router.searchQuery,
            state: state,
            tag: $tag,
            showsField: showsField,
            everywhere: model.hasWorkspaces ? $everywhere : nil,
            onOpen: { router.open(.doc($0)) }
        )
        .task(id: SearchKey(query: router.searchQuery, everywhere: everywhere, workspace: model.currentWorkspace)) {
            try? await Task.sleep(for: .milliseconds(250)) // debounce typing
            guard !Task.isCancelled else { return }
            let q = router.searchQuery
            guard !q.trimmingCharacters(in: .whitespaces).isEmpty else {
                state = nil
                return
            }
            let found = await model.search(q, everywhere: everywhere)
            guard !Task.isCancelled else { return }
            state = found
            if tag.map({ !found.tags.contains($0) }) ?? false { tag = nil }
            let filled = await model.fillTags(found)
            if !Task.isCancelled, filled != found { state = filled }
        }
    }
}

private struct SearchKey: Hashable {
    var query: String
    var everywhere: Bool
    var workspace: WorkspaceScope?
}

struct SearchContent: View {
    @Binding var query: String
    /// nil = no query yet
    let state: SearchState?
    @Binding var tag: String?
    var showsField = true
    var autofocus = true
    /// workspaces: the "This workspace · Everywhere" chip (nil = no workspaces)
    var everywhere: Binding<Bool>?
    var onOpen: (DocID) -> Void = { _ in }
    @FocusState private var focused: Bool

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if showsField { field }
                if let state, !state.results.isEmpty {
                    VStack(alignment: .leading, spacing: 2) {
                        chips(state)
                        Text(state.offline ? "Offline · searched the docs on this device" : "Matches by word · searched on the server")
                            .font(.caption2)
                            .foregroundStyle(state.offline ? Theme.amber : Theme.secondary)
                    }
                    LazyVStack(spacing: 10) {
                        ForEach(state.filtered(by: tag)) { r in
                            Button { onOpen(r.docID) } label: { SearchResultCard(result: r, query: query) }
                                .buttonStyle(.plain)
                        }
                    }
                } else if let state {
                    if everywhere != nil { chips(state) }
                    EmptyCard(icon: "magnifyingglass", title: "No matches for \u{201C}\(query)\u{201D}",
                              hint: state.offline ? "You're offline: only docs on this device were searched." : "Try fewer words, or a word from the title.")
                } else {
                    EmptyCard(icon: "text.magnifyingglass", title: "Search your library", hint: "Titles and text across every doc. Works offline over the docs on this device.")
                }
            }
            .padding(.horizontal, Theme.gutter)
            .padding(.top, 8)
            .padding(.bottom, 32)
            .frame(maxWidth: Theme.readingWidth)
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
        .groundBackground()
        .toolbarVisibility(.hidden, for: .navigationBar)
        .onAppear { if showsField && autofocus { focused = true } }
    }

    private var field: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(Theme.secondary).accessibilityHidden(true)
                TextField("Search your library", text: $query)
                    .focused($focused)
                    .submitLabel(.search)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .foregroundStyle(Theme.text)
                if !query.isEmpty {
                    Button { query = "" } label: {
                        Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.secondary)
                            .frame(width: Theme.minTarget, height: Theme.minTarget)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Clear search")
                }
            }
            .font(.subheadline)
            .padding(.leading, 12)
            .frame(minHeight: Theme.minTarget)
            .background(Theme.surface2, in: .rect(cornerRadius: 12, style: .continuous))
        }
    }

    private func chips(_ state: SearchState) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                if let everywhere {
                    FilterChip(title: "This workspace", selected: !everywhere.wrappedValue) { everywhere.wrappedValue = false }
                    FilterChip(title: "Everywhere", selected: everywhere.wrappedValue) { everywhere.wrappedValue = true }
                    Divider().frame(height: 18)
                }
                FilterChip(title: "All", selected: tag == nil) { tag = nil }
                ForEach(state.tags, id: \.self) { t in
                    FilterChip(title: "#\(t)", selected: tag == t) { tag = tag == t ? nil : t }
                }
            }
        }
        .scrollClipDisabled()
    }
}

struct SearchResultCard: View {
    let result: SearchResult
    let query: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let crumb = result.breadcrumb {
                Text(crumb).font(.caption2).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            Text(result.title).font(Theme.serif(.headline)).foregroundStyle(Theme.text)
            Text(highlighted).font(.footnote).foregroundStyle(Theme.secondary).lineLimit(3)
            if !result.tags.isEmpty {
                HStack(spacing: 6) { ForEach(result.tags.prefix(3), id: \.self) { TagChip(tag: $0) } }.padding(.top, 2)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
        .hoverWash()
        .contentShape(.rect(cornerRadius: Theme.radius))
        .accessibilityElement(children: .combine)
    }

    var highlighted: AttributedString {
        var s = AttributedString(result.snippet)
        for r in SearchState.matches(in: result.snippet, query: query) {
            guard let lo = AttributedString.Index(r.lowerBound, within: s), let hi = AttributedString.Index(r.upperBound, within: s) else { continue }
            s[lo..<hi].backgroundColor = Theme.accent.opacity(0.30)
            s[lo..<hi].foregroundColor = Theme.text
        }
        return s
    }
}

#if DEBUG
#Preview("Search") {
    @Previewable @State var query = "sync"
    @Previewable @State var tag: String?
    NavigationStack { SearchContent(query: $query, state: PreviewData.search, tag: $tag, autofocus: false) }
        .preferredColorScheme(.dark)
}

#Preview("Search, filtered, light") {
    @Previewable @State var query = "sync"
    @Previewable @State var tag: String? = "ios"
    NavigationStack { SearchContent(query: $query, state: PreviewData.search, tag: $tag, autofocus: false) }
        .preferredColorScheme(.light)
}

#Preview("Search, no matches") {
    @Previewable @State var query = "zebra"
    @Previewable @State var tag: String?
    NavigationStack { SearchContent(query: $query, state: SearchState(), tag: $tag, autofocus: false) }
        .preferredColorScheme(.dark)
}
#endif
